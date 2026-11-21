//! Integration tests for compiler-only ergonomics; no owned runtime objects.
use crate::{ast,codegen,grammar,hir,mir,proc_macros,strip_comments,surface};
fn compile(source:&str)->Result<String,String>{
    let parsed=grammar::ProgramParser::new().parse(&strip_comments(source))
        .map_err(|error|format!("parse: {error:?}"))?;
    let expanded=proc_macros::expand(&parsed)?;
    let typed=surface::lower(&expanded)?;
    let hir=hir::lower(&typed)?;
    let mir=mir::lower(&hir)?;
    Ok(codegen::generate(&mir))
}
#[test]
fn init_shorthand_and_default_fields(){
    let result=compile(r#"
      struct Product { id:i32, price:i32=10, active:bool=true }
      fn main()->i32 {
        let id:i32=32;
        let product=new Product {id};
        if product.active {return product.id+product.price-42;}
        return 1;
      }
    "#).unwrap();
    assert!(result.contains("@main"));
}
#[test]
fn duplicate_unknown_and_missing_initializers_are_errors(){
    for (source, expected) in [
        ("struct S {a:i32} fn main()->i32{let s=new S{a:1,a:2};return 0;}","duplicate field"),
        ("struct S {a:i32} fn main()->i32{let s=new S{q:1};return 0;}","unknown field"),
        ("struct S {a:i32} fn main()->i32{let s=new S{};return 0;}","missing field"),
    ] {
        let error=compile(source).unwrap_err();
        assert!(error.contains(expected),"{error}");
    }
}
#[test]
fn infer_struct_generic_types_from_supplied_fields(){
    let result=compile(r#"
      struct Pair<T,U> {first:T,second:U}
      fn main()->i32 {
        let pair=new Pair {first:32,second:true};
        if pair.second {return pair.first-32;} return 1;
      }
    "#).unwrap();
    assert!(result.contains("@main"));
}
#[test]
fn mismatched_generic_fields_fail_with_useful_error(){
    let error=compile(r#"
      struct Pair<T> {left:T,right:T}
      fn main()->i32 {let p=new Pair {left:1,right:true};return 0;}
    "#).unwrap_err();
    assert!(error.contains("cannot infer") || error.contains("inferred"),"{error}");
}
#[test]
fn pipeline_is_left_associative_with_first_arg_injection(){
    let parsed=grammar::ExprParser::new().parse("21 |> twice |> plus(0)").unwrap();
    assert!(matches!(parsed,ast::Expr::Call(name,args) if name=="plus" &&
        matches!(&args[0],ast::Expr::Call(inner,_) if inner=="twice")));
    let result=compile(r#"
      fn twice(n:i32)->i32{return n*2;}
      fn plus(n:i32,offset:i32)->i32{return n+offset;}
      fn main()->i32{return (21 |> twice |> plus(0))-42;}
    "#).unwrap();
    assert!(result.contains("@main"));
}

#[test]
fn list_comprehension_lowers_to_typed_collection_pipeline(){
    let parsed=grammar::ExprParser::new().parse("[x + 1 for (x: i32) in values if x > 0]").unwrap();
    let debug=format!("{parsed:?}");
    assert!(debug.contains("std::old::algorithm::comprehend"),"{debug}");
}
#[test]
fn if_let_and_while_let_use_existing_match_lowering(){
    let result=compile(r#"
      enum Maybe { Some(i32), None }
      fn main()->i32 {
        let mut current:Maybe=Some(4);
        let mut total:i32=0;
        while let Some(value)=current {
          total+=value;
          current=None;
        }
        if let None=current { return total-4; } else {return 1;}
      }
    "#).unwrap();
    assert!(result.contains("@main"));
}
#[test]
fn computed_properties_are_ordinary_static_typed_methods(){
    let result=compile(r#"
      struct Customer { age:i32 }
      impl Customer {property adult:bool { get => self.age >= 18; }}
      fn main()->i32 {
        let customer=new Customer {age:21};
        if customer.adult {return 0;}
        return 1;
      }
    "#).unwrap();
    assert!(result.contains("@main"));
}
#[test]
fn requires_static_new_signature_is_checked(){
    let result=compile(r#"
      struct User {id:i32}
      impl User {static fn new(id:i32)->Self {return new User{id};}}
      concept Factory<T> = requires(T value) {{ T::new(i32) } -> T;};
      static_assert(Factory<User>);
      fn main()->i32 {return 0;}
    "#).unwrap();
    assert!(result.contains("@main"));
}
#[test]
fn requires_static_signature_rejects_wrong_parameter_type(){
    let error=compile(r#"
      struct User {id:i32}
      impl User {static fn new(id:i32)->Self {return new User{id};}}
      concept BadFactory<T> = requires(T value) {{ T::new(bool) } -> T;};
      static_assert(BadFactory<User>);
      fn main()->i32 {return 0;}
    "#).unwrap_err();
    assert!(error.contains("static_assert"),"{error}");
}
#[test]
fn computed_property_setter_reuses_mutable_nova_method(){
    let llvm=compile(r#"
        struct Account { balance:i32 }
        impl Account {
            property available:i32 {get => self.balance;}
            fn set_available(self:&mut Account,value:i32)->() {
                self.balance=value;
                return;
            }
        }
        fn main()->i32 {
            let mut account=new Account{balance:1};
            account.available=30;
            account.available+=12;
            return account.available-42;
        }
    "#).unwrap();
    assert!(llvm.contains("@main"));
}
