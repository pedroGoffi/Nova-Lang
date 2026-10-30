use std::{path::Path, process::Command};

#[test]
fn shared_std_string_is_identical_across_direct_and_transitive_dependencies() {
    let compiler = env!("CARGO_BIN_EXE_nova");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let project = root.join("examples/packages/shared_dependencies_061");
    let checked = Command::new(compiler)
        .arg("check")
        .arg(&project)
        .current_dir(root)
        .output()
        .expect("unable to launch Nova compiler");
    assert!(
        checked.status.success(),
        "Shared dependency example did not typecheck:\n{}\n{}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr),
    );
    let executed = Command::new(compiler)
        .arg("run")
        .arg(&project)
        .current_dir(root)
        .output()
        .expect("unable to run Nova example");
    assert!(
        executed.status.success(),
        "Shared dependency example did not run:\n{}\n{}",
        String::from_utf8_lossy(&executed.stdout),
        String::from_utf8_lossy(&executed.stderr),
    );
    let output = String::from_utf8_lossy(&executed.stdout);
    assert!(output.contains("Shared STD"), "{output}");
    assert!(output.contains("RAII without text copy"), "{output}");
}
