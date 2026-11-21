//! Repository-level contracts for the standard library/compiler boundary.
//!
//! nova doctor deliberately does not compile or link a program. It checks
//! the source layout and the embedded-module inventory so a partially moved
//! stdlib is reported before it becomes a confusing type-resolution failure.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

const ABI_DIRECTORY: &str = "abi";
const OLD_DIRECTORY: &str = "old";
const COMPILER_INTRINSICS: &[&str] = &["size_of", "align_of", "byte_offset"];

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub checked_files: usize,
    pub extern_files: usize,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

fn collect_nova(root: &Path, base: &Path, output: &mut BTreeSet<String>) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(|e| format!("{}: {e}", root.display()))? {
        let path = entry.map_err(|e| format!("{}: {e}", root.display()))?.path();
        if path.is_dir() {
            collect_nova(&path, base, output)?;
        } else if path.extension().is_some_and(|extension| extension == "nova") {
            let relative = path
                .strip_prefix(base)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            output.insert(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

fn is_abi_file(relative: &str) -> bool {
    relative == format!("{ABI_DIRECTORY}.nova")
        || relative.starts_with(&format!("{ABI_DIRECTORY}/"))
}

fn audit_llvm_intrinsics(
    items: &[crate::ast::Item],
    namespace: &str,
    errors: &mut Vec<String>,
    found: &mut std::collections::HashSet<crate::llvm_intrinsics::Intrinsic>,
) {
    for item in items {
        let item = match item {
            crate::ast::Item::Public(inner) => inner.as_ref(),
            other => other,
        };
        match item {
            crate::ast::Item::Namespace { name, items } => {
                let child = if namespace.is_empty() {
                    name.clone()
                } else {
                    format!("{namespace}::{name}")
                };
                audit_llvm_intrinsics(items, &child, errors, found);
            }
            crate::ast::Item::Function(function)
                if function.attributes.iter().any(|attribute| attribute.name == "llvm_intrinsic") =>
            {
                let marker = function
                    .attributes
                    .iter()
                    .find(|attribute| attribute.name == "llvm_intrinsic")
                    .expect("llvm intrinsic marker was checked above");
                let compiler_name = format!("std__abi__llvm__{}", function.name);
                let Some(intrinsic) = crate::llvm_intrinsics::Intrinsic::from_name(&compiler_name) else {
                    errors.push(format!(
                        "std::abi::llvm::{} is marked as an LLVM intrinsic but is absent from the compiler registry",
                        function.name
                    ));
                    continue;
                };
                if marker.args.len() != 1 || marker.args[0] != intrinsic.label() {
                    errors.push(format!(
                        "std::abi::llvm::{} has marker {:?}, compiler registry requires #[llvm_intrinsic({})]",
                        function.name,
                        marker.args,
                        intrinsic.label()
                    ));
                }
                found.insert(intrinsic);
                if namespace != "std::abi::llvm" {
                    errors.push(format!(
                        "LLVM intrinsic '{}' is declared outside std::abi::llvm (found in {namespace})",
                        function.name
                    ));
                }
            }
            _ => {}
        }
    }
}

pub fn inspect(root: &Path) -> Result<Report, String> {
    let std_src = root.join("std").join("src");
    let mut report = Report::default();
    let mut actual = BTreeSet::new();
    collect_nova(&std_src, &std_src, &mut actual)?;

    let embedded = crate::modules::EMBEDDED_STD
        .iter()
        .map(|(path, _)| (*path).to_owned())
        .collect::<BTreeSet<_>>();
    let mut loader = crate::modules::Loader::default();
    let embedded_items = loader.embedded_std("std")?;
    let mut found_llvm_intrinsics = std::collections::HashSet::new();
    audit_llvm_intrinsics(
        &embedded_items,
        "std",
        &mut report.errors,
        &mut found_llvm_intrinsics,
    );
    let registered_llvm_intrinsics = crate::llvm_intrinsics::Intrinsic::ALL
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    if found_llvm_intrinsics != registered_llvm_intrinsics {
        let missing = registered_llvm_intrinsics
            .difference(&found_llvm_intrinsics)
            .map(|intrinsic| intrinsic.label())
            .collect::<Vec<_>>();
        let unexpected = found_llvm_intrinsics
            .difference(&registered_llvm_intrinsics)
            .map(|intrinsic| intrinsic.label())
            .collect::<Vec<_>>();
        report.errors.push(format!(
            "compiler/stdlib LLVM intrinsic registry diverged (missing: {}; unexpected: {})",
            if missing.is_empty() { "none".into() } else { missing.join(", ") },
            if unexpected.is_empty() { "none".into() } else { unexpected.join(", ") },
        ));
    }
    fn has_namespace(items: &[crate::ast::Item], path: &[&str]) -> bool {
        if path.is_empty() {
            return true;
        }
        items.iter().any(|item| {
            let item = match item {
                crate::ast::Item::Public(inner) => inner.as_ref(),
                other => other,
            };
            match item {
                crate::ast::Item::Namespace { name, items } if name == path[0] => {
                    has_namespace(items, &path[1..])
                }
                _ => false,
            }
        })
    }
    fn child_names(items: &[crate::ast::Item], path: &[&str]) -> Vec<String> {
        if path.is_empty() {
            return items
                .iter()
                .filter_map(|item| {
                    let item = match item {
                        crate::ast::Item::Public(inner) => inner.as_ref(),
                        other => other,
                    };
                    match item {
                        crate::ast::Item::Namespace { name, .. } => Some(name.clone()),
                        _ => None,
                    }
                })
                .collect();
        }
        items.iter().find_map(|item| {
            let item = match item {
                crate::ast::Item::Public(inner) => inner.as_ref(),
                other => other,
            };
            match item {
                crate::ast::Item::Namespace { name, items } if name == path[0] => {
                    Some(child_names(items, &path[1..]))
                }
                _ => None,
            }
        }).unwrap_or_default()
    }
    for required in [&["old", "vec"][..], &["old", "map"][..], &["abi", "native"][..]] {
        if !has_namespace(&embedded_items, required) {
            let available = child_names(&embedded_items, &["old"]);
            report.errors.push(format!(
                "embedded std is missing namespace std::{} (available under std::old: {})",
                required.join("::"),
                available.join(", ")
            ));
        }
    }
    let missing = actual.difference(&embedded).cloned().collect::<Vec<_>>();
    let stale = embedded.difference(&actual).cloned().collect::<Vec<_>>();
    if !missing.is_empty() {
        report.errors.push(format!(
            "stdlib files missing from embedded registry: {}",
            missing.join(", ")
        ));
    }
    if !stale.is_empty() {
        report.errors.push(format!(
            "embedded registry contains stale files: {}",
            stale.join(", ")
        ));
    }

    for relative in &actual {
        report.checked_files += 1;
        let path = std_src.join(relative);
        let source = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let extern_count = source.matches("extern fn").count();
        if extern_count > 0 {
            report.extern_files += 1;
            if !is_abi_file(relative) {
                report.errors.push(format!(
                    "foreign declarations must live under std/src/{ABI_DIRECTORY}: {relative}"
                ));
            }
        }
    }

    let raw = std_src.join(OLD_DIRECTORY).join("raw.nova");
    if raw.exists() {
        let source = fs::read_to_string(&raw).map_err(|e| format!("{}: {e}", raw.display()))?;
        for intrinsic in COMPILER_INTRINSICS {
            let declaration = format!("fn {intrinsic}");
            let foreign = format!("extern fn {intrinsic}");
            if source.contains(&declaration) || source.contains(&foreign) {
                report.errors.push(format!(
                    "compiler intrinsic '{intrinsic}' must not be implemented in std/src/{OLD_DIRECTORY}/raw.nova"
                ));
            }
        }
    }

    let helpers = std_src.join(OLD_DIRECTORY).join("helpers.nova");
    if helpers.exists() {
        let source = fs::read_to_string(&helpers)
            .map_err(|e| format!("{}: {e}", helpers.display()))?;
        for required in ["macro_rules! Spawn", "macro_rules! spawn_join", "macro_rules! ensure", "ThreadCallback"] {
            if !source.contains(required) {
                report.errors.push(format!(
                    "stdlib helper contract is missing '{required}' in std/src/{OLD_DIRECTORY}/helpers.nova"
                ));
            }
        }
    } else {
        report.errors.push("stdlib helper contract is missing std/src/old/helpers.nova".into());
    }

    let concepts = std_src.join(OLD_DIRECTORY).join("concepts.nova");
    if concepts.exists() {
        let source = fs::read_to_string(&concepts)
            .map_err(|e| format!("{}: {e}", concepts.display()))?;
        for required in ["pub concept Send", "pub concept Sync", "pub concept ThreadCallback"] {
            if !source.contains(required) {
                report.errors.push(format!(
                    "stdlib concurrency contract is missing '{required}' in std/src/{OLD_DIRECTORY}/concepts.nova"
                ));
            }
        }
    } else {
        report.errors.push("stdlib concurrency contract is missing std/src/old/concepts.nova".into());
    }

    let lib = fs::read_to_string(std_src.join("core/manifest.nova"))
        .map_err(|e| format!("std/src/core/manifest.nova: {e}"))?;
    if !lib.contains("pub mod abi;") {
        report
            .errors
            .push("std/src/core/manifest.nova must expose the compiler ABI boundary as pub mod abi".into());
    }
    if lib.contains("pub mod raw;") {
        report
            .errors
            .push("std/src/core/manifest.nova must not expose std::raw".into());
    }
    if report.errors.is_empty() {
        report
            .warnings
            .push("stdlib ABI boundary and embedded module inventory are consistent".into());
    }
    Ok(report)
}

pub fn run(root: &Path) -> Result<(), String> {
    let report = inspect(root)?;
    println!(
        "Nova doctor: checked {} stdlib files ({} ABI files with foreign declarations)",
        report.checked_files, report.extern_files
    );
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
    if report.errors.is_empty() {
        println!("OK: compiler/stdlib contracts are coherent");
        return Ok(());
    }
    for error in &report.errors {
        eprintln!("error[NOVA-DOCTOR]: {error}");
    }
    Err(format!("nova doctor found {} issue(s)", report.errors.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn repository_stdlib_has_a_single_abi_boundary() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let report = inspect(&root).expect("stdlib audit must run");
        assert!(report.checked_files > 0);
        assert!(report.errors.is_empty(), "{report:#?}");
    }
}
