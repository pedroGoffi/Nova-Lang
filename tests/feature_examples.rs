use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

fn feature_packages(root: &Path) -> Vec<PathBuf> {
    let features = root.join("examples/features");
    let mut packages = fs::read_dir(&features)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", features.display()))
        .map(|entry| entry.expect("cannot read feature entry").path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    packages.sort();
    packages
}

fn check_package(nova: &str, root: &Path, package: &Path) -> Result<(), String> {
    let output = Command::new(nova)
        .arg("check")
        .arg(package)
        .current_dir(root)
        .output()
        .map_err(|error| format!("cannot check {}: {error}", package.display()))?;

    if output.status.success() {
        return Ok(());
    }

    Err(format!(
        "nova check failed for {}\nstdout:\n{}\nstderr:\n{}",
        package.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

#[test]
fn every_feature_example_is_a_valid_package() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let packages = feature_packages(root);
    assert!(!packages.is_empty(), "examples/features must contain packages");

    for package in packages {
        assert!(
            package.join("Nova.toml").is_file(),
            "{} is missing Nova.toml",
            package.display()
        );
        assert!(
            package.join("src/main.nova").is_file(),
            "{} is missing src/main.nova",
            package.display()
        );
    }
}

#[test]
fn every_feature_example_passes_nova_check() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let nova = env!("CARGO_BIN_EXE_nova");
    let packages = feature_packages(root);

    // Four compiler processes keep the suite fast without making CI memory
    // usage proportional to the number of examples.
    for batch in packages.chunks(4) {
        thread::scope(|scope| {
            let handles = batch
                .iter()
                .map(|package| scope.spawn(move || check_package(nova, root, package)))
                .collect::<Vec<_>>();

            for handle in handles {
                match handle.join().expect("feature check worker panicked") {
                    Ok(()) => {}
                    Err(message) => panic!("{message}"),
                }
            }
        });
    }
}
