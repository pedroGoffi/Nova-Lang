use std::{path::Path, process::{Command, Output}};

fn run(binary: &Path, root: &Path, input: &str, args: &[&str]) -> Output {
    Command::new(binary)
        .args(args)
        .arg(format!("selfhost/tests/types_034/{input}.nova"))
        .env_remove("NOVA_SOURCE")
        .current_dir(root)
        .output()
        .expect("run self-hosted typed HIR")
}

#[test]
fn selfhost_034_checks_primitive_types_and_resolves_overloads() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bootstrap = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("bootstrap-selfhost")
        .current_dir(root)
        .output()
        .expect("bootstrap-selfhost");
    assert!(bootstrap.status.success(), "bootstrap failed:\n{}\n{}",
        String::from_utf8_lossy(&bootstrap.stdout),
        String::from_utf8_lossy(&bootstrap.stderr));
    let binary = root.join("selfhost/target/nova-selfhost");

    for case in ["valid_types", "valid_shadowing", "valid_namespace", "valid_literals"] {
        let checked = run(&binary, root, case, &["--quiet", "--types", "--check-types"]);
        let stdout = String::from_utf8_lossy(&checked.stdout);
        let stderr = String::from_utf8_lossy(&checked.stderr);
        assert!(checked.status.success(), "{case} failed:\n{stdout}\n{stderr}");
        assert!(stdout.contains("TYPES functions="), "{case}: {stdout}");
        assert!(stdout.contains("deferred=0"), "{case}: {stdout}");
        if case == "valid_types" {
            assert!(stdout.contains("-> fib : i64"), "{stdout}");
            assert!(stdout.contains("-> choose : i32"), "{stdout}");
            assert!(stdout.contains("-> choose : i64"), "{stdout}");
        }
        if case == "valid_namespace" {
            assert!(stdout.contains("-> sum : i64"), "{stdout}");
        }
    }

    for (case, error) in [
        ("invalid_initializer", "NOVA-SELFHOST-T1"),
        ("invalid_return", "NOVA-SELFHOST-T2"),
        ("invalid_condition", "NOVA-SELFHOST-T3"),
        ("invalid_operator", "NOVA-SELFHOST-T4"),
        ("invalid_call", "NOVA-SELFHOST-T5"),
        ("invalid_ambiguous", "NOVA-SELFHOST-T6"),
        ("deferred_nominal", "NOVA-SELFHOST-T7"),
    ] {
        let checked = run(&binary, root, case, &["--quiet", "--check-types"]);
        let stdout = String::from_utf8_lossy(&checked.stdout);
        let stderr = String::from_utf8_lossy(&checked.stderr);
        assert_eq!(checked.status.code(), Some(1), "{case} accepted:\n{stdout}\n{stderr}");
        assert!(stdout.contains(error), "{case} expected {error}: {stdout}");
        if error != "NOVA-SELFHOST-T7" {
            assert!(stdout.contains("--> line"), "{case}: no source location: {stdout}");
        }
    }

    // Typed checks must not change the syntax-only behavior from previous releases.
    let parse_only = run(&binary, root, "invalid_return", &["--quiet", "--check"]);
    assert!(parse_only.status.success(), "syntax-only mode was changed");
}
