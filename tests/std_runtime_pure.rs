use std::{path::Path, process::Command};

#[test]
fn nova_std_algorithms_work_without_protocol_code_in_c() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let package = root.join("examples/packages/nova_runtime_pure_029");
    for command in ["check", "build", "run"] {
        let output = Command::new(env!("CARGO_BIN_EXE_nova"))
            .arg(command)
            .arg(&package)
            .current_dir(root)
            .output()
            .expect("launch Nova compiler");
        assert!(output.status.success(), "Nova {command} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr));
    }
}
