use std::{path::Path, process::Command};
fn nova(action: &[&str], project: &str) -> (bool, String) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_nova"))
        .args(action)
        .arg(root.join("examples/packages").join(project))
        .current_dir(root)
        .output()
        .expect("launch Nova");
    (output.status.success(), format!("{}{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr)))
}
#[test]
fn nova_defined_getters_are_compiled_and_executed() {
    let (ok, output) = nova(&["run"], "nova_proc_getters_026");
    assert!(ok,"{output}");
}
#[test]
fn procedural_attribute_generates_real_struct() {
    let (ok, output) = nova(&["run"], "nova_proc_attribute_026");
    assert!(ok,"{output}");
}
#[test]
fn procedural_macros_may_not_call_runtime_functions() {
    let (ok, output) = nova(&["check"], "nova_proc_invalid_026");
    assert!(!ok,"expected compile-time rejection");
    assert!(output.contains("NOVA-PROC005"),"{output}");
}
#[test]
fn nova_meta_expand_shows_generated_methods() {
    let (ok, output) = nova(&["meta", "expand"], "nova_proc_getters_026");
    assert!(ok,"{output}");
    assert!(output.contains("impl"),"{output}");
}
#[test]
fn nova_defined_expression_macro_executes(){
    let (ok, output) = nova(&["run"], "nova_proc_expression_026");
    assert!(ok,"{output}");
}
#[test]
fn procedural_expression_macros_cannot_capture_implicit_locals(){
    let (ok, output)=nova(&["check"],"nova_proc_capture_invalid_026");
    assert!(!ok,"expected hygiene rejection");
    assert!(output.contains("NOVA-PROC010"),"{output}");
}
