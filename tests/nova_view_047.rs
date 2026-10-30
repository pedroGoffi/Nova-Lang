use std::{path::Path, process::Command};

#[test]
fn nova_view_047_components_and_dashboard_typecheck() {
    let compiler = env!("CARGO_BIN_EXE_nova");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for example in ["nova_view_047", "nova_web_047"] {
        let project = root.join("examples").join("packages").join(example);
        let output = Command::new(compiler)
            .arg("check")
            .arg(&project)
            .current_dir(root)
            .output()
            .expect("could not launch Nova compiler");
        assert!(
            output.status.success(),
            "Nova compiler rejected {example}:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
