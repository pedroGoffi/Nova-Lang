use std::{path::Path,process::Command};
fn run(action:&str,project:&str)->(bool,String){
    let root=Path::new(env!("CARGO_MANIFEST_DIR"));
    let output=Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg(action).arg(root.join("examples/packages").join(project))
        .current_dir(root).output().expect("Nova executable");
    (output.status.success(),format!("{}{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr)))
}
#[test]
fn reflection_and_comptime_functions_execute(){
    let (success,out)=run("run","nova_meta_ctfe_025");
    assert!(success,"{out}");
}
#[test]
fn failed_compile_time_contract_has_specific_error(){
    let (success,out)=run("check","nova_meta_ctfe_invalid_025");
    assert!(!success,"expected compile-time assertion failure");
    assert!(out.contains("NOVA-CTFE013"),"{out}");
}

#[test]
fn meta_verify_runs_contracts_without_native_codegen(){
    let root=Path::new(env!("CARGO_MANIFEST_DIR"));
    let output=Command::new(env!("CARGO_BIN_EXE_nova"))
        .args(["meta","verify"])
        .arg(root.join("examples/packages/nova_meta_ctfe_025"))
        .current_dir(root).output().expect("nova meta verify");
    let text=format!("{}{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(),"{text}");
    assert!(text.contains("assertions satisfied"),"{text}");
}
