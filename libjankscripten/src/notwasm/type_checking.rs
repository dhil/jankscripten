use super::constructors::*;
use super::syntax::*;
use crate::pos::Pos;
use im_rc::HashMap;
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct Env {
    env: HashMap<Id, Type>,
    imports: HashMap<Id, Type>,
}

impl Env {
    pub fn new() -> Env {
        let env: HashMap<Id, Type> = super::rt_bindings::get_rt_bindings()
            .into_iter()
            .map(|(k, v)| (Id::Named(k), v))
            .collect();
        Env {
            imports: env.clone(),
            env,
        }
    }

    pub fn get(&self, id: &Id) -> Option<&Type> {
        self.env.get(id)
    }

    pub fn insert(&mut self, id: Id, ty: Type) -> Option<Type> {
        self.env.insert(id, ty)
    }

    pub fn update(&self, id: Id, ty: Type) -> Self {
        Env {
            env: self.env.update(id, ty),
            imports: self.imports.clone(),
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum TypeCheckingError {
    #[error("undefined variable `{0}` at `{1}`")]
    NoSuchVariable(Id, Pos),
    #[error("`{0}` expected type `{1}` but received `{2}` at `{3}`")]
    TypeMismatch(String, Type, Type, Pos),
    #[error("expected function (`{0}`), but got `{1}` at `{2}`")]
    ExpectedFunction(Id, Type, Pos),
    #[error("`{0}` expected hash table, but got `{1}` at `{2}`")]
    ExpectedHT(String, Type, Pos),
    #[error("`{0}` expected array, but got `{1}` at `{2}`")]
    ExpectedArray(String, Type, Pos),
    #[error("`{0}` expected ref, but got `{1}` at `{2}`")]
    ExpectedRef(String, Type, Pos),
    #[error("unexpected return type `{0}` at `{1}`")]
    UnexpectedReturn(Type, Pos),
    #[error(
        "arity mismatch at `{0}`, expected `{1}` parameters but received `{2}` arguments at `{3}`"
    )]
    ArityMismatch(Id, usize, usize, Pos),
    #[error("identifier `{0}` is multiply defined at `{1}`")]
    MultiplyDefined(Id, Pos),
    #[error("In context `{0}`, unexpected type `{1}` at `{2}`")]
    InvalidInContext(String, Type, Pos),
    #[error("Error type-checking NotWasm: `{0}` at `{1}`")]
    Other(String, Pos),
}

pub type TypeCheckingResult<T> = Result<T, TypeCheckingError>;

macro_rules! err {
    ($s:expr, $($t:tt)*) => (
        TypeCheckingError::Other(format!($($t)*), $s.clone())
    )
}

macro_rules! error {
    ($s:expr, $($t:tt)*) => (
        Err(TypeCheckingError::Other(format!($($t)*), $s.clone()))
    )
}

fn invalid_in_context<T>(message: impl Into<String>, ty: &Type, s: &Pos) -> TypeCheckingResult<T> {
    return Err(TypeCheckingError::InvalidInContext(
        message.into(),
        ty.clone(),
        s.clone(),
    ));
}

fn lookup(env: &Env, id: &Id, s: &Pos) -> TypeCheckingResult<Type> {
    if let Some(ty) = env.get(id) {
        Ok(ty.clone())
    } else {
        Err(TypeCheckingError::NoSuchVariable(id.clone(), s.clone()))
    }
}

fn ensure(msg: &str, expected: Type, got: Type, s: &Pos) -> TypeCheckingResult<Type> {
    if expected == got {
        Ok(got)
    } else {
        Err(TypeCheckingError::TypeMismatch(
            String::from(msg),
            expected,
            got,
            s.clone(),
        ))
    }
}

/// Type-check a program, returning `Ok(())` if successful. This function also
/// mutates the program, adding additional type annotations that are needed for
/// code-generation. We could require these annotations in the input program,
/// but they are trivial to calculate.
pub fn type_check(p: &mut Program) -> TypeCheckingResult<()> {
    // Top-level type environment, including checks for duplicate identifiers.
    let mut env: Env = Env::new();
    for (x, t) in &p.rts_fn_imports {
        env.insert(Id::Named(x.to_string()), t.clone());
        env.imports.insert(Id::Named(x.to_string()), t.clone());
    }

    for (id, f) in p.functions.iter() {
        if env
            .insert(id.clone(), f.fn_type.clone().to_type())
            .is_some()
        {
            return Err(TypeCheckingError::MultiplyDefined(
                id.clone(),
                Default::default(),
            ));
        }
    }
    for (id, g) in p.globals.iter_mut() {
        // if the global is initialized
        if let Some(atom) = &mut g.atom {
            // type check it
            let got = type_check_atom(&env, atom)?;
            let p = Pos::UNKNOWN;
            ensure("global var type", g.ty.clone(), got, &p)?;
        }

        // Insert the global into the environment
        if env.insert(id.clone(), g.ty.clone()).is_some() {
            return Err(TypeCheckingError::MultiplyDefined(
                id.clone(),
                Pos::UNKNOWN.clone(),
            ));
        }
    }

    for (id, f) in p.functions.iter_mut() {
        type_check_function(env.clone(), id, f)?;
    }

    return Ok(());
}

fn ensure_ref(msg: &str, got: Type, s: &Pos) -> TypeCheckingResult<Type> {
    match got {
        Type::Ref(ty) => Ok(*ty),
        _ => Err(TypeCheckingError::ExpectedRef(
            String::from(msg),
            got,
            s.clone(),
        )),
    }
}

fn type_check_function(mut env: Env, id: &Id, f: &mut Function) -> TypeCheckingResult<()> {
    // TODO(arjun): We should probably check for multiply-defined argument
    // names here.
    if f.fn_type.args.len() != f.params.len() {
        return Err(TypeCheckingError::ArityMismatch(
            id.clone(),
            f.params.len(),
            f.fn_type.args.len(),
            Default::default(), // TODO(arjun): Fix
        ));
    }

    for (id, ty) in f.params.iter().zip(f.fn_type.args.iter()) {
        env.insert(id.clone(), ty.clone());
    }

    let _ = type_check_stmt(env, &mut f.body, &f.fn_type.result.clone().map(|b| *b))?;

    Ok(())
}

fn type_check_stmt(env: Env, s: &mut Stmt, ret_ty: &Option<Type>) -> TypeCheckingResult<Env> {
    match s {
        Stmt::Empty => Ok(env),
        Stmt::Var(var_stmt, _) => {
            let ty = type_check_expr(&env, &mut var_stmt.named)?;
            if let VarStmt {
                named: Expr::Atom(Atom::Lit(Lit::Undefined, _), _),
                ty: Some(_),
                ..
            } = var_stmt
            {
                // This is an initialization undefined
            } else {
                assert!(var_stmt.ty.is_none() || var_stmt.ty.as_ref() == Some(&ty));
                var_stmt.set_ty(ty.clone());
            }
            let id = &var_stmt.id;

            // ??? MMG what do we want here? i assume we don't actually want to allow strong update...
            if let Id::Named(name) = id {
                if name == "_" {
                    return Ok(env);
                }
            }

            // TODO(luna): what do we really want to do here? really we should
            // have desugared multiple vars to assignments long ago probably
            //if lookup(&env, id, s).is_ok() {
            //    Err(TypeCheckingError::MultiplyDefined(id.clone(), s))
            //} else {
            Ok(env.update(id.clone(), var_stmt.ty.clone().unwrap()))
            //}
        }
        Stmt::Expression(e, _) => {
            let _ = type_check_expr(&env, e)?;

            Ok(env)
        }
        Stmt::Store(id, e, s) => {
            let got_id = lookup(&env, id, s)?;
            let got_expr = type_check_expr(&env, e)?;

            let type_pointed_to = ensure_ref("ref type", got_id, s)?;

            ensure("ref store", type_pointed_to, got_expr, s)?;

            Ok(env)
        }
        Stmt::Assign(id, e, s) => {
            let got_id = lookup(&env, id, s)?;
            let got_expr = type_check_expr(&env, e)?;
            ensure("assign", got_id, got_expr, s)?;

            Ok(env)
        }
        Stmt::If(a_cond, s_then, s_else, s) => {
            let got = type_check_atom(&env, a_cond)?;
            let _ = ensure("if (conditional)", Type::Bool, got, s)?;

            // then/else branches are new blocks/scopes
            let _ = type_check_stmt(env.clone(), s_then, ret_ty)?;
            let _ = type_check_stmt(env.clone(), s_else, ret_ty)?;

            Ok(env)
        }
        Stmt::Loop(s_body, _) => {
            type_check_stmt(env.clone(), s_body, ret_ty)?;
            Ok(env)
        }
        Stmt::Label(_lbl, s_body, _) => {
            // LATER label checking
            type_check_stmt(env.clone(), s_body, ret_ty)?;
            Ok(env)
        }
        Stmt::Break(_lbl, _) => Ok(env),
        Stmt::Return(a, s) => {
            let got = type_check_atom(&env, a)?;

            // ??? MMG if ret_ty = None, can one return early?
            match ret_ty {
                None => Err(TypeCheckingError::UnexpectedReturn(got, s.clone())),
                Some(ret_ty) => {
                    let _ = ensure("return", ret_ty.clone(), got, s)?;

                    Ok(env)
                }
            }
        }
        Stmt::Block(ss, _) => {
            let mut env_inner = env.clone();

            for s in ss.iter_mut() {
                env_inner = type_check_stmt(env_inner, s, ret_ty)?;
            }

            Ok(env)
        }
        Stmt::Trap => Ok(env),
        Stmt::Goto(_lbl, _) => unimplemented!(),
    }
}

fn type_check_expr(env: &Env, e: &mut Expr) -> TypeCheckingResult<Type> {
    match e {
        Expr::ObjectEmpty => Ok(Type::DynObject),
        Expr::ArraySet(a_arr, a_idx, a_val, s) => {
            let got_arr = type_check_atom(env, a_arr)?;
            let got_idx = type_check_atom(env, a_idx)?;
            let got_val = type_check_atom(env, a_val)?;
            let _ = ensure("array set (index)", Type::I32, got_idx, s)?;
            let _ = ensure("array set (array)", Type::Array, got_arr, s);
            let _ = ensure("array set (value)", Type::Any, got_val, s);
            Ok(Type::Any)
        }
        Expr::ObjectSet(a_obj, a_field, a_val, s) => {
            let got_obj = type_check_atom(env, a_obj)?;
            let got_field = type_check_atom(env, a_field)?;
            let got_val = type_check_atom(env, a_val)?;

            let _ = ensure("object set (obj)", Type::DynObject, got_obj, s)?;
            let _ = ensure("object set (field)", Type::String, got_field, s)?;
            let _ = ensure("object set (val)", Type::Any, got_val, s)?;

            Ok(Type::Any) // returns value set
        }
        Expr::PrimCall(crate::rts_function::RTSFunction::Import(name), args, s) => {
            let ty = env.imports.get(&Id::Named(name.clone())).ok_or(err!(
                s,
                "invalid primitive {}",
                name
            ))?;
            let (arg_tys, opt_ret_ty) = ty.unwrap_fun();
            let ret_ty = opt_ret_ty.ok_or(err!(s, "invalid return type for {}", name))?;
            if arg_tys.len() != args.len() {
                return Err(err!(s, "wrong number of arguments for {}", name));
            }
            for ((i, expected_ty), arg) in arg_tys.iter().enumerate().zip(args.iter_mut()) {
                let got_ty = lookup(env, arg, s)?;
                if expected_ty != &got_ty {
                    return Err(err!(
                        s,
                        "wrong type for argument {} of {}. Expected {}, but received {}",
                        i,
                        name,
                        expected_ty,
                        got_ty
                    ));
                }
            }
            Ok(ret_ty.clone())
        }
        Expr::PrimCall(prim, args, s) => {
            match prim.janky_typ().notwasm_typ(false) {
                Type::Fn(fn_ty) => {
                    let arg_tys = args
                        .into_iter()
                        .map(|a| lookup(env, a, s))
                        .collect::<Result<Vec<_>, _>>()?;
                    if arg_tys.len() != fn_ty.args.len() {
                        error!(
                            s,
                            "primitive {:?} expected {} arguments, but received {}",
                            prim,
                            fn_ty.args.len(),
                            arg_tys.len(),
                        )
                    } else if arg_tys
                        .iter()
                        .zip(fn_ty.args.iter())
                        .any(|(t1, t2)| t1 != t2)
                    {
                        error!(s, "primitive {:?} applied to wrong argument type", prim)
                    } else {
                        // ??? MMG do we need a void/unit type?
                        Ok(match &fn_ty.result {
                            None => Type::Any,
                            Some(t) => *t.clone(),
                        })
                    }
                }
                _ => error!(s, "primitive is not a function ({:?})", prim),
            }
        }
        Expr::Call(id_f, actuals, s) => {
            let got_f = lookup(env, id_f, s)?;
            if let Type::Fn(fn_ty) = got_f {
                type_check_call(env, id_f, actuals, fn_ty, false, s)
            } else {
                Err(TypeCheckingError::ExpectedFunction(
                    id_f.clone(),
                    got_f,
                    s.clone(),
                ))
            }
        }
        Expr::AnyMethodCall(obj, _, actuals, _, s) => {
            ensure("AnyMethodCall", Type::Any, lookup(env, obj, s)?, s)?;
            actuals.iter().try_for_each(|a| {
                ensure("method arg", Type::Any, lookup(env, a, s)?, s).map(|_| ())
            })?;
            Ok(Type::Any)
        }
        Expr::ClosureCall(id_f, actuals, s) => {
            let got_f = lookup(env, id_f, s)?;
            if let Type::Closure(fn_ty) = got_f {
                type_check_call(env, id_f, actuals, fn_ty, true, s)
            } else {
                Err(TypeCheckingError::ExpectedFunction(
                    id_f.clone(),
                    got_f,
                    s.clone(),
                ))
            }
        }
        Expr::NewRef(a, ty, s) => {
            let actual = type_check_atom(env, a)?;
            ensure("new ref", ty.clone(), actual, s)?;
            Ok(ref_ty_(ty.clone()))
        }
        Expr::Atom(a, _) => type_check_atom(env, a),
        // this is really an existential type but for now i'm gonna try to
        // get away with pretending Type::Closure((i32) -> i32; [i32]) ==
        // Type::Closure((i32 -> i32; [])
        Expr::Closure(id, _, s) => match lookup(env, id, s) {
            Ok(Type::Fn(fn_ty)) => Ok(Type::Closure(fn_ty)),
            Ok(got) => Err(TypeCheckingError::ExpectedFunction(
                id.clone(),
                got,
                s.clone(),
            )),
            Err(e) => Err(e),
        },
    }
}

/// implicit_arg is true in a closure; typechecks as if fn_ty started with
/// an Env
fn type_check_call(
    env: &Env,
    id_f: &Id,
    actuals: &[Id],
    fn_ty: FnType,
    implicit_arg: bool,
    s: &Pos,
) -> TypeCheckingResult<Type> {
    // arity check

    let actuals_len = actuals.len() + if implicit_arg { 1 } else { 0 };
    let expected_len = fn_ty.args.len();
    if actuals_len != expected_len {
        return Err(TypeCheckingError::ArityMismatch(
            id_f.clone(),
            actuals_len,
            fn_ty.args.len(),
            s.clone(),
        ));
    }

    // match formals and actuals
    let mut nth = 0;
    let mut args_iter = fn_ty.args.iter();
    if implicit_arg {
        match args_iter.next() {
            Some(Type::Env) => (),
            Some(got) => {
                return Err(TypeCheckingError::TypeMismatch(
                    String::from("closure must accept environment"),
                    Type::Env,
                    got.clone(),
                    s.clone(),
                ))
            }
            None => unreachable!(),
        }
    }
    for (actual, formal) in actuals.iter().zip(args_iter) {
        let got = lookup(env, actual, s)?;
        let _ = ensure(
            &format!("call {:?} (argument #{})", id_f, nth),
            formal.clone(),
            got,
            s,
        )?;
        nth += 1;
    }

    // return type or any
    // ??? MMG do we need a void/unit type?
    Ok(fn_ty.result.map(|b| *b).unwrap_or(Type::Any))
}

fn assert_variant_of_any(ty: &Type, s: &Pos) -> TypeCheckingResult<()> {
    match ty {
        // an any can be stored in an any right? but, i can see why you
        // wouldn't want to generate code that does so
        Type::Any => invalid_in_context("cannot be stored in an Any", &ty, s),
        Type::I32 => Ok(()),
        Type::F64 => Ok(()),
        Type::Bool => Ok(()),
        Type::String => Ok(()),
        // We need to think this through. We cannot store arbitrary functions
        // inside an Any.
        Type::Fn(ty) => {
            if Some(&Type::Env) == ty.args.get(0) {
                Ok(())
            } else {
                error!(s, "function must accept dummy environment to be any-ified")
            }
        }
        Type::Closure(_) => Ok(()),
        // The following turn into pointers, and an Any can store a pointer
        Type::HT => Ok(()),
        Type::Array => Ok(()),
        Type::DynObject => Ok(()),
        Type::Ref(..) => invalid_in_context("ref should not be stored in Any", &ty, s),
        Type::Env => invalid_in_context("environments are not values", &ty, s),
        Type::Ptr => Ok(()),
    }
}

fn type_check_atom(env: &Env, a: &mut Atom) -> TypeCheckingResult<Type> {
    match a {
        Atom::Deref(a, ty, s) => ensure(
            "dereference",
            ty.clone(),
            ensure_ref("deref atom", type_check_atom(env, a)?, s)?,
            s,
        ),
        Atom::Lit(l, _) => Ok(l.notwasm_typ()),
        Atom::PrimApp(prim, args, s) => {
            let ty = lookup(env, prim, s)?;
            let (expected_arg_ts, ret_t) = ty.unwrap_fun();
            let ret_t = ret_t.expect("primtive function that returns a value");
            let arg_ts = args
                .iter_mut()
                .map(|a| type_check_atom(env, a))
                .collect::<Result<Vec<_>, _>>()?;
            if arg_ts.len() != expected_arg_ts.len() {
                return error!(
                    s,
                    "primitive {:?} expected {} arguments, but received {}",
                    prim,
                    expected_arg_ts.len(),
                    arg_ts.len()
                );
            }

            if arg_ts
                .iter()
                .zip(expected_arg_ts.iter())
                .any(|(t1, t2)| t1 != t2)
            {
                return error!(s, "primitive {:?} applied to wrong argument type", prim);
            }
            return Ok(ret_t.clone());
        }
        Atom::ToAny(to_any, s) => {
            let ty = type_check_atom(env, &mut to_any.atom)?;
            assert_variant_of_any(&ty, s)?;
            to_any.set_ty(ty);
            Ok(Type::Any)
        }
        Atom::FromAny(a, ty, s) => {
            let got = type_check_atom(env, a)?;
            ensure("from_any", Type::Any, got, s)?;
            Ok(ty.clone())
        }
        Atom::FloatToInt(a, s) => {
            let got = type_check_atom(env, a)?;
            ensure("float to int", Type::F64, got, s)?;
            Ok(Type::I32)
        }
        Atom::IntToFloat(a, s) => {
            let got = type_check_atom(env, a)?;
            ensure("int to float", Type::I32, got, s)?;
            Ok(Type::F64)
        }
        Atom::Id(id, s) => lookup(env, id, s),
        Atom::GetPrimFunc(id, s) => lookup(env, id, s),
        Atom::ObjectGet(a_obj, a_field, s) => {
            let got_obj = type_check_atom(env, a_obj)?;
            let got_field = type_check_atom(env, a_field)?;

            let _ = ensure("object get field", Type::String, got_field, s)?;
            let _ = ensure("object field", Type::DynObject, got_obj, s)?;
            Ok(Type::Any)
        }
        Atom::AnyLength(a_obj, _, s) => {
            let got_obj = lookup(env, a_obj, s)?;
            let _ = ensure("object field", Type::Any, got_obj, s)?;
            Ok(Type::Any)
        }
        Atom::Unary(op, a, s) => {
            let (ty_in, ty_out) = op.notwasm_typ(true);
            let got = type_check_atom(env, a)?;
            let _ = ensure(&format!("unary ({:?})", op), ty_in, got, s)?;
            Ok(ty_out)
        }
        Atom::Binary(BinaryOp::PtrEq, a_l, a_r, s) => {
            let got_l = type_check_atom(env, a_l)?;
            let got_r = type_check_atom(env, a_r)?;
            let _ = ensure("binary (===) lhs", got_l.clone(), got_r.clone(), s)?;
            Ok(Type::Bool)
        }
        Atom::Binary(op, a_l, a_r, s) => {
            let (ty_in, ty_out) = op.notwasm_typ(true);
            let got_l = type_check_atom(env, a_l)?;
            let got_r = type_check_atom(env, a_r)?;
            let _ = ensure(&format!("binary ({:?}) lhs", op), ty_in.clone(), got_l, s)?;
            let _ = ensure(&format!("binary ({:?}) lhs", op), ty_in, got_r, s)?;
            Ok(ty_out)
        }
        Atom::EnvGet(_, ty, _) => Ok(ty.clone()),
    }
}
