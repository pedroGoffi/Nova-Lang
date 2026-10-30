use std::{fs, path::{Path, PathBuf}, process::{Command, Output}};

fn run(binary: &Path, root: &Path, file: &str, flags: &[&str]) -> Output {
    Command::new(binary)
        .args(flags)
        .arg(format!("selfhost/tests/mir_035/{file}.nova"))
        .env_remove("NOVA_SOURCE")
        .current_dir(root)
        .output()
        .expect("execute self-hosted Nova compiler")
}

#[test]
fn mir_035_builds_and_compiles_llvm_for_supported_programs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bootstrap = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("bootstrap-selfhost")
        .current_dir(root).output().expect("bootstrap-selfhost");
    assert!(bootstrap.status.success(), "bootstrap failed:\n{}\n{}",
        String::from_utf8_lossy(&bootstrap.stdout),
        String::from_utf8_lossy(&bootstrap.stderr));
    let binary = root.join("selfhost/target/nova-selfhost");
    let output_dir: PathBuf = std::env::temp_dir().join(format!("nova-selfhost-mir-035-{}", std::process::id()));
    fs::create_dir_all(&output_dir).expect("create output folder");

    for (name, exit_code, print) in [
        ("return_constant", 8, ""),
        ("arithmetic_call", 8, ""),
        ("recursive_fibonacci", 8, ""),
        ("while_counter", 0, ""),
        ("counter_nested", 8, ""),
        ("branch_return", 3, ""),
        ("local_expression", 8, ""),
        ("if_else", 8, ""),
        ("constant_folding", 8, ""),
        ("hello_world", 0, "Hello World from selfhost"),
    ] {
        let ll = output_dir.join(format!("{name}.ll"));
        let executable = output_dir.join(format!("{name}-bin"));
        let args = vec!["--quiet", "--check-mir", "--opt-mir", "--mir", "--emit-llvm",
                        ll.to_str().expect("UTF-8 output path")];
        let built = run(&binary,root,name,&args);
        let stdout = String::from_utf8_lossy(&built.stdout);
        assert!(built.status.success(), "{name}: selfhost failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&built.stderr));
        assert!(stdout.contains("MIR functions="), "{name}: {stdout}");
        let llvm = fs::read_to_string(&ll).expect("emitted LLVM module");
        assert!(llvm.contains("define i32 @main("), "{name}: {llvm}");
        let clang = Command::new("clang").arg("-x").arg("ir")
            .arg("-Wno-override-module").arg("-O0")
            .arg(&ll).arg("-o").arg(&executable)
            .output().expect("clang must be installed for MIR/LLVM integration tests");
        assert!(clang.status.success(), "{name}: LLVM rejected by clang:\n{}\n{llvm}",
            String::from_utf8_lossy(&clang.stderr));
        let executed = Command::new(&executable).output().expect("run generated application");
        assert_eq!(executed.status.code(),Some(exit_code),"{name}: wrong exit code");
        if !print.is_empty() {
            assert!(String::from_utf8_lossy(&executed.stdout).contains(print),
                "{name}: expected stdout {print}");
        }
    }
    let denied = run(&binary, root,"unsupported_float", &["--quiet", "--check-mir"]);
    assert_eq!(denied.status.code(),Some(1),"unsupported f64 silently accepted:\n{}",
        String::from_utf8_lossy(&denied.stdout));
    assert!(String::from_utf8_lossy(&denied.stdout).contains("NOVA-SELFHOST-M1"));
}

#[test]
fn mir_036_opcode_enum_has_unique_stable_wire_ids() {
    use std::collections::HashSet;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("selfhost/src/mir_opcode.nova"))
        .expect("MIR opcode catalog source");
    assert!(source.contains("pub enum Opcode"));
    let mappings: Vec<_> = source.lines()
        .filter(|line| line.contains("=> { return "))
        .map(|line| line.split("return ").nth(1).unwrap()
            .split(';').next().unwrap().trim().parse::<u32>()
            .expect("numeric opcode encoding"))
        .collect();
    assert_eq!(mappings.len(), 26, "the MIR instruction ABI has 26 named variants");
    let distinct: HashSet<_> = mappings.iter().copied().collect();
    assert_eq!(distinct.len(), mappings.len(), "duplicate opcode wire IDs");
    assert_eq!(distinct, (1..=26).collect(), "wire ABI changed unexpectedly");
}

#[test]
fn mir_036_branch_encodes_two_labels_with_enum_opcode() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("selfhost/src/mir.nova");
    let source = fs::read_to_string(path).expect("MIR Nova source");
    assert!(source.contains("emit(p,mir_opcode::branch(),-1,value,body_block,after_block,node);"));
    assert!(!source.contains("emit(p,mir_opcode::branch(),-1,value,body_block,after_block,11,node);"));
}
