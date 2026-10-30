use std::{path::Path,process::Command};
fn nova(command: &str, project: &str)->(bool,String){
    let root=Path::new(env!("CARGO_MANIFEST_DIR"));
    let output=Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg(command).arg(root.join("examples/packages").join(project))
        .current_dir(root).output().expect("run Nova compiler");
    (output.status.success(),format!("{}{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr)))
}
#[test]
fn nova_authored_default_method_derives_compile_and_execute(){
    let (ok,out)=nova("run","nova_meta_programmable_024");
    assert!(ok,"{out}");
}
#[test]
fn incomplete_derive_contract_fails_before_codegen(){
    let (ok,out)=nova("check","nova_meta_invalid_template_024");
    assert!(!ok,"expected compiler failure");
    assert!(out.contains("NOVA-META020"),"{out}");
}

#[test]
fn structural_concept_rejects_invalid_derive_target(){
    let (ok,out)=nova("check","nova_meta_invalid_concept_024");
    assert!(!ok,"expected compiler failure");
    assert!(out.contains("NOVA-META022"),"{out}");
}

#[test]
fn meta_cli_displays_derived_impls_from_nova_source(){
    let root=Path::new(env!("CARGO_MANIFEST_DIR"));
    let result=Command::new(env!("CARGO_BIN_EXE_nova"))
        .args(["meta","expand"])
        .arg(root.join("examples/packages/nova_meta_programmable_024"))
        .current_dir(root).output().expect("run nova meta expand");
    let output=format!("{}{}",String::from_utf8_lossy(&result.stdout),String::from_utf8_lossy(&result.stderr));
    assert!(result.status.success(),"{output}");
    assert!(output.contains("impl Identifiable for Named(\"User\")"),"{output}");
}

#[test]
fn automatically_derived_impl_cannot_overlap_explicit_impl(){
    let (ok,out)=nova("check","nova_meta_duplicate_impl_024");
    assert!(!ok,"expected duplicate impl rejection");
    assert!(out.contains("NOVA-OP003"),"{out}");
}
