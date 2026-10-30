use std::{path::Path, process::{Command, Output}};

fn invoke(binary: &Path, root: &Path, options: &[&str], input: &str) -> Output {
    Command::new(binary)
        .args(options)
        .arg(input)
        .env_remove("NOVA_SOURCE")
        .env_remove("NOVA_CST_PATH")
        .env_remove("NOVA_DUMP_AST")
        .current_dir(root)
        .output()
        .expect("run self-hosted compiler")
}

#[test]
fn selfhost_030_ast_and_declaration_index() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bootstrap = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("bootstrap-selfhost")
        .current_dir(root)
        .output()
        .expect("bootstrap Nova-written compiler");
    assert!(bootstrap.status.success(),
        "bootstrap failed:\n{}\n{}",
        String::from_utf8_lossy(&bootstrap.stdout),
        String::from_utf8_lossy(&bootstrap.stderr));
    let binary = root.join("selfhost/target/nova-selfhost");
    let fixture = |name: &str| format!("selfhost/tests/semantics_030/{name}.nova");

    let valid = invoke(&binary, root, &["--quiet", "--ast", "--symbols", "--verify-entry"],
        &fixture("valid_namespaces"));
    assert!(valid.status.success(), "valid names rejected:\n{}\n{}",
        String::from_utf8_lossy(&valid.stdout),
        String::from_utf8_lossy(&valid.stderr));
    let text = String::from_utf8_lossy(&valid.stdout);
    assert!(text.contains("AST nodes="), "{text}");
    assert!(text.contains("name=routes"), "{text}");
    assert!(text.contains("name=compute"), "{text}");
    assert!(text.contains("name=main"), "{text}");

    for case in ["duplicate_type", "duplicate_macro"] {
        let output = invoke(&binary, root, &["--quiet", "--symbols"], &fixture(case));
        assert_eq!(output.status.code(), Some(1),
            "duplicate {case} accepted: {}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("NOVA-SELFHOST-S001"));
    }

    let invalid = invoke(&binary, root, &["--quiet", "--verify-entry"], &fixture("invalid_entry"));
    assert_eq!(invalid.status.code(), Some(1), "bad main signature accepted");
    assert!(String::from_utf8_lossy(&invalid.stdout).contains("NOVA-SELFHOST-S002"));

    // Direct parity for src/main.rs::strip_comments. UTF-8 byte offsets
    // must be retained even when comments contain multi-byte characters.
    let raw_source = std::fs::read_to_string(root.join(&fixture("comments"))).unwrap();
    let normalized = invoke(&binary, root, &["--strip-comments"], &fixture("comments"));
    assert!(normalized.status.success(), "strip_comments rejected fixture");
    let normalized = String::from_utf8(normalized.stdout).unwrap();
    assert_eq!(normalized.len(), raw_source.len(), "comment normalization shifted byte spans");
    assert!(normalized.contains("http://api.example/ação"));
    assert!(normalized.contains("/* deve ficar exatamente igual */"));
    assert!(!normalized.contains("explicação"));
    assert!(!normalized.contains("outra linha"));

    // The previous 0.29 syntax-only command must retain its behavior.
    let old = invoke(&binary, root, &[], "examples/hello.nova");
    assert!(old.status.success(), "CLI regression: {}", String::from_utf8_lossy(&old.stderr));
    assert!(String::from_utf8_lossy(&old.stdout).contains("OK examples/hello.nova"));
}
