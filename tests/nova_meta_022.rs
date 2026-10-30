use std::{path::Path, process::Command};
fn run(command:&str,package:&str)->(bool,String){
    let cwd=Path::new(env!("CARGO_MANIFEST_DIR"));
    let result=Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg(command).arg(cwd.join("examples/packages").join(package))
        .current_dir(cwd).output().expect("Nova executable");
    (result.status.success(),format!("{}{}",String::from_utf8_lossy(&result.stdout),String::from_utf8_lossy(&result.stderr)))
}
#[test]
fn user_defined_metadata_compiles(){let (ok,out)=run("run","nova_meta_022");assert!(ok,"{out}");}
#[test]
fn effects_are_checked_transitively_on_direct_calls(){let (ok,out)=run("check","nova_meta_effect_invalid_022");assert!(!ok,"unexpected success");assert!(out.contains("NOVA-EFF002"),"{out}");}
#[test]
fn invalid_attribute_target_is_rejected(){let (ok,out)=run("check","nova_meta_attribute_invalid_022");assert!(!ok);assert!(out.contains("NOVA-META010"),"{out}");}
#[test]
fn incomplete_protocol_is_rejected(){let (ok,out)=run("check","nova_meta_protocol_invalid_022");assert!(!ok);assert!(out.contains("NOVA-META005"),"{out}");}
#[test]
fn source_defined_lint_rule_emits_warning(){let (ok,out)=run("lint","nova_meta_lint_022");assert!(ok,"{out}");assert!(out.contains("NOVA-META-L001"),"{out}");}
#[test]
fn cli_prints_meta_registry(){let (ok,out)=run("meta","nova_meta_022");assert!(ok,"{out}");assert!(out.contains("effect telemetry") && out.contains("protocol custom_pipeline"),"{out}");}
