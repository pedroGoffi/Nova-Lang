//! Checks the compile-time side of JSON derives without invoking a native JSON runtime.
use crate::{ast, grammar, proc_macros, strip_comments};

const META: &str = r#"
namespace std { namespace meta {
  struct MetaType {} struct MetaField {} struct MetaFields {}
  struct MetaTypeRef {} struct MetaExpr {} struct MetaDecl {}
  #[comptime] #[meta_proc_intrinsic(fields)]
  fn fields(t:MetaType)->MetaFields{return 0;}
  #[comptime] #[meta_proc_intrinsic(field_name)]
  fn field_name(f:MetaField)->str{return "";}
  #[comptime] #[meta_proc_intrinsic(field_type_ref)]
  fn field_type_ref(f:MetaField)->MetaTypeRef{return 0;}
  #[comptime] #[meta_proc_intrinsic(type_ref)]
  fn type_ref(t:MetaType)->MetaTypeRef{return 0;}
  #[comptime] #[meta_proc_intrinsic(type_named)]
  fn type_named(t:str)->MetaTypeRef{return 0;}
  #[comptime] #[meta_proc_intrinsic(type_apply2)]
  fn type_apply2(n:str,t:MetaTypeRef,e:MetaTypeRef)->MetaTypeRef{return 0;}
  #[comptime] #[meta_proc_intrinsic(variable)]
  fn variable(n:str)->MetaExpr{return 0;}
  #[comptime] #[meta_proc_intrinsic(str_expr)]
  fn str_expr(t:str)->MetaExpr{return 0;}
  #[comptime] #[meta_proc_intrinsic(call2)]
  fn call2(n:str,a:MetaExpr,b:MetaExpr)->MetaExpr{return 0;}
  #[comptime] #[meta_proc_intrinsic(call)]
  fn call(n:str,a:MetaExpr)->MetaExpr{return 0;}
  #[comptime] #[meta_proc_intrinsic(try_expr)]
  fn try_expr(a:MetaExpr)->MetaExpr{return 0;}
  #[comptime] #[meta_proc_intrinsic(struct_init)]
  fn struct_init(t:MetaType)->MetaExpr{return 0;}
  #[comptime] #[meta_proc_intrinsic(init_field)]
  fn init_field(a:MetaExpr,n:str,b:MetaExpr)->MetaExpr{return 0;}
  #[comptime] #[meta_proc_intrinsic(function)]
  fn function(n:str,t:MetaTypeRef,e:MetaExpr)->MetaDecl{return 0;}
  #[comptime] #[meta_proc_intrinsic(parameter)]
  fn parameter(d:MetaDecl,n:str,t:MetaTypeRef)->MetaDecl{return 0;}
  #[comptime] #[meta_proc_intrinsic(static_method)]
  fn static_method(d:MetaDecl)->MetaDecl{return 0;}
  #[comptime] #[meta_proc_intrinsic(impl_for)]
  fn impl_for(t:MetaType)->MetaDecl{return 0;}
  #[comptime] #[meta_proc_intrinsic(method)]
  fn method(d:MetaDecl,m:MetaDecl)->MetaDecl{return 0;}

  #[comptime] #[proc_derive(JsonCheck)]
  pub fn derive_check(target:MetaType)->MetaDecl {
    let mut init=std::meta::struct_init(target);
    for f in std::meta::fields(target) {
      let name=std::meta::field_name(f);
      let value=std::meta::try_expr(std::meta::call2(
        "std::old::json::required_i64",std::meta::variable("source"),std::meta::str_expr(name)));
      init=std::meta::init_field(init,name,value);
    }
    let result=std::meta::type_apply2("std::old::result::Result",
      std::meta::type_ref(target),std::meta::type_named("std::old::json::JsonError"));
    let mut fn_decl=std::meta::function("from_json",result,std::meta::call("Ok",init));
    fn_decl=std::meta::parameter(fn_decl,"source",std::meta::type_named("str"));
    return std::meta::method(std::meta::impl_for(target),std::meta::static_method(fn_decl));
  }
}}
"#;

#[test]
fn derive_builds_static_json_constructor_using_regular_nova_ast() {
    let input = format!("{META}\n#[derive(JsonCheck)] struct Record {{ id:i64, count:i64 }}");
    let parsed = grammar::ProgramParser::new().parse(&strip_comments(&input)).unwrap();
    let expanded = proc_macros::expand(&parsed).expect("derivation must produce a normal impl");
    let functions: Vec<_> = expanded.items.iter().filter_map(|item| match item {
        ast::Item::Impl(imp) => Some(&imp.methods),
        _ => None,
    }).flatten().filter(|f| f.name == "from_json").collect();
    assert_eq!(functions.len(), 1);
    let generated = format!("{:?}",functions[0].body);
    assert!(generated.contains("__nova_internal_try"));
    assert!(generated.contains("required_i64"));
    assert!(generated.contains("id"));
    assert!(generated.contains("count"));
    assert!(!generated.contains("__nova_json_"));
}

#[test]
fn json_derive_uses_no_json_specific_compiler_intrinsics() {
    let source = include_str!("../std/src/meta.nova");
    assert!(source.contains("#[proc_derive(JsonDecode)]"));
    assert!(source.contains("#[proc_derive(JsonEncode)]"));
    assert!(source.contains("generic_call2"));
    assert!(!source.contains("meta_proc_intrinsic(json_parse)"));
}

#[test]
fn derived_json_uses_canonical_owner_for_types_in_other_modules() {
    // The JSON constraint solver receives a fully qualified/mangled T when a
    // DTO is imported from another module. A generated Result<T, JsonError>
    // must use that same owner identity, not the local spelling "Record".
    let source = format!("{META}\nnamespace users {{ #[derive(JsonCheck)] pub struct Record {{ id:i64 }} }}");
    let parsed = grammar::ProgramParser::new().parse(&strip_comments(&source)).unwrap();
    let expanded = proc_macros::expand(&parsed).expect("derive in a nested module");
    let items = expanded.items.iter().find_map(|item|match item {
        ast::Item::Namespace{name,items} if name=="users"=>Some(items),
        _=>None,
    }).expect("users module");
    let decode = items.iter().find_map(|item|match item {
        ast::Item::Impl(block) => block.methods.iter().find(|method| method.name=="from_json"),
        _=>None,
    }).expect("generated from_json");
    assert_eq!(decode.result,ast::TypeRef::Applied("std::old::result::Result".into(),vec![
        ast::TypeRef::Named("users::Record".into()),
        ast::TypeRef::Named("std::old::json::JsonError".into()),
    ]));
    assert!(decode.attributes.iter().any(|a|a.name=="static"));
}

#[test]
fn qualified_json_owner_preserves_generic_arguments() {
    let source = format!("{META}\nnamespace users {{ #[derive(JsonCheck)] struct Record<T> {{ id:i64 }} }}");
    let parsed = grammar::ProgramParser::new().parse(&strip_comments(&source)).unwrap();
    let expanded = proc_macros::expand(&parsed).expect("generic DTO derive");
    let items = expanded.items.iter().find_map(|item|match item {
        ast::Item::Namespace{name,items} if name=="users"=>Some(items),
        _=>None,
    }).expect("users module");
    let decode = items.iter().find_map(|item|match item {
        ast::Item::Impl(block) => block.methods.iter().find(|method| method.name=="from_json"),
        _=>None,
    }).expect("generated from_json");
    assert_eq!(decode.result,ast::TypeRef::Applied("std::old::result::Result".into(),vec![
        ast::TypeRef::Applied("users::Record".into(),vec![ast::TypeRef::Named("T".into())]),
        ast::TypeRef::Named("std::old::json::JsonError".into()),
    ]));
}
