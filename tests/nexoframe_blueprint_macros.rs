use std::{path::Path, process::Command};

#[test]
fn blueprint_macro_package_typechecks() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("check")
        .arg(root.join("examples/packages/nexoframe_backend_blueprint_check"))
        .current_dir(root).output().expect("Nova compiler unavailable");
    assert!(output.status.success(), "blueprint typecheck:\n{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn blueprint_macro_names_are_unique() {
    let source=include_str!("../packages/nexoframe/src/lib.nova");
    let mut names=std::collections::HashSet::new();
    for line in source.lines() {
        if let Some(name)=line.trim().strip_prefix("pub macro_rules! ") {
            assert!(names.insert(name.split_whitespace().next().unwrap()));
        }
    }
    for required in ["backend", "web", "prefix", "guard", "crud", "resource"] {
        assert!(names.contains(required), "missing macro {required}");
    }
}
