use std::{fs, path::Path, process::Command};

#[test]
fn native_entry_passes_argc_without_runtime_adapter() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let project = root.join("examples/packages/entry_args");
    let output = std::env::temp_dir().join(format!("nova_entry_argv_{}", std::process::id()));
    let compile = Command::new(env!("CARGO_BIN_EXE_nova"))
        .args(["build"])
        .arg(&project)
        .args(["-o"])
        .arg(&output)
        .current_dir(root)
        .output()
        .expect("run Nova compiler");
    assert!(compile.status.success(), "build failed: {}", String::from_utf8_lossy(&compile.stderr));
    let success = Command::new(&output).args(["hello", "world"]).status().unwrap();
    assert_eq!(success.code(), Some(0));
    let wrong_count = Command::new(&output).arg("hello").status().unwrap();
    assert_eq!(wrong_count.code(), Some(2));
    let _ = fs::remove_file(&output);
}
