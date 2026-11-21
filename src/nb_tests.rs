//! NB: annotation processing stays in Nova, class/member introspection in the
//! compiler's generic compile-time reflection bridge.
use crate::{ast::{self, Item, Stmt, Expr}, grammar, proc_macros, meta};

fn members<'a>(items:&'a [Item], wanted:&str)->Vec<&'a ast::Function> {
    fn visit<'a>(items:&'a [Item],wanted:&str,functions:&mut Vec<&'a ast::Function>) {
        for item in items {
            match item {
                Item::Namespace{items,..}=>visit(items,wanted,functions),
                Item::Public(inner)=>visit(std::slice::from_ref(inner.as_ref()),wanted,functions),
                Item::Impl(block) if matches!(&block.target,ast::TypeRef::Named(name) if name==wanted)=>{
                    functions.extend(block.methods.iter());
                }
                _=>{}
            }
        }
    }
    let mut found=vec![];visit(items,wanted,&mut found);found
}

#[test]
fn nb_package_generates_service_and_route_methods() {
    let parsed=crate::package::load(std::path::Path::new("examples/packages/nb_demo"))
        .expect("example with local NB dependency parses");
    let expanded=proc_macros::expand(&parsed).expect("Nova-authored NB decorators expand");
    meta::validate(&expanded).expect("field/method markers are declared by NB");
    let service=members(&expanded.items,"UserService");
    assert!(service.iter().any(|m|m.name=="nb_component_kind"),"service methods: {service:?}");
    let controller=members(&expanded.items,"UserController");
    for name in ["nb_component_name","nb_dependency_service","nb_path_get", "nb_verb_get", "nb_path_create", "nb_verb_create"] {
        assert!(controller.iter().any(|m|m.name==name),"missing {name}");
    }
    let getter=controller.iter().find(|m|m.name=="nb_path_get").unwrap();
    assert!(matches!(&getter.body[0],Stmt::Return(Some(Expr::Str(s))) if s=="\"/users/{id}\""));
}

#[test]
fn ordinary_comptime_decorator_can_edit_class_fields() {
    let source=r#"
        namespace std { namespace meta {
            struct MetaType {} struct MetaDecl {}
            #[comptime] #[meta_proc_intrinsic(add_field)] fn add_field(t:MetaType, name:str, ty:str)->MetaDecl{return 0;}
        }}
        pub comptime fn Audited(t:std::meta::MetaType)->std::meta::MetaDecl {
            return t.add_field("created_at","i64");
        }
        @Audited class AuditRecord { id:i64 }
    "#;
    let parsed=grammar::ProgramParser::new().parse(source).unwrap();
    let expanded=proc_macros::expand(&parsed).unwrap();
    let class=expanded.items.iter().find_map(|i|match i {Item::Struct(s) if s.name=="AuditRecord"=>Some(s),_=>None}).unwrap();
    assert_eq!(class.fields.len(),2);
    assert_eq!(class.fields[1].name,"created_at");
}

#[test]
fn decorators_cannot_duplicate_fields() {
    let source=r#"
        namespace std { namespace meta {
            struct MetaType {} struct MetaDecl {}
            #[comptime] #[meta_proc_intrinsic(add_field)] fn add_field(t:MetaType,name:str,ty:str)->MetaDecl{return 0;}
        }}
        comptime fn Audited(t:std::meta::MetaType)->std::meta::MetaDecl {
            return std::meta::add_field(t,"id","i64");
        }
        @Audited struct Record {id:i64}
    "#;
    let parsed=grammar::ProgramParser::new().parse(source).unwrap();
    let err=proc_macros::expand(&parsed).unwrap_err();
    assert!(err.contains("NOVA-PROC012"),"{err}");
}

#[test]
fn literal_arguments_in_decorator_syntax_parse() {
    let input="@NB::RestController(path=\"/v1\") class Foo {} impl Foo {@GetMapping(\"/x\") fn x(self:&Self)->i64 {return 1;}}";
    grammar::ProgramParser::new().parse(input).expect("qualified decorator plus string positional method annotation");
}

#[test]
fn nb_demo_reaches_normal_surface_lowering() {
    let parsed=crate::package::load(std::path::Path::new("examples/packages/nb_demo"))
        .expect("load NB example");
    crate::surface::lower(&parsed).expect("all generated NB code must type-check");
}

#[test]
fn nb_use_glob_import_resolves_annotation_to_package_symbol() {
    let input = "use NB::*; @Service class Example {} impl Example { @GetMapping(\"/v1\") fn get(self:&Self)->i64{return 1;} }";
    grammar::ProgramParser::new().parse(input).expect("use NB::*; parses");
    let parsed=crate::package::load(std::path::Path::new("examples/packages/nb_demo"))
        .expect("modular NB example with use glob imports");
    let mut annotations=vec![];
    fn collect(items:&[Item],out:&mut Vec<String>){
        for i in items {
            match i {
                Item::Namespace{items,..}=>collect(items,out),
                Item::Public(inner)=>collect(std::slice::from_ref(inner.as_ref()),out),
                Item::Struct(s) if s.name=="UserService"=>out.extend(s.attributes.iter().map(|a|a.name.clone())),
                _=>{},
            }
        }
    }
    collect(&parsed.items,&mut annotations);
    assert!(annotations.iter().any(|a|a=="NB::Service"),"{annotations:?}");
    let expanded=proc_macros::expand(&parsed).expect("expanded modular NB");
    meta::validate(&expanded).expect("qualified field and method markers validate");
    let controller=members(&expanded.items,"UserController");
    for name in ["nb_component_kind","nb_controller_prefix","nb_dependency_service",
        "nb_qualifier_service","nb_value_key_region","nb_route_count","nb_scope",
        "nb_path_get","nb_path_create"] {
        assert!(controller.iter().any(|f|f.name==name),"missing {name}");
    }
    let service=members(&expanded.items,"UserService");
    assert!(service.iter().any(|f|f.name=="nb_post_construct"));
    let config=members(&expanded.items,"AppConfig");
    assert!(config.iter().any(|f|f.name=="nb_config_field_count"));
    let factory=members(&expanded.items,"SystemConfiguration");
    assert!(factory.iter().any(|f|f.name=="nb_bean_factory_default_port"));
    crate::surface::lower(&parsed).expect("modular NB example should type-check");
}

// These negative tests load real Nova sources through the package linker.
// No fake AST calls: they cover lexical imports, decorators and processor errors.
fn temporary_nb_example(source:&str)->Result<ast::Program,String> {
    use std::sync::atomic::{AtomicUsize,Ordering};
    static NEXT:AtomicUsize=AtomicUsize::new(0);
    let base=std::env::temp_dir().join(format!("nova_nb_modular_{}_{}",
        std::process::id(),NEXT.fetch_add(1,Ordering::Relaxed)));
    std::fs::create_dir_all(base.join("src")).unwrap();
    let nb_root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packages/NB");
    std::fs::write(base.join("Nova.toml"),format!(
        "[package]\nname = \"nb_example\"\nversion = \"0.1.0\"\nentry = \"src/main.nova\"\n\n[dependencies]\nstd = {{ builtin = \"std\" }}\nNB = {{ path = \"{}\" }}\n",nb_root.display())).unwrap();
    std::fs::write(base.join("src/main.nova"),source).unwrap();
    let result=crate::package::load(&base);
    let _=std::fs::remove_dir_all(&base);
    result
}

#[test]
fn nb_selective_use_and_glob_conflict_are_explicit() {
    let parsed=temporary_nb_example("use NB::Service; @Service class S {} fn main()->i32{return 0;}").unwrap();
    let expanded=proc_macros::expand(&parsed).unwrap();
    assert!(members(&expanded.items,"S").iter().any(|m|m.name=="nb_scope"));
    let err=temporary_nb_example("use NB::*; use NB::Service; fn main()->i32{return 0;}").unwrap_err();
    assert!(err.contains("NOVA-M012"),"{err}");
}

#[test]
fn nb_rejects_unsupported_scope_and_duplicate_mappings() {
    let parsed=temporary_nb_example("use NB::*; @Service(scope=\"request\") class Bad {} fn main()->i32{return 0;}").unwrap();
    let err=proc_macros::expand(&parsed).unwrap_err();
    assert!(err.contains("scope must be singleton or prototype"),"{err}");
    let parsed=temporary_nb_example(r#"
        use NB::*;
        @RestController(path="/api") class C {}
        impl C {
          @GetMapping("/x") fn one(self:&Self)->i64{return 1;}
          @GetMapping("/x") fn two(self:&Self)->i64{return 2;}
        }
        fn main()->i32{return 0;}
    "#).unwrap();
    let err=proc_macros::expand(&parsed).unwrap_err();
    assert!(err.contains("duplicate HTTP verb and path"),"{err}");
}

#[test]
fn nb_rejects_missing_config_prefix_and_double_mapping() {
    let parsed=temporary_nb_example("use NB::*; @ConfigurationProperties class C {} fn main()->i32{return 0;}").unwrap();
    let err=proc_macros::expand(&parsed).unwrap_err();
    assert!(err.contains("expects prefix"),"{err}");
    let parsed=temporary_nb_example(r#"
        use NB::*;
        @RestController(path="/api") class C {}
        impl C {
          @GetMapping("/x") @PostMapping("/x") fn both(self:&Self)->i64{return 1;}
        }
        fn main()->i32{return 0;}
    "#).unwrap();
    let err=proc_macros::expand(&parsed).unwrap_err();
    assert!(err.contains("one HTTP mapping annotation"),"{err}");
}

#[test]
fn configuration_properties_uses_procedural_target_field_count() {
    let parsed = crate::package::load(std::path::Path::new("examples/packages/nb_demo"))
        .expect("load actual NB package for @ConfigurationProperties");
    let expanded = proc_macros::expand(&parsed)
        .expect("field count must be evaluated using MetaType procedural reflection");
    let config = members(&expanded.items, "AppConfig");
    let count = config.iter().find(|method| method.name == "nb_config_field_count")
        .expect("@ConfigurationProperties must generate nb_config_field_count");
    assert!(
        matches!(count.body.as_slice(), [Stmt::Return(Some(Expr::Int(n)))] if n.as_str() == "1"),
        "AppConfig has exactly one field; generated body: {:?}", count.body
    );
}

#[test]
fn nb_borrowed_http_mapping_contract_rejects_wrong_static_callbacks() {
    for signature in [
        "response:i64,request:i64)->()",
        "response:&NB::http::Response,request:&NB::http::Request)->i32",
    ] {
        let result_body=if signature.ends_with("i32") {"return 0;"} else {"return;"};
        let source=format!(r#"
            use NB::*;
            @RestController(path="/bad") class BadController {{}}
            impl BadController {{
                @GetMapping("/") static fn wrong({signature} {{{result_body}}}
            }}
            @BootApplication class Boot {{}}
            impl Boot {{static fn main()->i32 {{return 0;}}}}
            fn main()->i32 {{return 0;}}
        "#);
        let parsed=temporary_nb_example(&source).expect("load malformed HTTP callback fixture");
        let expanded=proc_macros::expand(&parsed).expect("callback shape reaches ordinary typed checks");
        let result=crate::surface::lower(&expanded).and_then(|lowered|crate::hir::lower(&lowered).map(|_|()));
        assert!(result.is_err(),"incompatible callback was accepted: {signature}");
    }
}

#[test]
fn explicit_generic_callback_specialization_preserves_types_and_constraints() {
    fn compile(source:&str)->Result<String,String> {
        let ast=grammar::ProgramParser::new().parse(source).map_err(|error|format!("{error:?}"))?;
        let lowered=crate::surface::lower(&ast)?;
        let hir=crate::hir::lower(&lowered)?;
        let mir=crate::mir::lower(&hir)?;
        Ok(crate::codegen::generate(&mir))
    }
    let source=r#"
        fn identity<T>(value:T)->T {return value;}
        fn apply<T>(value:T)->T {
            let callback:fn(T)->T=function_ref::<T>(identity);
            return callback(value);
        }
        fn main()->i32 {return apply(42)-42;}
    "#;
    let ir=compile(source).expect("generic callback should specialize to a typed Nova function");
    assert!(ir.contains("identity__i32"));
    for source in [
        "fn id<T>(v:T)->T{return v;} fn main()->i32{let f:fn(i64)->i64=function_ref::<i32>(id);return 0;}",
        "fn id<T: integer>(v:T)->T{return v;} fn main()->i32{let f:fn(f64)->f64=function_ref::<f64>(id);return 0;}",
        "fn id<T>(v:T)->T{return v;} fn main()->i32{let f=function_ref::<i32,i64>(id);return 0;}",
        "[[unsafe]] fn id<T>(v:T)->T{return v;} fn main()->i32{let f=function_ref::<i32>(id);return 0;}",
        "[[unsafe]] extern fn foreign(v:i32)->i32; fn main()->i32{let f=function_ref::<i32>(foreign);return 0;}",
    ] {assert!(compile(source).is_err(),"invalid callback was accepted: {source}");}
}
