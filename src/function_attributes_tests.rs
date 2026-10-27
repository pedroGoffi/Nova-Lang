//! Nova 0.15 function effect attributes: parser -> surface -> typed HIR -> MIR -> LLVM.
use crate::{codegen, grammar, hir, mir, strip_comments, surface};

fn compile(source: &str) -> Result<String, String> {
    let program = grammar::ProgramParser::new()
        .parse(&strip_comments(source))
        .map_err(|error| format!("parser: {error:?}"))?;
    let program = surface::lower(&program)?;
    let typed = hir::lower(&program)?;
    let lowered = mir::lower(&typed)?;
    Ok(codegen::generate(&lowered))
}

#[test]
fn unsafe_function_requires_unsafe_call_site() {
    let source = "[[unsafe]] fn read() -> i32 { return 4; } fn main() -> i32 { return read(); }";
    let error = compile(source).unwrap_err();
    assert!(error.contains("unsafe block"), "{error}");
}

#[test]
fn unsafe_function_body_is_unsafe_context() {
    let ir = compile(
        r#"
        [[unsafe]] extern fn raw_alloc(size:u64,alignment:u64)->*mut u8;
        [[unsafe]] fn allocate()->*mut u8 {return raw_alloc(8,8);}
        fn main()->i32 {unsafe {let pointer:*mut u8=allocate();} return 0;}
    "#,
    )
    .unwrap();
    assert!(ir.contains("@raw_alloc"), "{ir}");
}

#[test]
fn unmarked_c_abi_is_not_implicitly_unsafe() {
    let ir = compile(
        r#"
        extern fn strlen(text:str)->usize;
        extern fn printf(fmt:str,...)->i32;
        fn main()->i32 {let size:usize=strlen("nova");return printf("%zu",size);}
    "#,
    )
    .unwrap();
    assert!(
        ir.contains("declare") && ir.contains("@strlen") && ir.contains("@printf"),
        "{ir}"
    );
}

#[test]
fn unsafe_extern_requires_explicit_unsafe_block() {
    let error = compile(
        r#"
        [[unsafe]] extern fn ffi_unsafe(value:i32)->i32;
        fn main()->i32 {return ffi_unsafe(12);}
    "#,
    )
    .unwrap_err();
    assert!(error.contains("unsafe block"), "{error}");
    let ir = compile(
        r#"
        [[unsafe]] extern fn ffi_unsafe(value:i32)->i32;
        fn main()->i32 {unsafe {return ffi_unsafe(12);}}
    "#,
    )
    .unwrap();
    assert!(ir.contains("@ffi_unsafe"), "{ir}");
}

#[test]
fn method_attributes_survive_monomorphization() {
    let error = compile(
        r#"
        struct Device {number:i32}
        impl Device {
           [[unsafe]] fn privileged(self:Device)->i32{return self.number;}
        }
        fn main()->i32 {let d:Device=new Device{number:1};return d.privileged();}
    "#,
    )
    .unwrap_err();
    assert!(error.contains("unsafe block"), "{error}");
    let ir = compile(
        r#"
        struct Device {number:i32}
        impl Device {
           [[unsafe]] fn privileged(self:Device)->i32{return self.number;}
        }
        fn main()->i32 {let d:Device=new Device{number:1};unsafe {return d.privileged();}}
    "#,
    )
    .unwrap();
    assert!(ir.contains("privileged"), "{ir}");
}

#[test]
fn marker_attributes_work_with_pub_and_namespaces() {
    let ir = compile(
        r#"
        namespace api {
           #[unsafe] pub fn restricted()->i32{return 3;}
           pub [[unsafe]] extern fn foreign(value:i32)->i32;
        }
        fn main()->i32 {unsafe {return api::restricted();}}
    "#,
    )
    .unwrap();
    assert!(ir.contains("api__restricted"), "{ir}");
}

#[test]
fn invalid_function_attributes_are_diagnostics() {
    for (code, fragment) in [
        (
            "[[unsafe]] [[unsafe]] fn foo()->i32{return 0;}",
            "duplicate unsafe",
        ),
        (
            "#[unsafe(Extra)] fn foo()->i32{return 0;}",
            "accepts no arguments",
        ),
        (
            "[[some_future_effect]] fn foo()->i32{return 0;}",
            "unsupported function attribute",
        ),
    ] {
        let err = compile(code).unwrap_err();
        assert!(err.contains(fragment), "expected {fragment:?}, got {err}");
    }
}

#[test]
fn unsafe_function_pointer_cannot_discard_unsafe_effect() {
    let err = compile(
        r#"
        [[unsafe]] fn secret()->i32{return 1;}
        fn main()->i32 {let f:fn()->i32=secret;return 0;}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("unsafe function") || err.contains("function pointer"),
        "{err}"
    );
}
