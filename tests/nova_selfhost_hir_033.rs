use std::{path::Path, process::{Command, Output}};

fn invoke(binary: &Path, root: &Path, flags: &[&str], input: &str) -> Output {
    Command::new(binary)
        .args(flags)
        .arg(input)
        .env_remove("NOVA_SOURCE")
        .env_remove("NOVA_CST_PATH")
        .env_remove("NOVA_DUMP_AST")
        .current_dir(root)
        .output()
        .expect("run Nova selfhost")
}

fn bootstrap(root: &Path) -> std::path::PathBuf {
    let result = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("bootstrap-selfhost")
        .current_dir(root)
        .output()
        .expect("compile Nova selfhost");
    assert!(result.status.success(), "bootstrap failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr));
    root.join("selfhost/target/nova-selfhost")
}

#[test]
fn selfhost_033_lowers_functions_locals_and_calls() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let binary = bootstrap(root);
    let fixture = |name: &str| format!("selfhost/tests/hir_033/{name}.nova");

    for case in ["valid_calls", "valid_namespace", "valid_overloads"] {
        let output = invoke(&binary, root, &["--quiet", "--hir", "--check-hir"], &fixture(case));
        assert!(output.status.success(), "valid case '{case}' failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("HIR functions="), "{case}: {stdout}");
        assert!(stdout.contains("fn main("), "{case}: {stdout}");
        if case == "valid_calls" {
            assert!(stdout.contains("fn fib("), "{stdout}");
            assert!(stdout.contains(" -> fib"), "recursion target not resolved: {stdout}");
            assert!(stdout.contains(" -> puts"), "extern call not resolved: {stdout}");
        }
        if case == "valid_overloads" {
            assert!(stdout.contains("deferred=1"), "typed overload should be deferred: {stdout}");
        }
        if case == "valid_namespace" {
            assert!(stdout.contains(" -> compute"), "namespace call not resolved: {stdout}");
        }
    }

    for (case, error) in [
        ("invalid_duplicate_signature", "NOVA-SELFHOST-H1"),
        ("invalid_unknown_local", "NOVA-SELFHOST-H2"),
        ("invalid_scope_escape", "NOVA-SELFHOST-H2"),
        ("invalid_unknown_function", "NOVA-SELFHOST-H3"),
        ("invalid_arity", "NOVA-SELFHOST-H4"),
        ("invalid_duplicate_parameter", "NOVA-SELFHOST-H5"),
    ] {
        let output = invoke(&binary, root, &["--quiet", "--check-hir"], &fixture(case));
        assert_eq!(output.status.code(), Some(1), "'{case}' unexpectedly accepted:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(error), "{case} expected {error}: {stdout}");
        assert!(stdout.contains("--> line"), "missing source location for {case}: {stdout}");
    }

    // Syntax-only CLI behavior must remain unchanged.
    let ordinary = invoke(&binary, root, &["--quiet", "--check"], &fixture("invalid_arity"));
    assert!(ordinary.status.success(), "syntax-only mode changed unexpectedly");
}

#[test]
fn selfhost_033_parses_own_frontend_and_reference_types() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let binary = bootstrap(root);
    let files = std::fs::read_dir(root.join("selfhost/src")).expect("list Nova sources");
    for entry in files {
        let path = entry.expect("directory entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("nova") { continue; }
        let display = path.strip_prefix(root).expect("relative Nova path");
        let result = Command::new(&binary)
            .args(["--quiet", "--check"])
            .arg(display)
            .current_dir(root)
            .output()
            .expect("selfhost parses own module");
        assert!(result.status.success(), "selfhost cannot parse {}:\n{}\n{}",
            display.display(), String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    }
    let result = invoke(&binary, root, &["--quiet", "--check"],
        "selfhost/tests/hir_033/valid_references.nova");
    assert!(result.status.success(), "&T reference type rejected: {}",
        String::from_utf8_lossy(&result.stderr));
}
