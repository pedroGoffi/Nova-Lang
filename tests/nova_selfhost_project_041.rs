use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn binary(root: &Path) -> PathBuf {
    let compiled = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("bootstrap-selfhost").current_dir(root).output().unwrap();
    assert!(compiled.status.success(), "bootstrap: {}{}",
            String::from_utf8_lossy(&compiled.stdout),
            String::from_utf8_lossy(&compiled.stderr));
    root.join("selfhost/target/nova-selfhost")
}
fn run(name: &str) -> Output {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    Command::new(binary(root))
        .arg("--check-project-links")
        .arg(format!("selfhost/tests/project_041/{name}/Nova.toml"))
        .env_remove("NOVA_SOURCE")
        .current_dir(root).output().unwrap()
}
fn combined(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr))
}
fn succeeds(name: &str) {
    let out = run(name);
    assert!(out.status.success(), "{name}: {}", combined(&out));
    assert!(combined(&out).contains("Project links OK:"), "{}", combined(&out));
}
fn fails(name: &str, code: &str) {
    let out = run(name);
    assert!(!out.status.success(), "{name}: {}", combined(&out));
    assert!(combined(&out).contains(code), "{name}: {}", combined(&out));
}
#[test]
fn private_root_module_access() { succeeds("root_private_module"); }
#[test]
fn descendant_access_to_private_ancestor_function() { succeeds("private_ancestor_function"); }
#[test]
fn parent_access_to_private_child_module() { succeeds("private_descendant_module"); }
#[test]
fn cross_file_wrong_arity() { fails("arity_mismatch","NOVA-SELFHOST-L004"); }
#[test]
fn cross_file_overloads_by_arity() { succeeds("arity_overloads"); }
#[test]
fn external_variadic_arity() { succeeds("arity_varargs"); }
#[test]
fn cross_file_private_sibling_denied() { fails("nested_private_sibling_rejected","NOVA-SELFHOST-L003"); }
