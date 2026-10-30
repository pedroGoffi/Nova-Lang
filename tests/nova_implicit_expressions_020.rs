use std::{path::Path,process::Command};

#[test]
fn implicit_if_and_block_values_execute() {
    let root=Path::new(env!("CARGO_MANIFEST_DIR"));
    let output=Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("run")
        .arg(root.join("examples/packages/nova_implicit_expressions_020"))
        .current_dir(root).output().expect("Nova compiler not available");
    assert!(output.status.success(),"if/tail value regressions:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
}
