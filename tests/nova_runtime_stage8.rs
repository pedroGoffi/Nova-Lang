use std::{fs, path::Path, process::Command};

#[test]
fn stage8_generic_struct_and_owned_vectors() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for package in ["nova_plain_generic_032", "nova_owned_vec_033"] {
        for action in ["check", "run"] {
            let result = Command::new(env!("CARGO_BIN_EXE_nova"))
                .arg(action).arg(root.join("examples/packages").join(package))
                .current_dir(root).output().expect("launch Nova");
            assert!(result.status.success(), "{package} {action}:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr));
        }
    }
}

#[test]
fn raw_pointer_offset_and_collections_are_not_c_handlers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let runtime = fs::read_to_string(root.join("runtime/nova_rt.c")).unwrap();
    assert!(!runtime.contains("__nova_internal_byte_offset"));
    let backend = fs::read_to_string(root.join("src/codegen.rs")).unwrap();
    assert!(backend.contains("getelementptr i8, ptr {address}, i64 {distance}"));
    assert!(backend.contains("icmp eq ptr {address}, null"));
    let source = fs::read_to_string(root.join("std/src/old/vec.nova")).unwrap();
    assert!(source.contains("std::old::raw::take_from::<T>"));
    assert!(source.contains("std::old::raw::drop_in_place::<T>"));
    assert!(!source.contains("extern fn"));
}
