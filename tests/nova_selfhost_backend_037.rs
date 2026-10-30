use std::{fs, path::{Path, PathBuf}, process::{Command, Output}};

fn bootstrap(root: &Path) -> PathBuf {
    let built = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("bootstrap-selfhost")
        .current_dir(root)
        .output()
        .expect("execute Nova stage-0 bootstrap");
    assert!(built.status.success(), "bootstrap-selfhost failed:\n{}\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr));
    root.join("selfhost/target/nova-selfhost")
}

fn compile(binary: &Path, root: &Path, file: &str, llvm_path: &Path, optimize: bool) -> Output {
    let mut command = Command::new(binary);
    command.arg("--quiet").arg("--check-mir");
    if optimize { command.arg("--opt-mir"); }
    command.arg("--emit-llvm").arg(llvm_path)
        .arg(format!("selfhost/tests/mir_037/{file}.nova"))
        .env_remove("NOVA_SOURCE")
        .current_dir(root);
    command.output().expect("execute Nova selfhost MIR backend")
}

#[test]
fn selfhost_037_real_llvm_executes_short_circuit_loops_ranges_and_unsigned() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let binary = bootstrap(root);
    let tmp = std::env::temp_dir().join(format!("nova-selfhost-037-{}", std::process::id()));
    fs::create_dir_all(&tmp).expect("create temporary output directory");

    for (fixture, expected_exit, optimize, ir_marker) in [
        ("short_circuit_and", 0, false, "br i1"),
        ("short_circuit_or", 8, true, "br i1"),
        ("side_effect_short_circuit", 0, true, "br i1"),
        ("loop_break", 7, false, "br label"),
        ("while_continue", 5, true, "br label"),
        ("nested_loops", 6, true, "br label"),
        ("unsigned_division", 9, false, "udiv i32"),
        ("range_for", 7, false, "icmp slt i64"),
        ("range_for_continue", 9, true, "icmp slt i64"),
        ("nested_range_for", 8, true, "icmp slt i64"),
    ] {
        let ll = tmp.join(format!("{fixture}.ll"));
        let executable = tmp.join(format!("{fixture}.bin"));
        let generated = compile(&binary, root, fixture, &ll, optimize);
        assert!(generated.status.success(), "{fixture}: selfhost failed:\n{}\n{}",
            String::from_utf8_lossy(&generated.stdout),
            String::from_utf8_lossy(&generated.stderr));
        let ir = fs::read_to_string(&ll).expect("emitted LLVM IR");
        assert!(ir.contains("define i32 @main("), "{fixture}: no main definition");
        assert!(ir.contains(ir_marker), "{fixture}: expected IR marker {ir_marker}:\n{ir}");
        let checked = Command::new("clang")
            .args(["-x", "ir", "-Wno-override-module", "-O0"])
            .arg(&ll).arg("-o").arg(&executable)
            .output().expect("clang required for executable integration test");
        assert!(checked.status.success(), "{fixture}: LLVM rejected:\n{}\n{}",
            String::from_utf8_lossy(&checked.stderr), ir);
        let actual = Command::new(&executable).output().expect("run LLVM executable");
        assert_eq!(actual.status.code(), Some(expected_exit),
            "{fixture}: unexpected program exit; output={}",
            String::from_utf8_lossy(&actual.stdout));
    }
}

#[test]
fn selfhost_037_rejects_invalid_loop_control_and_integer_overflow() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let binary = bootstrap(root);
    let tmp = std::env::temp_dir().join(format!("nova-selfhost-037-invalid-{}", std::process::id()));
    fs::create_dir_all(&tmp).expect("create temporary test folder");
    for (fixture, diagnostic) in [
        ("invalid_break", "NOVA-SELFHOST-M5"),
        ("invalid_u8_literal", "NOVA-SELFHOST-M1"),
    ] {
        let result = compile(&binary, root, fixture, &tmp.join("invalid.ll"), false);
        assert!(!result.status.success(), "{fixture}: invalid program was accepted");
        assert!(String::from_utf8_lossy(&result.stdout).contains(diagnostic),
            "{fixture}: expected {diagnostic}, got:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    }
}

#[test]
fn selfhost_037_source_uses_named_flags_and_diagnostics() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let module = fs::read_to_string(root.join("selfhost/src/main.nova")).unwrap();
    assert!(module.contains("pub mod cli_flag;"));
    assert!(module.contains("pub mod mir_error;"));
    let cli = fs::read_to_string(root.join("selfhost/src/cli.nova")).unwrap();
    assert!(cli.contains("cli_flag::emit_llvm()"));
    assert!(!cli.contains("set_flag(args,65536)"));
    let mir = fs::read_to_string(root.join("selfhost/src/mir.nova")).unwrap();
    assert!(mir.contains("fn short_circuit("));
    assert!(mir.contains("fn temporary("));
    assert!(mir.contains("ast_kind::for_range_node()"));
    assert!(mir.contains("mir_error::invalid_loop_control()"));
    let optim = fs::read_to_string(root.join("selfhost/src/optim.nova")).unwrap();
    assert!(optim.contains("mir_opcode::is_terminator(op)"));
}
