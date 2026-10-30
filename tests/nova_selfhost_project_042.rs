use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn compile(root: &Path) -> PathBuf {
    let output = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("bootstrap-selfhost")
        .current_dir(root)
        .output()
        .expect("bootstrap stage-1 compiler");
    assert!(output.status.success(), "bootstrap: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));
    root.join("selfhost/target/nova-selfhost")
}
fn run(fixture: &str, args: &[&str]) -> Output {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let file = format!("selfhost/tests/project_042/{fixture}/Nova.toml");
    let bin = compile(root);
    Command::new(bin)
        .args(args)
        .arg(file)
        .env_remove("NOVA_SOURCE")
        .current_dir(root)
        .output()
        .expect("execute selfhost")
}
fn output_text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr))
}
fn should_link(fixture: &str) {
    let output = run(fixture, &["--check-project-links"]);
    assert!(output.status.success(), "{fixture}: {}", output_text(&output));
    assert!(output_text(&output).contains("Project links OK:"), "{fixture}: {}", output_text(&output));
}
fn should_reject(fixture: &str, diagnostic: &str) {
    let output = run(fixture, &["--check-project-links"]);
    assert!(!output.status.success(), "{fixture}: {}", output_text(&output));
    assert!(output_text(&output).contains(diagnostic), "{fixture}: {}", output_text(&output));
}
#[test]
fn indexes_inline_namespace_declarations() { should_link("inline_namespace_calls"); }
#[test]
fn indexes_nested_namespaces_and_self_paths() { should_link("nested_namespace_calls"); }
#[test]
fn resolves_child_module_of_inline_namespace() { should_link("namespace_child_mod"); }
#[test]
fn isolates_private_sibling_namespaces_even_within_same_file() {
    should_reject("inline_private_sibling_denied", "NOVA-SELFHOST-L002");
}
#[test]
fn verifies_cross_file_primitive_argument_types() {
    should_reject("wrong_literal_types", "NOVA-SELFHOST-L005");
}
#[test]
fn accepts_overloads_by_primitive_type() { should_link("typed_overloads"); }
#[test]
fn preserves_unknown_local_type_for_later_typed_hir() { should_link("unresolved_local_deferred"); }
#[test]
fn rejects_duplicate_inline_namespace_declarations() {
    should_reject("duplicate_namespaces", "NOVA-SELFHOST-S001");
}
#[test]
fn rejects_duplicate_primitive_overload_signatures() {
    should_reject("duplicate_primitive_signatures", "NOVA-SELFHOST-S001");
}
#[test]
fn rejects_duplicate_zero_argument_functions() {
    should_reject("duplicate_zero_argument_functions", "NOVA-SELFHOST-S001");
}
#[test]
fn lists_qualified_namespace_functions() {
    let result = run("nested_namespace_calls", &["--project-symbols"]);
    let text = output_text(&result);
    assert!(result.status.success(), "{text}");
    assert!(text.contains("crate::outer::inner::twice"), "{text}");
    assert!(text.contains("crate::outer::inner::delegate"), "{text}");
}
