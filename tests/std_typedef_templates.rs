use std::{fs, path::{Path, PathBuf}, process::Command};

#[test]
fn raw_u64_is_exclusive_to_std_native() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("std/src");
    for entry in fs::read_dir(&root).expect("read standard library") {
        let file = entry.expect("module entry").path();
        if file.extension().is_some_and(|ext| ext == "nova") {
            let source = fs::read_to_string(&file).expect("read module");
            assert!(
                !source.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').any(|token| token == "u64"),
                "primitive ABI type leaked into public module {}",
                file.display()
            );
        }
    }
    let native_types = fs::read_to_string(root.join("native/types.nova")).unwrap();
    assert!(native_types.contains("typedef Handle = u64"));
    assert!(native_types.contains("typedef Size = u64"));
}

#[test]
fn list_and_map_templates_compile_and_execute() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let package = root.join("examples/packages/std_typedef_templates");
    for command in ["check", "build"] {
        let output = Command::new(env!("CARGO_BIN_EXE_nova"))
            .arg(command).arg(&package).current_dir(root)
            .output().expect("launch Nova compiler");
        assert!(output.status.success(), "{command} failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr));
    }
    let app: PathBuf = package.join("target/nova-app");
    let output = Command::new(app).output().expect("run compiled Nova example");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("std typedef + templates: OK"));
}
