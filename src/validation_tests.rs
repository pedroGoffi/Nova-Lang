use crate::{ast::{Expr,Item,Stmt,TypeRef},grammar,proc_macros,meta};

const DEFINITIONS:&str=r#"
namespace std { namespace meta {
    struct MetaField {} struct MetaArgs {} struct MetaExpr {}
    #[comptime] #[meta_proc_intrinsic(field_name)] fn field_name(f:MetaField)->str{return "";}
    #[comptime] #[meta_proc_intrinsic(field_type_name)] fn field_type_name(f:MetaField)->str{return "";}
    #[comptime] #[meta_proc_intrinsic(variable)] fn variable(n:str)->MetaExpr{return 0;}
    #[comptime] #[meta_proc_intrinsic(access)] fn access(v:MetaExpr,n:str)->MetaExpr{return 0;}
    #[comptime] #[meta_proc_intrinsic(int_expr)] fn int_expr(n:i64)->MetaExpr{return 0;}
    #[comptime] #[meta_proc_intrinsic(binary)] fn binary(o:str,a:MetaExpr,b:MetaExpr)->MetaExpr{return 0;}
    #[comptime] #[meta_proc_intrinsic(has_arg)] fn has_arg(a:MetaArgs,n:str)->bool{return false;}
    #[comptime] #[meta_proc_intrinsic(int_arg)] fn int_arg(a:MetaArgs,n:str)->i64{return 0;}
    #[comptime] #[meta_proc_intrinsic(require)] fn require(v:bool,m:str)->bool{return false;}
    #[comptime] #[meta_proc_intrinsic(argument_count)] fn argument_count(a:MetaArgs)->i64{return 0;}
    #[comptime] #[meta_proc_intrinsic(bool_expr)] fn bool_expr(b:bool)->MetaExpr{return 0;}
    #[comptime] #[meta_proc_intrinsic(string_arg)] fn string_arg(a:MetaArgs,n:str)->str{return "";}
    #[comptime] #[meta_proc_intrinsic(str_expr)] fn str_expr(s:str)->MetaExpr{return 0;}
}}
// User-defined validators in the package itself.
    #[comptime] #[proc_field_validator(Range)]
    pub fn range_validator(field:std::meta::MetaField,args:std::meta::MetaArgs)->std::meta::MetaExpr {
        std::meta::require(std::meta::field_type_name(field)=="i64","expected i64");
        let min=std::meta::int_arg(args,"min");
        let max=std::meta::int_arg(args,"max");
        std::meta::require(min<=max,"bad bounds");
        let field_value=std::meta::access(std::meta::variable("self"),std::meta::field_name(field));
        return std::meta::binary("&&",std::meta::binary(">=",field_value,std::meta::int_expr(min)),
             std::meta::binary("<=",field_value,std::meta::int_expr(max)));
    }
    #[comptime] #[proc_field_validator(Greater)]
    pub fn greater_validator(field:std::meta::MetaField,args:std::meta::MetaArgs)->std::meta::MetaExpr {
        return std::meta::binary(">",std::meta::access(std::meta::variable("self"),std::meta::field_name(field)),std::meta::int_expr(std::meta::int_arg(args,"min")));
    }
    #[comptime] #[proc_field_validator(Equals)]
    pub fn equals_validator(field:std::meta::MetaField,args:std::meta::MetaArgs)->std::meta::MetaExpr {
        let value=std::meta::access(std::meta::variable("self"),std::meta::field_name(field));
        return std::meta::binary("==",value,std::meta::str_expr(std::meta::string_arg(args,"value")));
    }
    #[meta_attribute(struct)] pub trait Service {}
"#;

fn expand(source:&str)->Result<crate::ast::Program,String>{
    let source_with_definitions=format!("{DEFINITIONS}\n{source}");
    let normalized=crate::strip_comments(&source_with_definitions);
    let program=grammar::ProgramParser::new().parse(&normalized)
        .map_err(|e|format!("{e:?}"))?;
    let result=proc_macros::expand(&program)?;
    meta::validate(&result)?;
    Ok(result)
}
fn find_owner<'a>(items:&'a [Item],owner:&str)->Option<&'a crate::ast::Struct>{
    for item in items {
        match item {
            Item::Namespace{items,..}=>if let Some(s)=find_owner(items,owner){return Some(s)},
            Item::Struct(s) if s.name==owner=>return Some(s),
            _=>{}
        }
    }
    None
}
fn find_validate<'a>(items:&'a [Item])->Option<&'a crate::ast::Function>{
    for item in items {
        match item {
            Item::Namespace{items,..}=>if let Some(f)=find_validate(items){return Some(f)},
            Item::Impl(i) if i.methods.iter().any(|m|m.name=="validate")=>return i.methods.iter().find(|m|m.name=="validate"),
            _=>{}
        }
    }
    None
}
#[test]
fn new_annotations_are_defined_in_nova(){
    let ast=expand("struct Foo {a: @Range(min=10,max=20) i64, b: @Greater(min=0) i64}").unwrap();
    let decl=find_owner(&ast.items,"Foo").unwrap();
    assert!(decl.fields.iter().all(|f|f.attributes.is_empty()));
    let generated=find_validate(&ast.items).unwrap();
    assert_eq!(generated.body.len(),3);
    assert!(matches!(&generated.result,TypeRef::Applied(n,_) if n=="std::old::result::Result"));
    assert!(matches!(&generated.body[0],Stmt::If{condition:Expr::Unary(..),..}));
}
#[test]
fn invalid_range_is_reported_by_nova_authored_code(){
    let error=expand("struct Foo {a: @Range(min=20,max=10) i64}").unwrap_err();
    assert!(error.contains("bad bounds"),"{error}");
}
#[test]
fn invalid_type_is_reported_by_nova_authored_code(){
    let error=expand("struct Foo {a: @Range(min=1,max=5) str}").unwrap_err();
    assert!(error.contains("expected i64"),"{error}");
}
#[test]
fn missing_and_duplicate_args_are_rejected(){
    assert!(expand("struct Foo {a: @Range(min=1) i64}").unwrap_err().contains("required argument"));
    assert!(expand("struct Foo {a: @Range(min=1,min=2,max=3) i64}").unwrap_err().contains("duplicate argument"));
}
#[test]
fn service_is_nova_metadata_not_a_compiler_builtin(){
    assert!(expand("@Service class Foo {}").is_ok());
    assert!(expand("@Unknown class Foo {}").unwrap_err().contains("NOVA-META009"));
}
#[test]
fn plain_field_annotations_can_be_meta_only(){
    let source=format!("{DEFINITIONS}\n#[meta_attribute(field)] trait Sensitive {{}} struct Foo {{token: @Sensitive str}}");
    let normalized=crate::strip_comments(&source);
    let ast=grammar::ProgramParser::new().parse(&normalized).unwrap();
    let expanded=proc_macros::expand(&ast).unwrap();
    assert!(meta::validate(&expanded).is_ok());
}
#[test]
fn custom_method_conflict_is_reported(){
    let err=expand("struct Foo {a: @Range(min=1,max=5) i64} impl Foo {fn validate(self:&Self)->bool{return true;}}").unwrap_err();
    assert!(err.contains("NOVA-VAL002"),"{err}");
}
#[test]
fn field_markers_require_valid_definitions(){
    assert!(expand("struct Foo {a: @NotDeclared i64}").unwrap_err().contains("NOVA-META009"));
}

#[test]
fn string_valued_annotation_is_implemented_in_nova(){
    let ast=expand("struct Foo {role: @Equals(value = \"admin\") str}").unwrap();
    assert_eq!(find_validate(&ast.items).unwrap().body.len(),2);
}

#[test]
fn embedded_std_validation_project_reaches_surface_lowering(){
    let program=crate::package::load(std::path::Path::new("examples/packages/custom_annotations"))
        .expect("load Nova package with embedded std::old::validation");
    crate::surface::lower(&program)
        .expect("expand user-defined and stdlib validators");
}
