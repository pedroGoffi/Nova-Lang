//! Nova 0.8.2: C ABI varargs and embedded nested libc modules.
use crate::{codegen, grammar, hir, mir, package, strip_comments, surface};
use std::path::Path;

fn compile(source: &str) -> Result<String, String> {
    let ast = grammar::ProgramParser::new()
        .parse(&strip_comments(source))
        .map_err(|err| format!("syntax: {err}"))?;
    let surface = surface::lower(&ast)?;
    let hir = hir::lower(&surface)?;
    let mir = mir::lower(&hir)?;
    Ok(codegen::generate(&mir))
}

#[test]
fn c_varargs_have_actual_llvm_c_abi_and_default_promotions() {
    let ir = compile(
        r#"
      namespace std {namespace libc {namespace stdio {
         [[unsafe]] extern fn printf(fmt:str, ...) -> i32;
      }}}
      fn main() -> i32 {
         let small:u8 = 7;
         let single:f32 = 1.25;
         let number:i64 = 100;
         unsafe {std::libc::stdio::printf("%d %f %ld\n", small, single, number);}
         return 0;
      }
    "#,
    )
    .unwrap();
    assert!(ir.contains("declare i32 @printf(ptr, ...)"), "{ir}");
    assert!(ir.contains("zext i8") && ir.contains("to i32"), "{ir}");
    assert!(
        ir.contains("fpext float") && ir.contains("to double"),
        "{ir}"
    );
    assert!(ir.contains("call i32 (ptr, ...) @printf"), "{ir}");
    assert!(!ir.contains("@std__libc__stdio__printf"), "{ir}");
}

#[test]
fn c_vararg_unsafe_is_required() {
    let err = compile(
        r#"
      namespace std {namespace libc {namespace stdio {
         [[unsafe]] extern fn printf(fmt:str, ...) -> i32;
      }}}
      fn main() -> i32 {
         std::libc::stdio::printf("%d", 7);
         return 0;
      }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("unsafe"), "{err}");
}

#[test]
fn c_vararg_rejects_aggregate_argument() {
    let err = compile(
        r#"
      namespace std {namespace libc {namespace stdio {
         [[unsafe]] extern fn printf(fmt:str, ...) -> i32;
      }}}
      struct Point {x:i64, y:i64}
      fn main() -> i32 {
         let point:Point=new Point{x:1,y:2};
         unsafe {std::libc::stdio::printf("%s", point);}
         return 0;
      }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("ABI scalar") || err.contains("C vararg"),
        "{err}"
    );
}

#[test]
fn c_varargs_accept_checked_mutable_references_as_native_pointers() {
    let ir = compile(
        r#"
        [[unsafe]] extern fn sscanf(input:str, format:str, ...) -> i32;
        fn main() -> i32 {
            unsafe {
                let mut number:i32=0;
                let found:i32=sscanf("123", "%d", &mut number);
                return found;
            }
        }
    "#,
    )
    .unwrap();
    assert!(ir.contains("declare i32 @sscanf(ptr, ptr, ...)"), "{ir}");
    assert!(ir.contains("call i32 (ptr, ptr, ...) @sscanf"), "{ir}");
}

#[test]
fn empty_c_ellipsis_rejected() {
    let err = compile("extern fn no_fixed(...) -> i32; fn main()->i32 {return 0;}").unwrap_err();
    assert!(err.contains("fixed parameter"), "{err}");
}

#[test]
fn embedded_nested_std_links_to_real_libc_symbols() {
    let (program, _sources) = package::load_detailed(Path::new("examples/packages/libc_demo"))
        .expect("Nova.toml and std modules must load");
    let linked = surface::lower(&program).expect("resolve libc module public externs");
    let typed = hir::lower(&linked).expect("type-check libc and std::io pack");
    let mir = mir::lower(&typed).expect("lower libc to MIR");
    let ir = codegen::generate(&mir);
    assert!(ir.contains("declare i32 @printf(ptr, ...)"), "{ir}");
    assert!(ir.contains("declare i64 @strlen(ptr)"), "{ir}");
    assert!(ir.contains("@std__io__printf_i64__pack_2"), "{ir}");
    assert!(ir.contains("call i32 (ptr, ...) @printf"), "{ir}");
}

#[test]
fn c_ellipsis_must_be_last() {
    let err =
        compile("extern fn wrong(fmt:str, ..., value:i32) -> i32; fn main()->i32 {return 0;}")
            .unwrap_err();
    assert!(err.contains("ellipsis must appear once and last"), "{err}");
}

#[test]
fn bool_is_promoted_to_c_int() {
    let ir = compile(
        r#"
      [[unsafe]] extern fn printf(fmt:str, ...) -> i32;
      fn main()->i32 { unsafe { printf("%d", true); } return 0; }
    "#,
    )
    .unwrap();
    assert!(ir.contains("zext i1") && ir.contains("to i32"), "{ir}");
}

#[test]
fn annotated_libc_functions_need_unsafe() {
    let err = compile(
        r#"
      namespace std {namespace libc {namespace string {
         [[unsafe]] extern fn strlen(text:str)->usize;
      }}}
      fn main()->i32 {
        let n:usize=std::libc::string::strlen("Nova");
        return 0;
      }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("unsafe"), "{err}");
}

#[test]
fn native_thread_callback_function_pointer_is_c_abi_safe() {
    let ir = compile(
        r#"
        [[unsafe]] extern fn native_thread_start(callback: fn() -> ()) -> *mut i64;
        [[unsafe]] extern fn native_thread_join(handle: *mut i64) -> ();
        fn worker() -> () { return; }
        fn main() -> i32 {
            unsafe {
                let handle: *mut i64 = native_thread_start(worker);
                native_thread_join(handle);
            }
            return 0;
        }
    "#,
    )
    .expect("function pointer should be accepted as a pointer-sized C callback");
    assert!(ir.contains("declare ptr @native_thread_start(ptr)"), "{ir}");
    assert!(ir.contains("call ptr @native_thread_start(ptr"), "{ir}");
}

#[test]
fn c_callback_with_aggregate_parameter_is_rejected() {
    let err = compile(
        r#"
        struct Payload { value: i64 }
        [[unsafe]] extern fn install_callback(callback: fn(Payload) -> ()) -> ();
        fn main() -> i32 { return 0; }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("ABI") && err.contains("aggregate"), "{err}");
}
