use std::{path::Path, process::Command};

#[test]
fn selfhost_032_pretty_ast_uses_tree_names_and_locations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bootstrap = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("bootstrap-selfhost")
        .current_dir(root)
        .output()
        .expect("bootstrap Nova selfhost");
    assert!(bootstrap.status.success(), "bootstrap failed:\n{}\n{}",
        String::from_utf8_lossy(&bootstrap.stdout),
        String::from_utf8_lossy(&bootstrap.stderr));

    let binary = root.join("selfhost/target/nova-selfhost");
    let output = Command::new(&binary)
        .args(["--quiet", "--ast", "examples/hello.nova"])
        .env_remove("NOVA_SOURCE")
        .env_remove("NOVA_CST_PATH")
        .env_remove("NOVA_DUMP_AST")
        .current_dir(root)
        .output()
        .expect("run Nova pretty AST");
    assert!(output.status.success(), "pretty AST failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8(output.stdout).expect("valid UTF-8 output");
    assert!(stdout.contains("AST nodes="), "{stdout}");
    assert!(stdout.contains("Program"), "{stdout}");
    assert!(stdout.contains("Function main"), "{stdout}");
    assert!(stdout.contains("Return"), "{stdout}");
    assert!(stdout.contains("└──") || stdout.contains("├──"), "{stdout}");
    assert!(stdout.contains("@2:") || stdout.contains("@3:"), "{stdout}");
    assert!(!stdout.contains(" kind="), "legacy flat numeric output: {stdout}");

    let unicode = Command::new(&binary)
        .args(["--quiet", "--ast", "selfhost/tests/pretty_032/unicode.nova"])
        .current_dir(root)
        .output()
        .expect("render Unicode source");
    assert!(unicode.status.success(), "unicode AST: {}", String::from_utf8_lossy(&unicode.stderr));
    let unicode_stdout = String::from_utf8(unicode.stdout).unwrap();
    assert!(unicode_stdout.contains("‹\"ação 🚀\"›"), "{unicode_stdout}");
    assert!(unicode_stdout.contains("Return"), "{unicode_stdout}");

    let ordinary = Command::new(&binary)
        .arg("examples/hello.nova")
        .current_dir(root)
        .output()
        .expect("run default syntax check");
    assert!(ordinary.status.success());
    assert!(String::from_utf8_lossy(&ordinary.stdout).contains("OK examples/hello.nova"));
}
