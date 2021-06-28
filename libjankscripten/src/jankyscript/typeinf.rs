//! Type inference for JankyScript, using the TypeWhich approach.
//!
//! See the TypeWhich paper for details for a high-level overview:
//!
//! https://khoury.northeastern.edu/~arjunguha/main/papers/2021-typewhich.html
//!
//! This module hews closely to the TypeWhich artifact:
//!
//! https://github.com/arjunguha/TypeWhich
//!

use crate::{typ, z3f};
use super::super::shared::coercions::Coercion;
use super::operators::OVERLOADS;
use super::syntax::*;
use super::typeinf_env::Env;
use super::typeinf_z3::{Z3Typ, Z3TypList};
use super::walk::{Loc, Visitor};
use crate::pos::Pos;
use z3::ast::{self, Ast, Dynamic};
use z3::{Model, Optimize, SatResult};

struct Typeinf<'a> {
    vars: Vec<Dynamic<'a>>,
    z: Z3Typ<'a>,
    zl: Z3TypList<'a>,
    solver: Optimize<'a>,
    cxt: &'a z3::Context,
    env: Env,
    return_type: Type,
}

/// Calculates the type of a literal.
fn typ_lit(lit: &Lit) -> Type {
    match lit {
        Lit::Num(Num::Float(_)) => Type::Float,
        Lit::Num(Num::Int(_)) => Type::Int,
        Lit::String(_) => Type::String,
        Lit::Bool(_) => Type::Bool,
        Lit::Regex(..) => Type::Any,
        Lit::Undefined => Type::Any,
        Lit::Null => Type::Any,
    }
}

fn coerce(src: Type, dst: Type, e: Expr, p: Pos) -> Expr {
    match (e, &src) {
        (Expr::Coercion(Coercion::Meta(src1, Type::Any), e1, p1), Type::Any) => {
            super::constructors::coercion_(Coercion::meta(src1, dst), *e1, p1)
        }
        (e, _) => {
            super::constructors::coercion_(Coercion::meta(src, dst), e, p)
        }
    }
}

struct SubtMetavarVisitor<'a> {
    vars: &'a Vec<Type>,
}

impl<'a> Visitor for SubtMetavarVisitor<'a> {
    fn enter_typ(&mut self, t: &mut Type, _loc: &Loc) {
        match t {
            Type::Metavar(n) => {
                *t = self
                    .vars
                    .get(*n)
                    .expect("unbound type metavariable")
                    .clone();
            }
            _ => (),
        }
    }

    fn exit_expr(&mut self, expr: &mut Expr, _loc: &Loc) {
        match expr {
            Expr::Coercion(c, e, _) => {
                if let Coercion::Meta(t1, t2) = c {
                    if t1 == t2 {
                        *expr = e.take();
                    }
                }
            }
            Expr::JsOp(op, arg_es, arg_ts, p) => {
                let p = std::mem::replace(p, Default::default());
                let mut es = std::mem::replace(arg_es, Default::default());
                match OVERLOADS.target(op, arg_ts.as_slice()) {
                    Some(lower_op) => {
                        *expr = lower_op.make_app(es, p);
                    }
                    None => {
                        let (ty, lower_op) = OVERLOADS.any_target(op);
                        let (conv_arg_tys, _) = ty.unwrap_fun();
                        // TODO(arjun): This is going to end up duplicating a lot of
                        // code that appears above.
                        for (e, t) in es.iter_mut().zip(conv_arg_tys) {
                            *e = coerce(Type::Any, t.clone(), e.take(), p.clone());
                        }
                        *expr = lower_op.make_app(es, p);
                    }
                }
            }
            _ => {}
        }
    }
}

impl<'a> Typeinf<'a> {
    fn t(&self, t: &Type) -> z3::ast::Dynamic<'a> {
        match t {
            Type::Int => self.z.make_int(),
            Type::Any => self.z.make_any(),
            Type::String => self.z.make_str(),
            Type::Bool => self.z.make_bool(),
            Type::Array => self.z.make_array(),
            Type::DynObject => self.z.make_dynobject(),
            Type::Metavar(n) => self
                .vars
                .get(*n)
                .expect("unbound type metavariable")
                .clone(),
            Type::Function(args, r) => {
                let mut z_args = self.zl.make_tnil();
                for a in args.iter().rev() {
                    z_args = self.zl.make_tcons(&self.t(a), &z_args);
                }
                self.z.make_fun(&z_args, &self.t(r))
            }
            _ => todo!("Type: {:?}", t),
        }
    }

    fn z3_to_typ_vec(&self, model: &'a Model, mut e: Dynamic<'a>) -> Vec<Type> {
        let mut r = Vec::<Type>::new();
        while !self.zl.is_tnil(&model, &e) {
            let hd = model.eval(&self.zl.tcons_thd(&e)).expect("no head model");
            r.push(self.z3_to_typ(model, hd));
            e = model.eval(&self.zl.tcons_ttl(&e)).unwrap();
        }
        return r;
    }

    fn z3_to_typ(&self, model: &'a Model, e: Dynamic) -> Type {
        if self.z.is_int(model, &e) {
            Type::Int
        } else if self.z.is_any(model, &e) {
            Type::Any
        } else if self.z.is_str(model, &e) {
            Type::String
        } else if self.z.is_array(model, &e) {
            Type::Array
        } else if self.z.is_dynobject(&model, &e) {
            Type::DynObject
        } else if self.z.is_fun(&model, &e) {
            let args = model.eval(&self.z.fun_args(&e)).expect("model for fun_args");
            let ret = model.eval(&self.z.fun_ret(&e)).expect("model for fun_args");
            Type::Function(self.z3_to_typ_vec(&model, args), Box::new(self.z3_to_typ(&model, ret)))
        }
        else {
            todo!()
        }
    }

    fn fresh_weight(&self) -> z3::ast::Bool<'a> {
        let e = z3::ast::Bool::fresh_const(self.z.cxt, "w");
        self.solver.assert_soft(&e, 1, None);
        return e;
    }

    fn fresh_metavar(&mut self, prefix: &'static str) -> Type {
        let x = self.z.fresh(prefix);
        let n = self.vars.len();
        self.vars.push(x.clone());
        return Type::Metavar(n);
    }

    pub fn cgen_stmt(&mut self, stmt: &mut Stmt) {
        match stmt {
            Stmt::Var(x, t, e, _) => {
                let alpha = self.fresh_metavar("x");
                *t = alpha.clone();
                self.env.update(x.clone(), alpha.clone());
                // TODO(arjun): This is a little hacky, but necessary to deal with function results
                // getting named.
                if !e.is_undefined() {
                    let (phi, t) = self.cgen_expr(e);
                    self.solver.assert(&phi);
                    self.solver.assert(&z3f!(self,
                         (= (tid t) (tid alpha))));
                }
            }
            Stmt::Expr(e, _) => {
                let (phi, _) = self.cgen_expr(&mut *e);
                self.solver.assert(&phi);
            }
            Stmt::Empty => (),
            Stmt::Loop(s, _) => self.cgen_stmt(s),
            Stmt::Label(_, s, _) => self.cgen_stmt(s),
            Stmt::Block(stmts, _) => {
                for s in stmts.iter_mut() {
                    self.cgen_stmt(s);
                }
            }
            Stmt::Catch(body, exn_name, catch_body, _) => {
                self.cgen_stmt(&mut *body);
                let env = self.env.clone();
                self.env.extend(exn_name.clone(), Type::Any);
                self.cgen_stmt(catch_body);
                self.env = env;
            }
            Stmt::Return(e, p) => {
                let (phi, t) = self.cgen_expr(e);
                let w = self.fresh_weight();
                self.solver.assert(&phi);
                let t_r = self.return_type.clone();
                self.solver.assert(&z3f!(self,
                    (or (and (id w.clone()) (= (tid t_r.clone()) (tid t.clone())))
                    // TODO(arjun): And t_r must be ground
                        (and (not (id w)) (= (tid t_r.clone()) (typ any))))
                ));
                **e = coerce(t, t_r, e.take(), p.clone());
            }
            _ => todo!("{:?}", stmt),
        }
    }

    fn cgen_exprs<'b>(
        &mut self,
        exprs: impl Iterator<Item = &'b mut Expr>,
    ) -> (Vec<ast::Bool<'a>>, Vec<Type>) {
        let mut phis = Vec::new();
        let mut ts = Vec::new();
        for e in exprs {
            let (phi, t) = self.cgen_expr(e);
            phis.push(phi);
            ts.push(t);
        }
        (phis, ts)
    }

    fn zand(&self, phis: Vec<ast::Bool<'a>>) -> ast::Bool<'a> {
        let phis = phis.iter().collect::<Vec<_>>();
        ast::Bool::and(self.z.cxt, phis.as_slice())
    }


    pub fn cgen_expr(&mut self, expr: &mut Expr) -> (ast::Bool<'a>, Type) {
        match expr {
            Expr::Binary(..)
            | Expr::PrimCall(..)
            | Expr::NewRef(..)
            | Expr::Deref(..)
            | Expr::Store(..)
            | Expr::EnvGet(..)
            | Expr::Coercion(..)
            | Expr::Closure(..)
            | Expr::Unary(..) => panic!("unexpected {:?}", &expr),
            Expr::Lit(l, p) => {
                let t = typ_lit(&l);
                let p = p.clone();
                let alpha_t = self.fresh_metavar("alpha");
                let w = self.fresh_weight();
                let phi =  z3f!(self,
                    (or (and (unquote w.clone()) (= (tid alpha_t.clone()) (tid t.clone())))
                        (and (not (unquote w)) (= (tid alpha_t.clone()) (typ any)))));
                let e = expr.take();
                *expr = coerce(t, alpha_t.clone(), e, p);
                (phi, alpha_t)
            }
            Expr::Array(es, _) => {
                let (mut phis, ts) = self.cgen_exprs(es.iter_mut());
                for t in ts {
                    phis.push(self.t(&t)._eq(&self.z.make_any()));
                }
                (self.zand(phis), Type::Array)
            }
            Expr::Object(props, _) => {
                let (mut phis, ts) = self.cgen_exprs(props.iter_mut().map(|(_, e)| e));
                for t in ts {
                    phis.push(self.t(&t)._eq(&self.z.make_any()));
                }
                (self.zand(phis), Type::DynObject)
            }
            Expr::Id(x, t, _) => {
                *t = self.env.get(x);
                (ast::Bool::from_bool(self.cxt, true), t.clone())
            }
            Expr::Dot(e, _x, p) => {
                let p = p.clone();
                let w = self.fresh_weight();
                let (phi_1, t) = self.cgen_expr(e);
                let phi_2 = (self.t(&t)._eq(&self.z.make_dynobject()) & &w) | 
                    (self.t(&t)._eq(&self.z.make_any()) & !w) ;
                let e = expr.take();
                *expr = coerce(t, Type::DynObject, e, p);
                (phi_1 & phi_2, Type::Any)
            }
            Expr::Bracket(..) => todo!(),
            Expr::JsOp(op, args, empty_args_t, _) => {
                let w = self.fresh_weight();
                // all overloads for op
                let sigs = OVERLOADS.overloads(op);
                // Fresh type metavariable for the result of this expression
                let alpha_t = self.fresh_metavar("alpha");
                // Recur into each argument and unzip Z3 constants and our type metavars
                let args_rec = args.iter_mut().map(|e| self.cgen_expr(e));
                let (mut args_phi, args_t): (Vec<_>, Vec<_>) = args_rec.unzip();
                // Fresh type metavariables for each argument
                let mut betas_t = Vec::new();
                for (arg, arg_t) in args.iter_mut().zip(&args_t) {
                    match arg_t {
                        Type::Metavar(_) => {
                            betas_t.push(arg_t.clone());
                        }
                        _ => {
                            let a = arg.take();
                            let beta_t = self.fresh_metavar("beta");
                            *arg = coerce(arg_t.clone(), beta_t.clone(), a, Default::default());
                            betas_t.push(beta_t);
                        }
                    }
                }
                // In DNF, one disjunct for each overload
                let mut disjuncts = Vec::new();
                for (op_arg_t, op_ret_t) in sigs.map(|t| t.unwrap_fun()) {
                    // For this overload, arguments and result must match
                    let mut conjuncts = vec![w.clone()];
                    for ((t1, t2), t3) in args_t.iter().zip(op_arg_t).zip(betas_t.iter()) {
                        conjuncts.push(self.t(t1)._eq(&self.t(t2)));
                        conjuncts.push(self.t(t2)._eq(&self.t(t3)));
                    }
                    conjuncts.push(self.t(&alpha_t)._eq(&self.t(op_ret_t)));
                    disjuncts.push(self.zand(conjuncts));
                }

                if let Some(any_ty) = OVERLOADS.on_any(op) {
                    let (_, result_typ) = any_ty.unwrap_fun();
                    let mut conjuncts = vec![!w];
                    for (_t1, t2) in args_t.iter().zip(betas_t.iter()) {
                        // TODO(arjun): t1 must be compatible with any
                        conjuncts.push(self.t(t2)._eq(&self.z.make_any()));
                    }
                    conjuncts.push(self.t(&alpha_t)._eq(&self.t(result_typ)));
                    disjuncts.push(self.zand(conjuncts))
                }
                let cases =
                    ast::Bool::or(self.z.cxt, disjuncts.iter().collect::<Vec<_>>().as_slice());
                args_phi.push(cases);
                // Annotate the AST with the type metavariables that hold the argument types
                *empty_args_t = betas_t;
                (self.zand(args_phi), alpha_t)
            }
            Expr::Assign(lval, e, _) => match &mut **lval {
                LValue::Id(x, x_t) => {
                    let t = self.env.get(x);
                    *x_t = t.clone();
                    let (phi_1, e_t) = self.cgen_expr(&mut *e);
                    // TODO(arjun): Not quite right. Too strict
                    let phi_2 = self.t(&t)._eq(&self.t(&e_t));
                    (phi_1 & phi_2, t)
                }
                _ => todo!(),
            },
            Expr::Call(f, args, p) => {
                let w_1 = self.fresh_weight();
                let w_2 = self.fresh_weight();
                let (phi_1, t_f)= self.cgen_expr(f);
                let (args_phi, args_t) = self.cgen_exprs(args.iter_mut());
                let phi_2 = self.zand(args_phi);
                let beta = self.fresh_metavar("beta");
                let gamma = self.fresh_metavar("gamma");

                let phi_31 = self.zand(args_t.iter().map(|x| z3f!(self, (= (tid x) (typ any)))).collect());
                let phi_3 = z3f!(self, 
                    (or
                        (and (= (tid t_f) (typ fun_vec(args_t.clone()) -> unquote(beta.clone())))
                             (id w_1.clone()))
                        (and (= (tid t_f) (typ any))
                             (= (tid beta) (typ any))
                             (unquote phi_31)
                             (not (id w_1)))));
                let phi_4 = z3f!(self,
                    (or (and (= (tid beta) (tid gamma)) (id w_2.clone()))
                        (and (= (tid gamma) (typ any)) (not (id w_2)))));
                **f = coerce(t_f, typ!(fun_vec(args_t) -> unquote(beta.clone())), f.take(), p.clone());
                let p = p.clone();
                let e = expr.take();
                *expr = coerce(beta, gamma.clone(), e, p);
                (self.zand(vec![phi_1, phi_2, phi_3, phi_4]), gamma)
            }
            // TypeWhich shows us how to do unary functions. Generazling
            Expr::Func(f, p) => {
                // Fudge stack with local state: the function body will
                // update the environment and the return type.
                let outer_env = self.env.clone();
                let outer_return_typ = self.return_type.take();
                // Fresh metavariable for the return type.
                self.return_type = self.fresh_metavar("ret");
                f.result_typ = self.return_type.clone();
                // Fresh metavariables for formal arguments.
                for (x, t) in f.args_with_typs.iter_mut() {
                    assert_eq!(t, &Type::Missing);
                    *t = self.fresh_metavar("alpha");                    
                    self.env.update(x.clone(), t.clone());
                }
                // Recur into the body.
                self.cgen_stmt(&mut *f.body);
                // Get the return type.
                let return_typ = self.return_type.take();
                // Pop the fudged stack.
                self.return_type = outer_return_typ;
                self.env = outer_env;

                let args: Vec<Type> = f.args_with_typs.iter().map(|(_, t)| t.clone()).collect();

                let w = self.fresh_weight();
                let beta = self.fresh_metavar("beta");
                let phi = z3f!(self,
                    (or
                        (and (= (typ unquote(beta)) (typ fun_vec(args.clone()) -> unquote(return_typ.clone())))
                             (id w.clone()))
                        (and (= (typ unquote(beta)) (typ any))
                             (= (typ unquote(return_typ.clone())) (typ any))
                             /* TODO(arjun): args must be * too. */
                             (not (id w)))));
                let p = p.clone();
                *expr = coerce(typ!(fun_vec(args) -> unquote(return_typ)), beta.clone(), expr.take(), p);
                (phi, beta)
            }
        }
    }

    fn solve_model(&self, model: z3::Model) -> Vec<Type> {
        let mut result = Vec::new();
        for x_ast in self.vars.iter() {
            let x_val_ast = model.eval(x_ast).expect("evaluating metavar");
            result.push(self.z3_to_typ(&model, x_val_ast));
        }
        result
    }
}

#[allow(unused)]
pub fn typeinf(stmt: &mut Stmt) {
    let z3_cfg = z3::Config::new();
    let cxt = z3::Context::new(&z3_cfg);
    let dts = Z3Typ::make_dts(&cxt);
    let dts_list = Z3TypList::make_dts(&cxt);
    let sorts = z3::datatype_builder::create_datatypes(vec![dts, dts_list]);
    let z = Z3Typ::new(&cxt, &sorts[0]);
    let zl = Z3TypList::new(&cxt, &sorts[1]);
    let env = Env::new();
    let mut state = Typeinf {
        vars: Default::default(),
        z,
        zl,
        cxt: &cxt,
        solver: Optimize::new(&cxt),
        // Cannot have return statement at top-level
        return_type: Type::Missing,
        env,
    };
    state.cgen_stmt(stmt);
    match state.solver.check(&[]) {
        SatResult::Unknown => panic!("Got an unknown from Z3"),
        SatResult::Unsat => panic!("type inference failed (unsat)"),
        SatResult::Sat => (),
    };
    let model = state
        .solver
        .get_model()
        .expect("model not available (despite SAT result)");
    let mapping = state.solve_model(model);

    println!("Before subst: {}", &stmt);
    let mut subst_metavar = SubtMetavarVisitor { vars: &mapping };
    stmt.walk(&mut subst_metavar);
    println!("After cgen: {}", &stmt);
}

#[cfg(test)]
mod tests {
    use super::super::super::javascript::{desugar, parse};
    use super::super::super::shared::NameGen;
    use super::super::syntax::*;
    use super::super::type_checking::type_check;
    use super::super::walk::*;
    use super::typeinf;

    #[derive(Default)]
    struct CountToAnys {
        num_anys: usize,
    }

    impl Visitor for CountToAnys {
        fn enter_typ(&mut self, t: &mut Type, loc: &Loc) {
            match (loc, t) {
                (Loc::Node(Context::MetaCoercionRight(..), _), Type::Any) => {
                    self.num_anys += 1;
                }
                _ => { }
            }
        }
    }

    fn typeinf_test(s: &str) -> usize {
        let mut js = parse("<text>", s).expect("error parsing JavaScript");
        let mut ng = NameGen::default();
        desugar(&mut js, &mut ng);
        let mut janky = crate::jankyscript::from_js::from_javascript(js);
        typeinf(&mut janky);
        let mut count_anys = CountToAnys::default();
        janky.walk(&mut count_anys);
        type_check(&janky).expect("result of type inference does not type check");
        return count_anys.num_anys;
    }

    #[test]
    fn janky_plus() {
        let n = typeinf_test(r#"1 + "2";"#);
        assert_eq!(n, 2);
    }

    #[test]
    fn num_plus() {
        let n = typeinf_test(r#"1 + 2;"#);
        assert_eq!(n, 0);
    }

    #[test]
    fn simple_update() {
        let n = typeinf_test(
            r#"
            var x = 20;
            x = 30 + x;
        "#,
        );
        assert_eq!(n, 0);
    }

    #[test]
    fn any_inducing_update() {
        let n = typeinf_test(
            r#"
            var x = 20;
            x = true;
        "#,
        );
        assert_eq!(n, 2);
    }

    #[test]
    fn heterogenous_array() {
        let n = typeinf_test(
            r#"
            [10, "hi", true]
        "#,
        );
        assert_eq!(n, 3);
    }

    #[test]
    fn object_lit() {
        let n = typeinf_test(
            r#"
            ({ x: 10, y: 20 })
        "#,
        );
        assert_eq!(n, 2);
    }

    #[test]
    fn prop_read() {
        let n = typeinf_test(
            r#"
            ({x : 10}).y << 2
            "#,
        );
        assert_eq!(n, 1); // We coerce the 10 to any. The y is coerced *from* any
    }

    #[test]
    fn id_trivial_app() {
        let n = typeinf_test(
            r#"
            function F(x) {
                return x;
            }
            F(100)
            "#);
        assert_eq!(n, 0);
    }

    #[test]
    fn poly_id() {
        let n = typeinf_test(
            r#"
            function F(x) {
                return x;
            }
            F(100);
            F(true);
            "#);
        assert_eq!(n, 2);
    }
}
