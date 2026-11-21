use std::{path::Path, process::Command};

#[test]
fn signed_loop_indices_converted_to_size_typecheck_and_run() {
    let root=Path::new(env!("CARGO_MANIFEST_DIR"));
    let package=root.join("examples/packages/std_size_indexing_regression");
    for action in ["check", "run"] {
        let result=Command::new(env!("CARGO_BIN_EXE_nova"))
            .arg(action).arg(&package).current_dir(root)
            .output().expect("launch Nova");
        assert!(result.status.success(),
            "Nova {action} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    }
}
