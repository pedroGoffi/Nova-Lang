//! Regression tests for zero-runtime optional operators and compact syntax.
use crate::{ast, codegen, grammar, hir, mir, strip_comments, surface};

fn parsed(source:&str)->ast::Program {
    grammar::ProgramParser::new().parse(&strip_comments(source)).unwrap()
}
fn compile(source:&str)->Result<String,String> {
    let parsed=parsed(source);
    let specialized=surface::lower(&parsed)?;
    let typed=hir::lower(&specialized)?;
    let mir=mir::lower(&typed)?;
    Ok(codegen::generate(&mir))
}
fn option_source(body:&str)->String {
    format!(r#"
        enum Option<T> {{ Some(T), None }}
        fn fetch() -> Option<i32> {{return Some(42);}}
        fn main() -> Option<i32> {{ {body} }}
    "#)
}

#[test]
fn optional_propagation_uses_typed_match() {
    let p=parsed(&option_source("let n=fetch()?; return Some(n);"));
    let lowered=crate::ergonomic_lowering::lower(&p).unwrap();
    let function=lowered.items.iter().find_map(|item|match item {
        ast::Item::Function(f) if f.name=="main" => Some(f), _=>None
    }).unwrap();
    assert!(matches!(function.body.first(),Some(ast::Stmt::Match{value:ast::Expr::Call(name,_),arms})
        if name=="fetch" && arms.len()==2 && matches!(arms[0].pattern,ast::Pattern::Variant{..})));
}

#[test]
fn result_error_propagation_reuses_err_value() {
    let p=parsed(r#"
      enum Result<T,E> {Ok(T),Err(E)}
      fn fetch()->Result<i32,i32>{return Ok(42);}
      fn main()->Result<i32,i32>{let n=fetch()?;return Ok(n);}
    "#);
    let lowered=crate::ergonomic_lowering::lower(&p).unwrap();
    let function=lowered.items.iter().find_map(|item|match item {
        ast::Item::Function(f) if f.name=="main" => Some(f), _=>None
    }).unwrap();
    let Some(ast::Stmt::Match{arms,..})=function.body.first() else {panic!("expected match")};
    assert!(matches!(&arms[1].pattern,ast::Pattern::Variant{path,..} if path=="Err"));
    assert!(matches!(&arms[1].body[0],ast::Stmt::Return(Some(ast::Expr::Call(name,_))) if name=="Err"));
}

#[test]
fn try_rejects_incompatible_return_type() {
    let p=parsed("fn fetch()->i32{return 42;} fn main()->i32{let n=fetch()?;return n;}");
    let error=crate::ergonomic_lowering::lower(&p).unwrap_err();
    assert!(error.contains("NOVA-TRY001"),"{error}");
}

#[test]
fn fallback_is_evaluated_only_in_none_arm() {
    let p=parsed(r#"
        enum Option<T>{Some(T),None}
        fn fallback()->i32{return 1;}
        fn main()->i32{
           let value:Option<i32> = Some(42);
           let answer:i32=value ?? fallback();
           return answer-42;
        }
    "#);
    let lowered=crate::ergonomic_lowering::lower(&p).unwrap();
    let function=lowered.items.iter().find_map(|item|match item {
        ast::Item::Function(f) if f.name=="main"=>Some(f),_=>None
    }).unwrap();
    let Some(ast::Stmt::Match{arms,..})=function.body.get(1) else {panic!("expected coalesce match")};
    let look=|stmt:&ast::Stmt| format!("{stmt:?}").contains("fallback");
    assert!(!arms[0].body.iter().any(look),"fallback was made eager");
    assert!(arms[1].body.iter().any(look),"missing lazy fallback");
}

#[test]
fn optional_member_access_is_a_single_match() {
    let p=parsed(r#"
        enum Option<T>{Some(T),None}
        struct User{id:i32}
        fn main()->i32 {
            let user:Option<User> = Some(new User{id:42});
            let result:Option<i32> = user?.id;
            let answer:i32=result ?? 0;
            return answer-42;
        }
    "#);
    let lowered=crate::ergonomic_lowering::lower(&p).unwrap();
    let function=lowered.items.iter().find_map(|item|match item {
        ast::Item::Function(f) if f.name=="main"=>Some(f),_=>None
    }).unwrap();
    assert!(matches!(function.body.get(1),Some(ast::Stmt::Match{arms,..}) if arms.len()==2));
}

#[test]
fn optional_method_skips_arguments_in_none_arm() {
    let p=parsed(r#"
        enum Option<T>{Some(T),None}
        struct User{id:i32}
        impl User{fn add(self:&Self,value:i32)->i32{return self.id+value;}}
        fn expensive()->i32{return 1;}
        fn main()->i32{
            let u:Option<User> = None;
            let answer:Option<i32> = u?.add(expensive());
            return 0;
        }
    "#);
    let lowered=crate::ergonomic_lowering::lower(&p).unwrap();
    let function=lowered.items.iter().find_map(|item|match item {
        ast::Item::Function(f) if f.name=="main"=>Some(f),_=>None
    }).unwrap();
    let Some(ast::Stmt::Match{arms,..})=function.body.get(1) else {panic!("expected match")};
    assert!(!format!("{:?}",arms[1].body).contains("expensive"));
}

#[test]
fn lazy_binary_right_hand_side_remains_conditional() {
    let p=parsed(r#"
        enum Option<T>{Some(T),None}
        fn fallback()->bool{return false;}
        fn main()->i32{
            let a:Option<bool> = Some(true);
            let b:bool=false && (a ?? fallback());
            return 0;
        }
    "#);
    let lowered=crate::ergonomic_lowering::lower(&p).unwrap();
    let function=lowered.items.iter().find_map(|item|match item {
        ast::Item::Function(f) if f.name=="main"=>Some(f),_=>None
    }).unwrap();
    assert!(matches!(function.body.get(1),Some(ast::Stmt::If{..})));
}

#[test]
fn struct_destructuring_evaluates_source_once() {
    let p=parsed(r#"
        struct Point {x:i32,y:i32}
        fn get()->Point{return new Point{x:20,y:22};}
        fn main()->i32{let Point{x,y}=get();return x+y-42;}
    "#);
    let function=p.items.iter().find_map(|item|match item {
        ast::Item::Function(f) if f.name=="main"=>Some(f),_=>None
    }).unwrap();
    assert!(matches!(&function.body[0],ast::Stmt::Let{init:ast::Expr::Call(name,_),..} if name=="get"));
    assert_eq!(function.body.iter().filter(|s|matches!(s,ast::Stmt::Let{..})).count(),3);
}

#[test]
fn destructuring_with_double_underscore_field_is_unambiguous() {
    let p=parsed("struct S{foo__bar:i32} fn main()->i32{let S{foo__bar}=new S{foo__bar:42};return foo__bar-42;}");
    let function=p.items.iter().find_map(|item|match item {
        ast::Item::Function(f) if f.name=="main"=>Some(f),_=>None
    }).unwrap();
    assert!(matches!(&function.body[1],ast::Stmt::Let{name,..} if name=="foo__bar"));
}

#[test]
fn fixed_size_array_destructuring_uses_checked_indices() {
    let p=parsed("fn main()->i32{let (left,right)=[20,22];return left+right-42;}");
    let function=p.items.iter().find_map(|item|match item {
        ast::Item::Function(f) if f.name=="main"=>Some(f),_=>None
    }).unwrap();
    assert!(matches!(&function.body[2],ast::Stmt::Let{init:ast::Expr::Index(_, _),..}));
}

#[test]
fn compact_lambda_result_inferred_from_typed_parameter() {
    let p=parsed("fn main()->i32{let add=|x:i32| x+1;return add(41)-42;}");
    let lifted=crate::lambdas::lift(p).unwrap();
    assert!(lifted.items.iter().any(|item|matches!(item,ast::Item::Function(f)
        if f.name.starts_with("__nova_lambda_") && f.result==ast::TypeRef::Named("i32".into()))));
}

#[test]
fn compact_lambda_reports_ambiguous_return() {
    let p=parsed("fn custom(x:i32)->i32{return x;} fn main()->i32{let f=|x:i32| custom(x);return f(42)-42;}");
    let error=crate::lambdas::lift(p).unwrap_err();
    assert!(error.contains("infer expression lambda"),"{error}");
}

#[test]
fn source_pipeline_stays_runtime_free() {
    let p=parsed(&option_source("let x:i32=fetch() ?? 0;return Some(x);"));
    let expanded=crate::ergonomic_lowering::lower(&p).unwrap();
    assert!(!format!("{expanded:?}").contains("__nova_internal_coalesce"));
    assert!(!format!("{expanded:?}").contains("__nova_internal_try"));
}

#[test]
fn compiler_pipeline_smoke_test() {
    // Execute the full Nova pipeline when Rust/Cargo is available.
    let llvm=compile("fn main()->i32 {let (a,b)=[20,22];return a+b-42;}").unwrap();
    assert!(llvm.contains("@main"));
}

#[test]
fn match_as_expression_reuses_existing_enum_control_flow() {
    let p=parsed(r#"
      enum Choice{First(i32),Second(i32)}
      fn pick()->Choice{return First(40);}
      fn main()->i32{
          let score:i32=match pick() {
              First(n)=>{n+2}
              Second(n)=>{n}
          };
          return score-42;
      }
    "#);
    let lowered=crate::ergonomic_lowering::lower(&p).unwrap();
    let function=lowered.items.iter().find_map(|item|match item {
        ast::Item::Function(f) if f.name=="main"=>Some(f),_=>None
    }).unwrap();
    assert!(matches!(function.body.first(),Some(ast::Stmt::Match{arms,..}) if arms.len()==2));
}
