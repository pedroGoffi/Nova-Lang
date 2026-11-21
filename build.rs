use std::{env, fs, path::{Path, PathBuf}};

// Keep embedded STD in sync with the actual Nova sources. File additions and
// deletions are recognized automatically on the next Cargo build.
fn collect_nova_sources(root: &Path, directory: &Path, paths: &mut Vec<String>) -> std::io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_nova_sources(root, &path, paths)?;
        } else if path.extension().is_some_and(|extension| extension == "nova") {
            let relative = path.strip_prefix(root).expect("STD source under std/src");
            paths.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

fn embed_std_sources() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("std/src");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut paths = Vec::new();
    collect_nova_sources(&root, &root, &mut paths)?;
    paths.sort();
    paths.dedup();
    if !paths.iter().any(|path| path == "core/manifest.nova") {
        return Err("std/src/core/manifest.nova is missing".into());
    }
    let mut generated = String::from("&[\n");
    for path in &paths {
        // The generated file lives under OUT_DIR. Resolve file bytes relative
        // to the crate, not to OUT_DIR or the process' working directory.
        generated.push_str(&format!(
            "    ({path:?}, include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/std/src/\", {path:?}))),\n"
        ));
    }
    generated.push_str("]\n");
    let output = PathBuf::from(env::var("OUT_DIR")?).join("nova_embedded_std.rs");
    // Avoid touching generated files whose content has not changed. This
    // matters for incremental Cargo builds and downstream rustc invocations.
    if fs::read_to_string(&output).ok().as_deref() != Some(generated.as_str()) {
        fs::write(output, generated)?;
    }
    Ok(())
}

fn main() {
    embed_std_sources().expect("failed to embed the Nova standard library");

    // `lalrpop_mod!(pub grammar)` expects `$OUT_DIR/grammar.rs`.
    //
    // Cargo reruns this build script when STD sources change because they are
    // embedded above. Do not pay the full LALRPOP generation cost on every
    // STD edit: the parser only needs regeneration when its own input is
    // newer or when Cargo has no generated parser yet.
    let grammar = PathBuf::from("src/grammar.lalrpop");
    let generated = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"))
        .join("grammar.rs");
    println!("cargo:rerun-if-changed={}", grammar.display());

    let grammar_updated = fs::metadata(&grammar)
        .and_then(|metadata| metadata.modified())
        .ok();
    let parser_updated = fs::metadata(&generated)
        .and_then(|metadata| metadata.modified())
        .ok();
    let needs_parser = match (grammar_updated, parser_updated) {
        (Some(grammar_time), Some(parser_time)) => grammar_time > parser_time,
        _ => true,
    };

    if needs_parser {
        lalrpop::Configuration::new()
            .use_cargo_dir_conventions()
            .process()
            .expect("failed to generate the Nova parser from src/grammar.lalrpop");
    } else {
        println!("cargo:warning=Nova parser is up to date; skipping LALRPOP generation");
    }
}
