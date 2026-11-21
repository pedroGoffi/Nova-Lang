use std::{path::Path, process::Command};

#[test]
fn std_fs_read_bytes_and_env_arguments_specialize_and_run() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let package = root.join("examples/packages/std_fs_read_bytes_regression");
    for command in ["check", "build", "run"] {
        let output = Command::new(env!("CARGO_BIN_EXE_nova"))
            .arg(command)
            .arg(&package)
            .current_dir(root)
            .output()
            .expect("launch Nova compiler");
        assert!(
            output.status.success(),
            "Nova {command} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
