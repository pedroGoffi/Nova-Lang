use std::{path::Path, process::Command};

fn nova(mode: &str, package: &str) -> std::process::Output {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg(mode)
        .arg(root.join("examples/packages").join(package))
        .current_dir(root)
        .output()
        .expect("cannot execute Nova compiler")
}

#[test]
fn nexoframe_054_html5_macro_examples_typecheck() {
    for package in ["nexoframe_054_sanity", "nexoframe_054"] {
        let output = nova("check", package);
        assert!(
            output.status.success(),
            "{package} failed typechecking:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn nexoframe_054_markup_escapes_and_validates_inputs() {
    let output = nova("run", "nexoframe_054_sanity");
    assert!(
        output.status.success(),
        "markup tests failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("NexoFrame 0.54 HTML OK"));
}

// Nova resolves macros and modules in separate namespaces. A public macro
// may intentionally have the same spelling as a module (e.g. routes and
// routes!), but two macros or two modules must not share the same name.
#[test]
fn nexoframe_054_public_macros_do_not_shadow_modules() {
    use std::collections::HashSet;
    let source = include_str!("../packages/nexoframe/src/lib.nova");
    let mut modules = HashSet::new();
    for name in source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pub mod "))
        .filter_map(|name| name.strip_suffix(';'))
    {
        assert!(modules.insert(name), "duplicate NexoFrame module: {name}");
    }

    let mut macros = HashSet::new();
    for line in source.lines() {
        if let Some(rest) = line.trim().strip_prefix("pub macro_rules! ") {
            let name = rest.split_whitespace().next().expect("macro name");
            assert!(macros.insert(name), "duplicate NexoFrame macro: {name}");
        }
    }

    // This collision is intentional, valid, and exercised by the other
    // NexoFrame typechecking and native execution integration tests.
    assert!(modules.contains("routes"), "routes module missing");
    assert!(macros.contains("routes"), "routes! macro missing");
    assert!(macros.contains("input_tag"));
    assert!(modules.contains("input"));
}
