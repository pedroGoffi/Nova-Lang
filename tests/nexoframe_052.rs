use std::{path::Path, process::Command};

fn run_nova(subcommand: &str, package: &str) -> std::process::Output {
    let compiler = env!("CARGO_BIN_EXE_nova");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    Command::new(compiler)
        .arg(subcommand)
        .arg(root.join("examples/packages").join(package))
        .current_dir(root)
        .output()
        .expect("failed to launch Nova compiler")
}

#[test]
fn nexoframe_052_packages_resolve_and_typecheck() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        root.join("packages/nexoframe/Nova.toml").is_file(),
        "NexoFrame manifest missing: install the complete 0.52 archive in the repository root"
    );
    for name in ["nexoframe_052_sanity", "nexoframe_052"] {
        let output = run_nova("check", name);
        assert!(
            output.status.success(),
            "Nova check failed for {name}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn nexoframe_052_macro_smoke() {
    let output = run_nova("run", "nexoframe_052_sanity");
    assert!(
        output.status.success(),
        "Nova runtime failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("NexoFrame 0.52 DX OK"));
}
