use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bootstrap(root: &Path) -> PathBuf {
    let result = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("bootstrap-selfhost").current_dir(root).output().expect("stage-0 compiler");
    assert!(result.status.success(), "bootstrap failed: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr));
    root.join("selfhost/target/nova-selfhost")
}
fn run(bin: &Path, root: &Path, args: &[&str]) -> Output {
    Command::new(bin).args(args).env_remove("NOVA_SOURCE")
        .current_dir(root).output().expect("execute selfhost")
}
fn text(result: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr))
}

#[test]
fn project_039_indexes_qualified_symbols_across_files_and_visibility() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bin = bootstrap(root);
    let output = run(&bin,root,&["--project-symbols",
        "selfhost/tests/project_039/namespaces/Nova.toml"]);
    assert!(output.status.success(), "{}", text(&output));
    let displayed = text(&output);
    for required in ["crate::first::common kind=function visibility=pub",
                     "crate::first::hidden kind=function visibility=private",
                     "crate::first::Common kind=struct visibility=pub",
                     "crate::second::common kind=function visibility=pub",
                     "crate::first kind=module visibility=pub",
                     "crate::second kind=module visibility=private",
                     "Modules parsed: 3"] {
        assert!(displayed.contains(required), "missing {required}: {displayed}");
    }
    assert!(!displayed.contains("error["), "{displayed}");
}

#[test]
fn project_039_separates_module_and_function_namespaces() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bin = bootstrap(root);
    let output = run(&bin,root,&["--project-symbols",
        "selfhost/tests/project_039/distinct_spaces/Nova.toml"]);
    assert!(output.status.success(), "{}", text(&output));
    let displayed = text(&output);
    assert!(displayed.contains("crate::task kind=module visibility=pub"), "{displayed}");
    assert!(displayed.contains("crate::task kind=function visibility=pub"), "{displayed}");
    assert!(displayed.contains("crate::task::run kind=function visibility=pub"), "{displayed}");
}

#[test]
fn project_039_duplicate_type_name_is_rejected_in_its_namespace() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bin = bootstrap(root);
    let output = run(&bin,root,&["--project-symbols",
        "selfhost/tests/project_039/duplicate/Nova.toml"]);
    assert!(!output.status.success(), "duplicate accepted: {}", text(&output));
    assert!(text(&output).contains("NOVA-SELFHOST-S001"), "{}", text(&output));
    // This remains a syntax-only project: no duplicate symbol check unless requested.
    let basic = run(&bin,root,&["--check-project",
        "selfhost/tests/project_039/duplicate/Nova.toml"]);
    assert!(basic.status.success(), "syntax loading regression: {}", text(&basic));
}

#[test]
fn project_039_selfhost_itself_indexes_all_modules() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bin = bootstrap(root);
    let output = run(&bin,root,&["--project-symbols", "selfhost/Nova.toml"]);
    assert!(output.status.success(), "{}", text(&output));
    let displayed = text(&output);
    for required in ["crate::lexer::scan", "crate::mir::", "crate::project_index::",
                     "Modules parsed: 25", "Symbols indexed:"] {
        assert!(displayed.contains(required), "missing {required}: {displayed}");
    }
}

#[test]
fn project_039_existing_project_graph_is_unchanged() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bin = bootstrap(root);
    let output = run(&bin,root,&["--project-graph",
        "selfhost/tests/project_038/valid/Nova.toml"]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("Modules parsed: 5"), "{}", text(&output));
}
