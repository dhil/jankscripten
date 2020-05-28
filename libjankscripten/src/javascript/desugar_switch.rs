use super::syntax::*;
use super::walk::*;
use super::*;
use super::constructors::*;
use resast::BinaryOp;
use resast::LogicalOp;

struct SwitchToIf { }

impl Visitor for SwitchToIf {
    /// called before recursing on a statement
    fn enter_stmt(&mut self, _stmt: &mut Stmt) {
    }
    /// called before recursing on an expression
    fn enter_expr(&mut self, _expr: &mut Expr, _loc: &mut Loc) {}
    /// called after recursing on a statement, with the new value
    fn exit_stmt(&mut self, stmt: &mut Stmt) {
        match stmt {
            Stmt::Switch(expr, cases, default) => { //cases = vec<(expr, stmt)>
                let test = &**expr;
                let mut v = vec![
                    vardecl1_("fallthrough", Expr::Lit(Lit::Bool(false))),
                    vardecl1_("test", test.clone())
                ];
                for (e, s) in cases {
                    v.push(
                        Stmt::If(
                            Box::new(Expr::Binary(
                                BinOp::LogicalOp(LogicalOp::Or),
                                Box::new(Expr::Binary(
                                    BinOp::BinaryOp(BinaryOp::StrictEqual), 
                                    Box::new(Expr::Id(Id::Named("test".to_string()))),
                                    Box::new(e.clone()))),
                                Box::new(Expr::Id(Id::Named("fallthrough".to_string()))))),
                            Box::new(s.clone()),
                            Box::new(Stmt::Empty)))
                }
                let d = &**default;
                //println!("{:?}", d);
                match d {
                    Stmt::Block(dv) => {
                        for s in dv {
                            v.push(s.clone());
                        }
                    },
                    _ => v.push(d.clone())
                }
                *stmt = Stmt::Label(
                    Id::Named("sw".to_string()),
                    Box::new(Stmt::Block(v)))
            }
            _ => {}
        }
    }
    /// called after recursing on an expression, with the new value
    fn exit_expr(&mut self, _expr: &mut Expr, _loc: &mut Loc) {}
}

#[test] 
    fn switchtest() {
        let prog = parse(r#"
            var x = 1;
            switch(x) {
                case 0:
                    x = 1;
                    break;
                case 1:
                    x = 2;
                default:
                    x = 3;
            }
        "#).unwrap();

        print!("{:?}", prog);
    }

    #[test] 
    fn iftest() {
        let prog = parse(r#"
            sw: {
                let fallthrough = false;
                let test = x;
                if (test === 0 || fallthrough) {
                    x = 1;
                    break sw;
                } if (test === 1 || fallthrough) {
                    x = 2;
                } 
                x = 3;
                x = 4;
            }
        "#).unwrap();

        print!("{:?}", prog);
    }

#[test]
fn switchif() {
    let mut prog = parse(r#"
        switch(x) {
            case 0: 
                x = 1;
                break;
            case 1: 
                x = 2;
        }
    "#).unwrap();

    prog.walk(&mut SwitchToIf {});

    print!("{:?}", prog);
}


