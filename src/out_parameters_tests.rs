use crate::{ast::{Expr,Item,Stmt,TypeRef}, grammar, out_parameters};

fn lower(source:&str)->Result<crate::ast::Program,String>{
    let program=grammar::ProgramParser::new().parse(source)
        .map_err(|err|format!("{err:?}"))?;
    let arguments=crate::call_arguments::lower(&program)?;
    out_parameters::lower(&arguments)
}

#[test]
fn output_declaration_in_call_is_typed_and_borrowed() {
    let source=r#"
        fn try_value(out value:i32)->bool {
            value=42;
            return true;
        }
        fn main()->i32 {
            let success=try_value(out let result);
            if success { return result; }
            return 0;
        }
    "#;
    let program=lower(source).unwrap();
    let Item::Function(try_value)=&program.items[0] else {panic!("missing function")};
    assert!(matches!(&try_value.params[0].ty,TypeRef::Reference{mutable:true,inner}
        if matches!(inner.as_ref(),TypeRef::Named(name) if name=="i32")));
    assert!(matches!(&try_value.body[0],Stmt::DerefSet{name,..} if name=="value"));
    let Item::Function(main)=&program.items[1] else {panic!("missing main")};
    assert!(matches!(&main.body[0],Stmt::Let{name,mutable:true,ty:Some(TypeRef::Named(t)),..}
        if name=="result" && t=="i32"));
    assert!(matches!(&main.body[1],Stmt::Let{init:Expr::Call(_,args),..}
        if matches!(args.first(),Some(Expr::AddressOf{mutable:true,value})
            if matches!(value.as_ref(),Expr::Var(name) if name=="result"))));
}

#[test]
fn existing_mutable_and_nova_let() {
    let source=r#"
        fn set(out value:i32)->() { value=9; }
        fn main()->i32 {
            let mut existing:i32=0;
            set(out existing);
            set(out let result);
            return result+existing;
        }
    "#;
    let program=lower(source).unwrap();
    let Item::Function(main)=&program.items[1] else {panic!()};
    assert!(matches!(&main.body[1],Stmt::Expr(Expr::Call(_,args)) if matches!(args.first(),Some(Expr::AddressOf{mutable:true,..}))));
    assert!(matches!(&main.body[2],Stmt::Let{name,..} if name=="result"));
}

#[test]
fn missing_out_at_call_is_error() {
    let err=lower("fn f(out value:i32)->(){value=1;} fn main()->i32{let mut n=0;f(n);return 0;}").unwrap_err();
    assert!(err.contains("NOVA-OUT003"),"{err}");
}
#[test]
fn out_used_on_normal_parameter_is_error() {
    let err=lower("fn f(value:i32)->(){ } fn main()->i32{let mut n=0;f(out n);return 0;}").unwrap_err();
    assert!(err.contains("NOVA-OUT004"),"{err}");
}
#[test]
fn out_must_be_assigned_on_all_return_paths() {
    let err=lower(r#"fn bad(out result:i32, ok:bool)->bool {
        if ok {result=1;return true;}
        return false;
    }"#).unwrap_err();
    assert!(err.contains("NOVA-OUT001"),"{err}");
}
#[test]
fn out_cannot_be_read_before_assignment() {
    let err=lower("fn bad(out n:i32)->i32{return n;}").unwrap_err();
    assert!(err.contains("NOVA-OUT002"),"{err}");
}
#[test]
fn both_branches_must_assign() {
    let good="fn f(out n:i32, flag:bool)->(){if flag{n=1;}else{n=2;}}";
    assert!(lower(good).is_ok());
    let bad="fn f(out n:i32, flag:bool)->(){if flag{n=1;}}";
    assert!(lower(bad).unwrap_err().contains("NOVA-OUT001"));
}
#[test]
fn output_can_be_forwarded_by_reference() {
    let p=lower(r#"
        fn initial(out n:i32)->(){n=33;}
        fn forward(out n:i32)->(){initial(out n);}
        fn main()->i32{let ok:i32=0;forward(out let n);return n;}
    "#).unwrap();
    let Item::Function(forward)=&p.items[1] else {panic!()};
    assert!(matches!(&forward.body[0],Stmt::Expr(Expr::Call(_,args))
        if matches!(args.first(),Some(Expr::Var(n)) if n=="n")));
}
#[test]
fn cannot_infer_unsubstituted_generic_out_type() {
    let err=lower(r#"fn set<T>(out value:T)->(){value=0;}
        fn main()->i32{set(out let v);return 0;}"#).unwrap_err();
    assert!(err.contains("NOVA-OUT010") || err.contains("NOVA-OUT009"),"{err}");
}

#[test]
fn output_survives_typed_pipeline() {
    let source=r#"
        fn fetch(out n:i32)->bool {n=42;return true;}
        fn main()->i32 {
            let yes=fetch(out let n);
            if yes {return n-42;}
            return 1;
        }
    "#;
    let program=grammar::ProgramParser::new().parse(source).unwrap();
    let surface=crate::surface::lower(&program).unwrap();
    let typed=crate::hir::lower(&surface).unwrap();
    let intermediate=crate::mir::lower(&typed).unwrap();
    let llvm=crate::codegen::generate(&intermediate);
    assert!(llvm.contains("@fetch"));
}

#[test]
fn contextual_keyword_does_not_break_existing_out_identifiers() {
    let source=r#"
        extern fn time(out: *mut i64) -> i64;
        fn append(out: i32, value: i32)->i32 {return out+value;}
        fn main()->i32{return append(1, 2)-3;}
    "#;
    assert!(grammar::ProgramParser::new().parse(source).is_ok());
}
