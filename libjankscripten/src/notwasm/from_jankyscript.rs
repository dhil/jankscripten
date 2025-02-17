//! This module compiles JankyScript to NotWasm. This requires a transformation that is akin to
//! *A-normalization*:
//!
//! Cormac Flanagan, Amr Sabry, Bruce F. Duba, and Matthias Felleisen.
//! The Essence of Compiling with Continuations. PLDI 1993.
//!
//! However, we use a variation of what the aforementioned paper presents that produces cleaner
//! code. We'll explain this variation using a small language in A Normal Form. The language does
//! not support tail calls, but JankyScript doesn't either. We'll present the A-normalization
//! algorithm using Haskell-ish notation.
//!
//! This is our source language:
//!
//! ```bnf
//! expr ::= c | x | lambda x . expr | expr1(expr2) | expr1 + expr2 | let x = expr1 in expr2
//!
//! ```
//!
//! This is our target language, which closely corresponds to A Normal Form:
//!
//! ```bnf
//! a ::= c | x | lambda x . e
//! b ::= a | a1 + a2 | a1(a2)
//! e ::= let x = b in e | a
//! ```
//!
//! Original program: `1 + (2 * 3)`
//!
//! Compiled program: `let tmp = 2 * 3 in let tmp1 = 1 + tmp in tmp1`
//!
//!
//! The following function translates from the source to target. (Use `id` as the initial value for
//! `k`):
//!
//! ```haskellish
//! anf : expr -> (a -> e) -> e
//! anf c k = k c
//! and x k = k x
//! anf (e1 e2) k = anf e1 (\x1 -> anf e2 (\x2 -> let x = x1(x2) in k x))
//! anf (lambda x . e) k = k (lambda x . anf e (\x -> x))
//! anf (e1 + e2) k =  anf e1 (\x1 -> anf e2 (\x2 -> let r = x1 + x2 in k r))
//! anf (let x = e1 in e2) k = anf e1 (\y -> let x = y in (anf e2 k))
//! ```
//!
//! To compile `expr`, run `anf expr (\a -> a)`.
//!
//! Unfortunately, the previous function introduces a lot of useless names. For example:
//!
//! ```haskellish
//!   anf (let x = 1 + 2 in x) id
//! = anf (1 + 2) (\y -> let x = y in anf x id)
//! = anf (1 + 2) (\y -> let x = y in x)
//! = anf 1 (\x1 -> anf 2 (\x2 -> let r = x1 + x2 in (\y -> let x = y in x) r))
//! = anf 1 (\x1 -> anf 2 (\x2 -> let r = x1 + x2 in let x = r in x))
//! = anf 1 (\x1 -> let r = x1 + 2 in let x = r in x)
//! = let r = 1 + 2 in (let x = r in x)
//! ```
//!
//! We can address this by introducing two kinds of contexts: 1) the (a -> e) context receives an
//! a-value as shown above, and 2) a (b -> e) context that receives a b-expression.
//!
//! ```haskellish
//! data Context = AContext (a -> e) | BContext (b -> e)
//!
//! anf : expr -> Context -> expr
//! anf (e1 + e2) k = anf e1 (AContext (\x1 ->
//!   anf e2 (AContext (\x2 ->
//!     case k of
//!       BContext k' -> k' (x1 + x2)
//!       AContext k' -> let r = x1 + x2 in k' r))))
//! anf (let x = e1 in e2) k = anf e1 (BContext (\b -> let x = b in e2 k))
//! ...
//! ```
//!
//! Instead of case-splitting on kind of context when we need to use it, we can define two helper
//! functions:
//!
//! ```haskellish
//! recv_b : Context -> b -> e
//! recv_b (BContext k) b = k b
//! recv_b (AContext k) b = let x = b in k x -- x is fresh
//!
//! recv_a : Context -> a -> e
//! recv_a (AContext k) a = k a
//! recv_a (BContext k) a = k a -- all a-values can be injected into b-expressions
//! ```
//!
//! When we compile from JankyScript to NotWasm, the two kinds of contexts receive `Syntax::Atom` and
//! `Syntax::Expr`, and the output of A-normalization is a `Syntax::Stmt`.
//!
//! To summarize, A-normalization requires:
//! - Generating fresh names
//! - Two kinds of contexts

use super::super::jankyscript::syntax as J;
use super::super::rope::Rope;
use super::constructors::*;
use super::syntax::*;
use crate::pos::Pos;
use crate::shared::methods::METHODS_TABLE;
use crate::shared::NameGen;
use std::collections::HashMap;

fn compile_lit(lit: J::Lit) -> Lit {
    match lit {
        J::Lit::String(state) => Lit::String(state),
        J::Lit::Regex(_, _) => todo!("regex not supported anywhere in toolchain"),
        J::Lit::Bool(b) => Lit::Bool(b),
        J::Lit::Null => Lit::Null,
        J::Lit::Undefined => Lit::Undefined,
        J::Lit::Num(J::Num::Int(n)) => Lit::I32(n),
        J::Lit::Num(J::Num::Float(x)) => Lit::F64(x),
    }
}

/// State that is needed during A-normalization
#[derive(Default)]
struct S {
    namegen: NameGen,
    functions: HashMap<Id, Function>,
}

impl S {
    fn fresh(&mut self) -> Id {
        self.namegen.fresh("anf")
    }

    fn new_function(&mut self, name: Id, f: Function) {
        // TODO(arjun): we will have to rename functions if there are two with the same name in
        // different scopes.
        self.functions.insert(name, f);
    }
}

/// The contexts for A-normalization
enum C<'a> {
    /// Context expects an `Id`. So, name the result before passing it to the context.
    Id(Box<dyn FnOnce(&'a mut S, Id) -> Rope<Stmt> + 'a>),
    /// Context expects an `Atom`. If it isn't, name the result before passing it to the context.
    Atom(Box<dyn FnOnce(&'a mut S, Atom) -> Rope<Stmt> + 'a>),
    /// Context expects an `Expr`. This is the easy case, since an `Atom` or `Id` can be injected
    /// into an `Expr`.
    Expr(Box<dyn FnOnce(&'a mut S, Expr) -> Rope<Stmt> + 'a>),
}

impl<'a> C<'a> {
    /// Constructs a `C::Atom`, taking care of boxing the function.
    fn a(f: impl FnOnce(&'a mut S, Atom) -> Rope<Stmt> + 'a) -> C<'a> {
        C::Atom(Box::new(f))
    }

    /// Constructs a `C::Expr`, taking care of boxing the function.
    fn e(f: impl FnOnce(&'a mut S, Expr) -> Rope<Stmt> + 'a) -> C<'a> {
        C::Expr(Box::new(f))
    }

    /// Constructs a `C::Id`, taking care of boxing the function.
    fn id(f: impl FnOnce(&'a mut S, Id) -> Rope<Stmt> + 'a) -> C<'a> {
        C::Id(Box::new(f))
    }

    fn recv_a(self, state: &'a mut S, a: Atom) -> Rope<Stmt> {
        match self {
            C::Atom(f) => f(state, a),
            C::Id(f) => match a {
                Atom::Id(x, _) => f(state, x),
                _ => {
                    let x = state.fresh();
                    Rope::singleton(Stmt::Var(
                        VarStmt::new(x.clone(), Expr::Atom(a, Default::default())),
                        Default::default(),
                    ))
                    .append(f(state, x))
                }
            },
            C::Expr(f) => f(state, Expr::Atom(a, Default::default())),
        }
    }

    fn recv_e(self, state: &'a mut S, e: Expr) -> Rope<Stmt> {
        match self {
            // The Id and Atom cases are essentially identical
            C::Id(f) => {
                let x = state.fresh();
                Rope::singleton(Stmt::Var(VarStmt::new(x.clone(), e), Default::default()))
                    .append(f(state, x))
            }
            C::Atom(f) => {
                let x = state.fresh();
                Rope::singleton(Stmt::Var(VarStmt::new(x.clone(), e), Default::default()))
                    .append(f(state, Atom::Id(x, Default::default())))
            }
            C::Expr(f) => f(state, e),
        }
    }
}

/// Compile a vector of expressions, name them, and send their names (in a vector) to a context.
fn compile_exprs<'a>(
    state: &'a mut S,
    exprs: Vec<J::Expr>,
    cxt: impl FnOnce(&'a mut S, Vec<Id>) -> Rope<Stmt>,
) -> Rope<Stmt> {
    let mut ids = Vec::<Id>::new();
    let mut stmts = Rope::new();
    for e in exprs.into_iter() {
        stmts = stmts.append(compile_expr(
            state,
            e,
            C::id(|_s, x| {
                ids.push(x);
                Rope::nil()
            }),
        ));
    }
    return stmts.append(cxt(state, ids));
}

fn coercion_to_expr(c: J::Coercion, a: Atom, p: Pos) -> Atom {
    use J::Coercion::*;
    match c {
        FloatToInt => Atom::FloatToInt(Box::new(a), p),
        IntToFloat => Atom::IntToFloat(Box::new(a), p),
        Tag(..) => to_any_(a, p),
        Untag(ty) => from_any_(a, ty.notwasm_typ(true), p),
        Fun(..) => todo!(), // TODO(michael) needs to call something that proxies the function
        Id(..) => a,
        Seq(c1, c2) => coercion_to_expr(*c2, coercion_to_expr(*c1, a, p.clone()), p),
        Meta(..) => panic!("Meta coerce remains {}", p),
    }
}

fn compile_expr<'a>(state: &'a mut S, expr: J::Expr, cxt: C<'a>) -> Rope<Stmt> {
    match expr {
        J::Expr::JsOp(..) => panic!("impossible case: cannot compile JsOp to WebAssembly"),
        J::Expr::Lit(lit, p) => cxt.recv_a(state, Atom::Lit(compile_lit(lit), p)),
        J::Expr::Array(members, p) => compile_exprs(state, members, move |state, member_ids| {
            let array_name = state.fresh();
            let mut rv = Rope::singleton(Stmt::Var(
                VarStmt::new(
                    array_name.clone(),
                    Expr::prim_call("array_new", vec![], p.clone()),
                ),
                p.clone(),
            ));
            for member_id in member_ids {
                rv = rv.append(Rope::singleton(Stmt::Expression(
                    Expr::prim_call("array_push", vec![array_name.clone(), member_id], p.clone()),
                    p.clone(),
                )))
            }
            rv.append(cxt.recv_a(state, Atom::Id(array_name, p)))
        }),
        J::Expr::Object(keys_exprs, p) => {
            let (keys, exprs): (Vec<_>, Vec<_>) = keys_exprs.into_iter().unzip();
            compile_exprs(state, exprs, move |state, ids| {
                // TODO: semi-static classes when objects are defined like this
                let obj_name = state.fresh();
                let mut rv = Rope::singleton(Stmt::Var(
                    VarStmt::new(obj_name.clone(), Expr::ObjectEmpty),
                    p.clone(),
                ));
                for (key, id) in keys.into_iter().zip(ids) {
                    let key_str = match key {
                        J::Key::Str(state) => state,
                        J::Key::Int(_) => todo!(),
                    };
                    rv = rv.append(Rope::singleton(Stmt::Expression(
                        Expr::ObjectSet(
                            Atom::Id(obj_name.clone(), p.clone()),
                            str_(key_str, p.clone()),
                            Atom::Id(id, p.clone()),
                            p.clone(),
                        ),
                        p.clone(),
                    )))
                }
                rv.append(cxt.recv_a(state, Atom::Id(obj_name, p)))
            })
        }
        J::Expr::Dot(obj, field, p) => compile_expr(
            state,
            *obj,
            C::a(move |state, obj| {
                cxt.recv_a(
                    state,
                    object_get_(obj, str_(field.into_name(), p.clone()), p),
                )
            }),
        ),
        J::Expr::Unary(op, expr, p) => compile_expr(
            state,
            *expr,
            C::a(move |state, a| cxt.recv_a(state, unary_(op, a, p))),
        ),
        J::Expr::Bracket(c, f, t, p) => compile_expr(
            state,
            *c,
            C::a(move |state, c| {
                compile_expr(
                    state,
                    *f,
                    C::a(move |state, f| match t {
                        J::Type::Array => {
                            cxt.recv_a(state, prim_app_("array_index", vec![c, f], p))
                        }
                        J::Type::DynObject => cxt.recv_a(state, object_get_(c, f, p)),
                        J::Type::String => todo!("string index???"),
                        _ => panic!("non-array non-object index"),
                    }),
                )
            }),
        ),
        J::Expr::Coercion(coercion, e, p) => compile_expr(
            state,
            *e,
            C::a(move |state, a| cxt.recv_a(state, coercion_to_expr(coercion, a, p))),
        ),
        J::Expr::Id(x, _, p) => cxt.recv_a(state, Atom::Id(x, p)),
        J::Expr::Func(f, p) => {
            let name = state.fresh();
            let f = compile_function(state, f, p.clone());
            state.new_function(name.clone(), f);
            cxt.recv_a(state, Atom::Id(name, p))
        }
        J::Expr::Closure(f, env, p) => {
            let name = state.fresh();
            let f = compile_function(state, f, p.clone());
            state.new_function(name.clone(), f);
            // compile the environment, adapted from compile_exprs
            let mut env_items = Vec::new();
            let mut stmts = Rope::new();
            for (e, ty) in env.into_iter() {
                stmts = stmts.append(compile_expr(
                    state,
                    e,
                    C::a(|_s, x| {
                        env_items.push((x, ty.notwasm_typ(true)));
                        Rope::nil()
                    }),
                ));
            }
            stmts.append(cxt.recv_e(state, Expr::Closure(name, env_items, p)))
        }
        J::Expr::Binary(op, e1, e2, p) => compile_expr(
            state,
            *e1,
            C::a(move |state, a1| {
                compile_expr(
                    state,
                    *e2,
                    C::a(move |state, a2| {
                        cxt.recv_a(state, Atom::Binary(op, Box::new(a1), Box::new(a2), p))
                    }),
                )
            }),
        ),
        J::Expr::Assign(lv, e, p) => compile_expr(
            state,
            *e,
            // TODO(luna): if we change Assign to an expression, we can make
            // this C::e and drop the clone which will generate less useless
            // locals; but it will mean sometimes dropping values. we
            // could also change Assign to an atom, which would mean
            // introducing new locals for assignment expressions
            // but differently. see this discussion on slack:
            // https://plasma.slack.com/archives/C013E3BK7QA/p1596656877066800
            C::a(move |state, a| match *lv {
                J::LValue::Id(id, _) => {
                    Rope::singleton(Stmt::Assign(id, atom_(a.clone(), p.clone()), p))
                        .append(cxt.recv_a(state, a))
                }
                J::LValue::Dot(container, field) => {
                    // TODO(luna): don't assume bracket is array
                    compile_expr(
                        state,
                        container,
                        // TODO(luna): support array set in notwasm, i can't
                        // believe we don't yet
                        C::a(move |state, cont| {
                            cxt.recv_e(
                                state,
                                Expr::ObjectSet(
                                    cont,
                                    Atom::Lit(Lit::String(field.to_pretty(80)), p.clone()),
                                    a,
                                    p,
                                ),
                            )
                        }),
                    )
                }
                J::LValue::Bracket(container, field, typ) => compile_expr(
                    state,
                    container,
                    C::a(move |state, cont| {
                        compile_expr(
                            state,
                            field,
                            C::a(move |state, f| match typ {
                                J::Type::Array => cxt.recv_e(state, Expr::ArraySet(cont, f, a, p)),
                                J::Type::DynObject => {
                                    cxt.recv_e(state, Expr::ObjectSet(cont, f, a, p))
                                }
                                _ => panic!("bad bracket lvalue type"),
                            }),
                        )
                    }),
                ),
            }),
        ),
        J::Expr::PrimCall(prim_name, args, p) => {
            compile_exprs(state, args, move |state, arg_ids| {
                cxt.recv_e(state, Expr::PrimCall(prim_name, arg_ids, p))
            })
        }
        J::Expr::Call(fun, args, p) => compile_expr(
            state,
            *fun,
            C::id(move |state, fun_id| {
                compile_exprs(state, args, move |state, arg_ids| {
                    cxt.recv_e(state, Expr::ClosureCall(fun_id, arg_ids, p))
                })
            }),
        ),
        J::Expr::MethodCall(obj, method, args, _, p) => compile_expr(
            state,
            *obj,
            C::id(move |state, obj_id| {
                // borrow checker
                let args_len = args.len();
                compile_exprs(state, args, move |state, arg_ids| {
                    let possible_typs = METHODS_TABLE
                        .get(&(method.as_str(), args_len))
                        .unwrap()
                        .iter()
                        // never a function type
                        .map(|t| t.notwasm_typ(false))
                        .collect();
                    cxt.recv_e(
                        state,
                        Expr::AnyMethodCall(obj_id, Lit::String(method), arg_ids, possible_typs, p),
                    )
                })
            }),
        ),
        J::Expr::Length(obj, _, p) => compile_expr(
            state,
            *obj,
            C::id(move |state, obj_id| {
                cxt.recv_a(
                    state,
                    Atom::AnyLength(obj_id, Lit::String("length".into()), p),
                )
            }),
        ),
        J::Expr::NewRef(expr, ty, p) => compile_expr(
            state,
            *expr,
            C::a(move |state, of| cxt.recv_e(state, Expr::NewRef(of, ty.notwasm_typ(true), p))),
        ),
        J::Expr::Deref(expr, ty, p) => compile_expr(
            state,
            *expr,
            C::a(move |state, of| cxt.recv_a(state, deref_(of, ty.notwasm_typ(true), p))),
        ),
        J::Expr::Store(into, expr, _, p) => compile_expr(
            state,
            *into,
            C::id(move |state, into| {
                compile_expr(
                    state,
                    *expr,
                    C::e(move |_s, what| Rope::singleton(Stmt::Store(into, what, p))),
                )
            }),
        ),
        J::Expr::EnvGet(i, ty, p) => cxt.recv_a(state, Atom::EnvGet(i, ty.notwasm_typ(true), p)),
    }
}

fn compile_stmt<'a>(state: &'a mut S, stmt: J::Stmt) -> Rope<Stmt> {
    use J::Stmt as S;
    match stmt {
        // In JankyScript:
        //
        // var r = f() + 1;
        //
        // In NotWasm:
        //
        // var tmp = f();
        // var r = tmp + 1;
        //
        // NOTE(luna): We copy in the type unlike our usual approach because
        // sometimes we "mis-annotate" undefined for initialization
        S::Var(x, t, e, p) => compile_expr(
            state,
            *e,
            C::e(|_s, e_notwasm| {
                Rope::singleton(Stmt::Var(
                    VarStmt {
                        id: x,
                        named: e_notwasm,
                        ty: Some(t.notwasm_typ(true)),
                    },
                    p,
                ))
            }),
        ),
        S::Block(stmts, p) => Rope::singleton(Stmt::Block(
            stmts
                .into_iter()
                .map(|stmt| compile_stmt(state, stmt))
                .flatten()
                .collect(),
            p,
        )),
        S::Empty => Rope::singleton(Stmt::Empty),
        S::Expr(e, _) => compile_expr(
            state,
            *e,
            // We could use a C::e context. However, the C::a context will make generated code
            // easier to understand in trivial examples. A C::e context would discard useless
            // binary operations.
            C::a(|_s, _a_notwasm| Rope::nil()),
        ),
        S::If(cond, then_branch, else_branch, p) => compile_expr(
            state,
            *cond,
            C::a(|state, a| {
                Rope::singleton(if_(
                    a,
                    compile_stmt_block(state, *then_branch, p.clone()),
                    compile_stmt_block(state, *else_branch, p.clone()),
                    p,
                ))
            }),
        ),
        S::Loop(body, p) => Rope::singleton(loop_(
            Stmt::Block(compile_stmt(state, *body).into_iter().collect(), p.clone()),
            p,
        )),
        S::ForIn(..) => todo!("for..in in notwasm"),
        S::Label(x, body, p) => Rope::singleton(label_(
            Label::Named(x.to_pretty(80)),
            Stmt::Block(compile_stmt(state, *body).into_iter().collect(), p.clone()),
            p,
        )),
        S::Break(x, p) => Rope::singleton(Stmt::Break(Label::Named(x.to_pretty(80)), p)),
        // TODO(luna): notwasm needs to support exceptions
        // (this just executes the statement with no continuation; in jankyp
        // we discovered that in most benchmarks, even if they use try/catch, no
        // error is thrown)
        S::Catch(try_stmt, _, _, _) => compile_stmt(state, *try_stmt),
        S::Finally(_, _, _) => todo!("NotWasm needs to support exceptions"),
        // TODO(luna): notwasm needs to support exceptions
        S::Throw(_, _) => Rope::new(),
        S::Return(e, p) => {
            compile_expr(state, *e, C::a(|_s, a| Rope::singleton(Stmt::Return(a, p))))
        }
    }
}

fn compile_stmt_block(state: &mut S, stmt: J::Stmt, p: Pos) -> Stmt {
    rope_to_block(compile_stmt(state, stmt), p)
}
fn rope_to_block(rope: Rope<Stmt>, p: Pos) -> Stmt {
    Stmt::Block(rope.into_iter().collect(), p)
}

fn compile_function<'a>(state: &'a mut S, f: J::Func, p: Pos) -> Function {
    let (mut param_names, jnks_tys): (Vec<_>, Vec<_>) = f.args_with_typs.into_iter().unzip();
    // add the env to the function type as well. this only matters when
    // not sent through an any so immediate application weirdness so probably
    // related to this. again, this could be much cleaner if we figured out a
    // way to iterate over all types in jankyscript and change them with
    // closure conversion
    param_names.insert(0, Id::Bogus("env"));
    let param_tys = std::iter::once(Type::Env)
        .chain(jnks_tys.into_iter().map(|t| t.notwasm_typ(true)))
        .collect();
    Function {
        body: Stmt::Block(compile_stmt(state, *f.body).into_iter().collect(), p),
        params: param_names,
        fn_type: FnType {
            args: param_tys,
            result: Some(Box::new(f.result_typ.notwasm_typ(true))),
        },
        span: Default::default(),
    }
}

pub fn from_jankyscript(janky_program: J::Stmt) -> Program {
    let mut state: S = Default::default();
    let main_body = Stmt::Block(
        compile_stmt(&mut state, janky_program)
            .into_iter()
            .collect(),
        Default::default(),
    );
    state.new_function(
        Id::from("main"),
        Function {
            body: main_body,
            params: Vec::new(),
            fn_type: FnType {
                args: Vec::new(),
                result: None,
            },
            span: Default::default(),
        },
    );
    Program {
        rts_fn_imports: HashMap::new(),
        functions: state.functions,
        globals: HashMap::new(),
        data: Vec::new(),
    }
}
