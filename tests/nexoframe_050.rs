use std::{path::Path, process::Command};

#[test]
fn nexoframe_050_macros_and_samples_typecheck() {
    let compiler = env!("CARGO_BIN_EXE_nova");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in ["nexoframe_050_sanity", "nexoframe_050"] {
        let output = Command::new(compiler)
            .arg("check")
            .arg(root.join("examples/packages").join(name))
            .current_dir(root)
            .output()
            .expect("cannot launch Nova compiler");
        assert!(
            output.status.success(),
            "Nova check failed for {name}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn nexoframe_050_json_html_and_macros_smoke() {
    let compiler = env!("CARGO_BIN_EXE_nova");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(compiler)
        .arg("run")
        .arg(root.join("examples/packages/nexoframe_050_sanity"))
        .current_dir(root)
        .output()
        .expect("cannot launch Nova compiler");
    assert!(
        output.status.success(),
        "Nova runtime test failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("NexoFrame 0.50 OK"));
}
