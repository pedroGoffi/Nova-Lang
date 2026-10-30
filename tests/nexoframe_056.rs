use std::{path::Path, process::Command};

#[test]
fn nexoframe_validation_056_check_and_run() {
    let compiler = env!("CARGO_BIN_EXE_nova");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let example = root.join("examples/packages/nexoframe_056_sanity");
    for command in ["check", "run"] {
        let output = Command::new(compiler)
            .arg(command)
            .arg(&example)
            .current_dir(root)
            .output()
            .expect("cannot launch Nova compiler");
        assert!(
            output.status.success(),
            "Nova {command} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if command == "run" {
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("NexoFrame 0.56 OK"),
                "unexpected output: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }
}

#[test]
fn nexoframe_validation_has_no_missing_novaview_dependency() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("packages/nexoframe/src/validation.nova");
    let source = std::fs::read_to_string(path).expect("NexoFrame validation source missing");
    assert!(!source.contains("nova_view::validation::"));
}
