//! Regression tests for zero-runtime ergonomic abstractions.
use crate::{ast,grammar,hir,mir,codegen,strip_comments,surface,proc_macros};
fn compile(source:&str)->Result<String,String>{
    let ast=grammar::ProgramParser::new().parse(&strip_comments(source))
        .map_err(|e|format!("{e:?}"))?;
    let expanded=proc_macros::expand(&ast)?;
    let lowered=surface::lower(&expanded)?;
    let hir=hir::lower(&lowered)?;
    let mir=mir::lower(&hir)?;
    Ok(codegen::generate(&mir))
}
#[test]
fn associated_types_are_usable_as_concepts(){
    let llvm=compile(r#"
        trait Sequence { type Item; }
        struct Numbers { count:i64 }
        impl Sequence for Numbers { type Item = i64; }
        concept HasItem<T> = has_associated_type<T,Item>;
        concept YieldsI64<T> = associated_type_is<T,Item,i64>;
        concept RequiresItem<T> = requires(T value) { type T::Item; };
        static_assert(HasItem<Numbers>);
        static_assert(YieldsI64<Numbers>);
        static_assert(RequiresItem<Numbers>);
        fn main()->i32{return 0;}
    "#).unwrap();
    assert!(llvm.contains("@main"));
}
#[test]
fn concept_can_reject_wrong_associated_type(){
    let result=compile(r#"
        trait Sequence { type Item; }
        struct Numbers {count:i64}
        impl Sequence for Numbers { type Item = i64; }
        static_assert(associated_type_is<Numbers,Item,bool>);
        fn main()->i32 {return 0;}
    "#).unwrap_err();
    assert!(result.contains("static_assert failed"),"{result}");
}
#[test]
fn bounds_on_impl_methods_are_checked(){
    let code=r#"
        concept Integral<T> = integer<T>;
        struct Box<T> {value:T}
        impl<T: Integral> Box<T> where integer<T> {
            fn twice(self:&Box<T>)->T {return self.value + self.value;}
        }
        fn main()->i32 {
            let b = new Box<i32> {value:21};
            return b.twice()-42;
        }
    "#;
    assert!(compile(code).unwrap().contains("@main"));
}
#[test]
fn invalid_impl_specialization_is_rejected(){
    let err=compile(r#"
        concept Integral<T> = integer<T>;
        struct Box<T> {value:T}
        impl<T: Integral> Box<T> {
            fn value_twice(self:&Box<T>)->T {return self.value + self.value;}
        }
        fn main()->i32 {
            let b = new Box<f64> {value:2.0};
            return b.value_twice() as i32;
        }
    "#).unwrap_err();
    assert!(err.contains("concept") || err.contains("Integral"),"{err}");
}
#[test]
fn derived_constructor_is_ordinary_static_method(){
    let ast=grammar::ProgramParser::new().parse(r#"
        namespace std {namespace meta {
            struct MetaType {} struct MetaFields {} struct MetaField {} struct MetaTypeRef {} struct MetaExpr {} struct MetaDecl {}
            #[comptime] #[meta_proc_intrinsic(fields)] fn fields(t:MetaType)->MetaFields{return 0;}
            #[comptime] #[meta_proc_intrinsic(field_name)] fn field_name(t:MetaField)->str{return "";}
            #[comptime] #[meta_proc_intrinsic(field_type_ref)] fn field_type_ref(t:MetaField)->MetaTypeRef{return 0;}
            #[comptime] #[meta_proc_intrinsic(type_ref)] fn type_ref(t:MetaType)->MetaTypeRef{return 0;}
            #[comptime] #[meta_proc_intrinsic(impl_for)] fn impl_for(t:MetaType)->MetaDecl{return 0;}
            #[comptime] #[meta_proc_intrinsic(function)] fn function(n:str,t:MetaTypeRef,e:MetaExpr)->MetaDecl{return 0;}
            #[comptime] #[meta_proc_intrinsic(parameter)] fn parameter(d:MetaDecl,n:str,t:MetaTypeRef)->MetaDecl{return 0;}
            #[comptime] #[meta_proc_intrinsic(method)] fn method(d:MetaDecl,m:MetaDecl)->MetaDecl{return 0;}
            #[comptime] #[meta_proc_intrinsic(variable)] fn variable(n:str)->MetaExpr{return 0;}
            #[comptime] #[meta_proc_intrinsic(struct_init)] fn struct_init(t:MetaType)->MetaExpr{return 0;}
            #[comptime] #[meta_proc_intrinsic(init_field)] fn init_field(e:MetaExpr,n:str,v:MetaExpr)->MetaExpr{return 0;}
            #[comptime] #[meta_proc_intrinsic(static_method)] fn static_method(d:MetaDecl)->MetaDecl{return 0;}
            #[comptime] #[proc_derive(Constructor)]
            pub fn derive(t:MetaType)->MetaDecl {
                let mut init = struct_init(t);
                let mut ctor = function("new",type_ref(t),init);
                for field in fields(t) {
                    init=init_field(init,field_name(field),variable(field_name(field)));
                }
                ctor=function("new",type_ref(t),init);
                for field in fields(t) {ctor=parameter(ctor,field_name(field),field_type_ref(field));}
                return method(impl_for(t),static_method(ctor));
            }
        }}
        #[derive(Constructor)] struct Person {id:i64, active:bool}
    "#).unwrap();
    let transformed=proc_macros::expand(&ast).unwrap();
    fn generated(items:&[ast::Item])->bool{items.iter().any(|item|match item {
        ast::Item::Impl(block)=>block.methods.iter().any(|f|f.name=="new" && f.params.len()==2 && f.attributes.iter().any(|a|a.name=="static")),
        _=>false,
    })}
    assert!(generated(&transformed.items));
}

#[test]
fn typedefs_are_transparent_to_compile_time_constraints(){
    let llvm=compile(r#"
        typedef Age=i64;
        typedef OtherAge=Age;
        concept Integer<T>=integer<T>;
        static_assert(Integer<Age>);
        static_assert(same_as<Age,OtherAge>);
        static_assert(copyable<Age>);
        fn main()->i32{return 0;}
    "#).unwrap();
    assert!(llvm.contains("@main"));
}
#[test]
fn incomplete_associated_type_implementation_is_rejected(){
    let error=compile(r#"
        trait Values {type Item;type Error;}
        struct Reader {offset:i64}
        impl Values for Reader {type Item=i64;}
        fn main()->i32{return 0;}
    "#).unwrap_err();
    assert!(error.contains("missing associated type 'Error'"),"{error}");
}
#[test]
fn default_constructor_concept_checks_static_new_signature(){
    let llvm=compile(r#"
        struct Value {number:i64}
        impl Value {static fn new()->Self {return new Value {number:0};}}
        static_assert(default_constructible<Value>);
        fn main()->i32{return 0;}
    "#).unwrap();
    assert!(llvm.contains("@main"));
}
