use std::{path::Path, process::Command};

fn nova(command: &str) -> std::process::Output {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg(command)
        .arg(root.join("examples/packages/nexoframe_dom_063"))
        .current_dir(root)
        .output()
        .expect("unable to run Nova")
}

#[test]
fn dom_063_nested_macros_typecheck() {
    let result = nova("check");
    assert!(
        result.status.success(),
        "DOM 0.63 did not typecheck:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn dom_063_nested_macros_escape_and_drop() {
    let result = nova("run");
    assert!(
        result.status.success(),
        "DOM 0.63 runtime failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("NexoFrame 0.63 DOM OK"));
}

#[test]
fn dom_063_macro_definitions_are_unique() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(root.join("packages/nexoframe/src/dom.nova"))
        .expect("missing DOM package");
    let root_source = std::fs::read_to_string(root.join("packages/nexoframe/src/lib.nova"))
        .expect("missing NexoFrame package");
    assert!(root_source.contains("pub mod dom;"));
    let mut macros = std::collections::HashSet::new();
    for line in source.lines() {
        if let Some(name) = line.trim().strip_prefix("pub macro_rules! ") {
            let name = name.split_whitespace().next().unwrap();
            assert!(
                macros.insert(name.to_string()),
                "duplicate DOM macro {name}"
            );
        }
    }
    assert!(macros.contains("grid"));
    assert!(macros.contains("main"));
    assert!(macros.contains("div"));
}
