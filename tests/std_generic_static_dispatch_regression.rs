use std::{path::Path, process::Command};

#[test]
fn generic_type_parameter_dispatches_to_static_method_after_specialization() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let package = root.join("examples/packages/std_generic_static_dispatch_regression");
    for command in ["check", "run"] {
        let result = Command::new(env!("CARGO_BIN_EXE_nova"))
            .arg(command)
            .arg(&package)
            .current_dir(root)
            .output()
            .expect("launch Nova compiler");
        assert!(result.status.success(),
            "Nova {command} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    }
}
