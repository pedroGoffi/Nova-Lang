use std::{path::Path, process::Command};

#[test]
fn unicode_character_literals_reach_native_execution() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let project = root.join("examples/packages/nova_char_027");
    for action in ["check", "run"] {
        let output = Command::new(env!("CARGO_BIN_EXE_nova"))
            .args([action])
            .arg(&project)
            .current_dir(root)
            .output()
            .expect("launch Nova character integration example");
        assert!(
            output.status.success(),
            "Nova {action} character example failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
