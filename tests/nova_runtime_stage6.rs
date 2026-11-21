use std::{path::Path, process::Command};

#[test]
fn builder_math_and_conversion_are_nova_not_c() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let package = root.join("examples/packages/nova_native_min_030");
    for action in ["check", "build", "run"] {
        let result = Command::new(env!("CARGO_BIN_EXE_nova"))
            .arg(action)
            .arg(&package)
            .current_dir(root)
            .output()
            .expect("failed to execute Nova compiler");
        assert!(
            result.status.success(),
            "Nova {action} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn managed_builder_kind_is_not_required_in_c() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let runtime = std::fs::read_to_string(root.join("runtime/nova_rt.c")).unwrap();
    assert!(!runtime.contains("__nova_std__old__builder____"));
    assert!(!runtime.contains("BUILDER,"));
    let native = std::fs::read_to_string(root.join("std/src/builder.nova")).unwrap();
    assert!(!native.contains("pub extern fn"));
    assert!(native.contains("pub fn native_finish("));
}
