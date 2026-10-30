use std::{path::Path, process::Command};

fn launch(package: &str) -> (bool,String) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("run")
        .arg(root.join("examples/packages").join(package))
        .current_dir(root)
        .output()
        .expect("Nova binary unavailable");
    let diagnostic = format!("stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));
    (output.status.success(),diagnostic)
}

#[test]
fn nova_defined_operators_and_capabilities_execute() {
    let (ok,diagnostic) = launch("nova_operator_traits_021");
    assert!(ok,"{diagnostic}");
}
#[test]
fn missing_capability_is_rejected() {
    let (ok,diagnostic) = launch("nova_operator_missing_021");
    assert!(!ok,"capability without impl was accepted");
    assert!(diagnostic.contains("NOVA-OP001"),"{diagnostic}");
}
#[test]
fn ambiguous_traits_are_rejected() {
    let (ok,diagnostic) = launch("nova_operator_ambiguous_021");
    assert!(!ok,"ambiguous operator was accepted");
    assert!(diagnostic.contains("NOVA-OP004"),"{diagnostic}");
}
#[test]
fn unsatisfied_declared_concept_is_rejected() {
    let (ok,diagnostic) = launch("nova_operator_requires_021");
    assert!(!ok,"false #[requires] concept was accepted");
    assert!(diagnostic.contains("NOVA-OP005"),"{diagnostic}");
}
