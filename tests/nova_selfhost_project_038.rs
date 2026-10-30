use std::{fs, path::{Path, PathBuf}, process::{Command, Output}};

fn bootstrap(root: &Path) -> PathBuf {
    let result = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("bootstrap-selfhost")
        .current_dir(root)
        .output()
        .expect("stage-0 Nova executable is required");
    assert!(result.status.success(), "bootstrap failure:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr));
    root.join("selfhost/target/nova-selfhost")
}
fn run(binary: &Path, root: &Path, args: &[&str]) -> Output {
    Command::new(binary)
        .args(args)
        .env_remove("NOVA_SOURCE")
        .current_dir(root)
        .output()
        .expect("run selfhost project checker")
}
fn combined(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout),
                     String::from_utf8_lossy(&output.stderr))
}

#[test]
fn selfhost_038_loads_manifest_and_recursive_mod_tree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bin = bootstrap(root);
    let result = run(&bin, root, &["--project-graph", "selfhost/tests/project_038/valid/Nova.toml"]);
    assert!(result.status.success(), "{}", combined(&result));
    let message = combined(&result);
    for needle in ["main.nova", "calculator.nova", "arithmetic.nova", "nested/mod.nova", "nested/child.nova", "Modules parsed: 5"] {
        assert!(message.contains(needle), "missing {needle}: {message}");
    }
    let result = run(&bin, root, &["selfhost/tests/project_038/valid/Nova.toml", "--check-project"]);
    assert!(result.status.success(), "{}", combined(&result));
    assert!(combined(&result).contains("Modules parsed: 5"));
    let direct = run(&bin, root, &["--check-project", "selfhost/tests/project_038/valid/src/main.nova"]);
    assert!(direct.status.success(), "{}", combined(&direct));
}

#[test]
fn selfhost_038_rejects_missing_duplicate_ambiguous_modules_and_syntax() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bin = bootstrap(root);
    for (relative, expected) in [
        ("missing/Nova.toml", "NOVA-M005"),
        ("duplicate/src/main.nova", "NOVA-M004"),
        ("ambiguous/src/main.nova", "NOVA-M006"),
        ("invalid_syntax/src/main.nova", "NOVA-SELFHOST-P001"),
        ("invalid_manifest/Nova.toml", "NOVA-SELFHOST-PROJECT001"),
        ("missing_root/Nova.toml", "NOVA-M005"),
    ] {
        let input = format!("selfhost/tests/project_038/{relative}");
        let result = run(&bin, root, &["--check-project", &input]);
        assert!(!result.status.success(), "{relative} unexpectedly passed: {}", combined(&result));
        assert!(combined(&result).contains(expected), "{relative} expected {expected}: {}", combined(&result));
    }
}

#[test]
fn selfhost_038_loader_is_written_in_nova_with_source_backed_ast() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let main = fs::read_to_string(root.join("selfhost/src/main.nova")).unwrap();
    let project = fs::read_to_string(root.join("selfhost/src/project.nova")).unwrap();
    assert!(main.contains("pub mod project;"));
    assert!(main.contains("project::check("));
    assert!(project.contains("ast::kind(tree,node) == ast_kind::module_item()"));
    assert!(project.contains("fn module_folder(path: u64)"));
    assert!(project.contains("fn entry_from_manifest(path: u64)"));
    assert!(project.contains("fn visit(graph: Graph"));
    assert!(!project.contains("std::env::native_arg_at("));
}
