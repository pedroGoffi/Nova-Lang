use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bootstrap(root: &Path) -> PathBuf {
    let output = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("bootstrap-selfhost").current_dir(root)
        .output().expect("stage-0 compiler");
    assert!(output.status.success(), "bootstrap failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));
    root.join("selfhost/target/nova-selfhost")
}
fn execute(binary: &Path, root: &Path, project: &str) -> Output {
    Command::new(binary).arg("--check-project-links").arg(project)
        .env_remove("NOVA_SOURCE").current_dir(root)
        .output().expect("execute Nova linker")
}
fn result_text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr))
}
fn fixture(name: &str) -> String {
    format!("selfhost/tests/project_040/{name}/Nova.toml")
}

#[test]
fn project_040_accepts_public_qualified_calls() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let binary = bootstrap(root);
    let output = execute(&binary,root,&fixture("public_calls"));
    assert!(output.status.success(), "{}", result_text(&output));
    assert!(result_text(&output).contains("Project links OK:"));
    assert!(result_text(&output).contains("Modules parsed: 2"));
}

#[test]
fn project_040_rejects_missing_qualified_function() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = execute(&bootstrap(root),root,&fixture("missing_call"));
    assert!(!output.status.success(), "{}", result_text(&output));
    assert!(result_text(&output).contains("NOVA-SELFHOST-L001"), "{}", result_text(&output));
    assert!(result_text(&output).contains("crate::math::missing"));
}

#[test]
fn project_040_rejects_private_function_from_another_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = execute(&bootstrap(root),root,&fixture("private_function"));
    assert!(!output.status.success(), "{}", result_text(&output));
    assert!(result_text(&output).contains("NOVA-SELFHOST-L002"), "{}", result_text(&output));
}

#[test]
fn project_040_rejects_private_nested_module_from_sibling() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = execute(&bootstrap(root),root,&fixture("private_module"));
    assert!(!output.status.success(), "{}", result_text(&output));
    assert!(result_text(&output).contains("NOVA-SELFHOST-L003"), "{}", result_text(&output));
}

#[test]
fn project_040_does_not_misidentify_external_std_as_project_symbol() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = execute(&bootstrap(root),root,&fixture("external_package"));
    assert!(output.status.success(), "{}", result_text(&output));
}

#[test]
fn project_040_preserves_private_local_function_access() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = execute(&bootstrap(root),root,&fixture("same_module_private"));
    assert!(output.status.success(), "{}", result_text(&output));
}

#[test]
fn project_040_does_not_break_previous_project_graph() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let binary = bootstrap(root);
    let output = Command::new(binary).args(["--project-graph",
        "selfhost/tests/project_038/valid/Nova.toml"])
        .current_dir(root).output().expect("graph compatibility");
    assert!(output.status.success(), "{}", result_text(&output));
    assert!(result_text(&output).contains("Modules parsed: 5"));
}

#[test]
fn project_040_normalizes_path_segments_from_ast() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = execute(&bootstrap(root),root,&fixture("spaces_in_path"));
    assert!(output.status.success(), "{}", result_text(&output));
}
