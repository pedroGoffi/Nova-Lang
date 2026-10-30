use std::{path::Path, process::Command};

fn nova(mode: &str, package: &str) -> std::process::Output {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg(mode)
        .arg(root.join("examples/packages").join(package))
        .current_dir(root)
        .output()
        .expect("cannot execute Nova compiler")
}

#[test]
fn nexoframe_060_ui_and_validation_typecheck() {
    let output = nova("check", "nexoframe_060_sanity");
    assert!(
        output.status.success(),
        "typecheck failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn nexoframe_060_ui_and_validation_runtime() {
    let output = nova("run", "nexoframe_060_sanity");
    assert!(
        output.status.success(),
        "runtime failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("NexoFrame 0.60 ownership OK"),
        "unexpected stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
