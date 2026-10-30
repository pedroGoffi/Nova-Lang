use std::{path::Path, process::Command};

#[test]
fn nexoframe_049_projects_typecheck() {
    let compiler = env!("CARGO_BIN_EXE_nova");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in ["nexoframe_049_sanity", "nexoframe_049"] {
        let project = root.join("examples/packages").join(name);
        let output = Command::new(compiler)
            .arg("check")
            .arg(&project)
            .current_dir(root)
            .output()
            .expect("could not launch Nova compiler");
        assert!(
            output.status.success(),
            "Nova compiler rejected {name}:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
fn nexoframe_049_runtime_json_and_validation() {
    let compiler = env!("CARGO_BIN_EXE_nova");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let project = root.join("examples/packages/nexoframe_049_sanity");
    let output = Command::new(compiler)
        .arg("run")
        .arg(&project)
        .current_dir(root)
        .output()
        .expect("could not launch Nova example");
    assert!(
        output.status.success(),
        "NexoFrame smoke test failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("NexoFrame 0.49 OK"));
}
