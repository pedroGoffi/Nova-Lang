//! Local-first package projects with Nova.toml and Nova.lock, plus embedded std.
//! Dependency resolution is intentionally path-only; remote registries require
//! a trust model and a real content-addressed resolver.
use crate::{
    ast::{self, Item},
    modules::{self, Loader, SourceFile},
};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
struct Manifest {
    name: String,
    version: String,
    entry: String,
    deps: Vec<(String, String)>,
}
fn value(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        Ok(raw[1..raw.len() - 1].to_owned())
    } else {
        Err(format!("expected quoted TOML string, got '{raw}'"))
    }
}
fn valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(),Some(c) if c.is_ascii_alphabetic()||c=='_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
fn manifest(root: &Path) -> Result<Manifest, String> {
    let path = root.join("Nova.toml");
    let raw = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut section = String::new();
    let mut package = HashMap::new();
    let mut deps = vec![];
    for (i, line) in raw.lines().enumerate() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_string();
            if section != "package" && section != "dependencies" {
                return Err(format!(
                    "{}:{}: unsupported section [{section}]",
                    path.display(),
                    i + 1
                ));
            }
            continue;
        }
        let (key, raw_value) = line
            .split_once('=')
            .ok_or_else(|| format!("{}:{}: expected key=value", path.display(), i + 1))?;
        let key = key.trim();
        let raw_value = raw_value.trim();
        match section.as_str() {
            "package" => {
                if !["name", "version", "entry"].contains(&key) {
                    return Err(format!("unknown [package] key '{key}'"));
                }
                if package.insert(key.to_string(), value(raw_value)?).is_some() {
                    return Err(format!("duplicate key '{key}'"));
                }
            }
            "dependencies" => {
                if !valid_identifier(key) {
                    return Err(format!("invalid dependency namespace '{key}'"));
                }
                if !raw_value.starts_with('{') || !raw_value.ends_with('}') {
                    return Err(format!("dependency '{key}' must use {{ path = \"...\" }}"));
                }
                let inside = raw_value[1..raw_value.len() - 1].trim();
                let (kind, raw) = inside.split_once('=').ok_or("invalid dependency table")?;
                let location = match kind.trim() {
                    "path" => value(raw)?,
                    "builtin" => {
                        let name = value(raw)?;
                        if key != "std" || name != "std" {
                            return Err(format!("error[NOVA-M015]: dependency alias '{key}' cannot expose the built-in STD '{name}'; declare `std = {{ builtin = \"std\" }}` instead. `std::internal` remains private to the standard library"));
                        }
                        "@builtin/std".to_string()
                    }
                    _ => {
                        return Err(format!(
                            "dependency '{key}' supports path or builtin, not remote sources"
                        ))
                    }
                };
                if deps.iter().any(|(k, _)| k == key) {
                    return Err(format!("duplicate dependency '{key}'"));
                }
                deps.push((key.to_string(), location));
            }
            _ => {
                return Err(format!(
                    "{}:{}: expected [package] or [dependencies]",
                    path.display(),
                    i + 1
                ))
            }
        }
    }
    let name = package.remove("name").ok_or("missing [package].name")?;
    if !valid_identifier(&name) {
        return Err(format!("invalid package name '{name}'"));
    }
    let version = package
        .remove("version")
        .ok_or("missing [package].version")?;
    if version.is_empty() {
        return Err("package version cannot be empty".into());
    }
    let entry = package.remove("entry").unwrap_or("src/main.nova".into());
    Ok(Manifest {
        name,
        version,
        entry,
        deps,
    })
}
fn fingerprint(data: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in data.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}
fn recursive(
    root: &Path,
    active: &mut HashSet<PathBuf>,
    lock: &mut Vec<String>,
    loader: &mut Loader,
    scope: &str,
    packages: &mut HashMap<PathBuf, String>,
    use_std: &mut bool,
) -> Result<ast::Program, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("{}: {e}", root.display()))?;
    if !active.insert(root.clone()) {
        return Err(format!(
            "cyclic path dependency involving {}",
            root.display()
        ));
    }
    let m = manifest(&root)?;
    let source_path = crate::editor_overlay::normalized_path(&root.join(&m.entry))
        .map_err(|e| format!("entry '{}': {e}", m.entry))?;
    if !source_path.starts_with(&root) {
        return Err("package entry escapes its root".into());
    }
    let source = crate::editor_overlay::read_to_string(&source_path)
        .map_err(|e| format!("{}: {e}", source_path.display()))?;
    let items = loader.load_file(&source_path, &root, scope)?;
    let mut ast = ast::Program { items };
    lock.push(format!(
        "{} {} path={} hash={}",
        m.name,
        m.version,
        root.display(),
        fingerprint(&source)
    ));
    let mut aliases = HashSet::new();
    for (alias, path) in &m.deps {
        if !aliases.insert(alias.clone()) {
            return Err(format!("duplicate alias '{alias}'"));
        }
        let namespace = if scope.is_empty() {
            alias.clone()
        } else {
            format!("{scope}::{alias}")
        };
        if path == "@builtin/std" {
            // There is one actual STD definition in the linked program.
            // Every package's `std` dependency is an alias of that definition,
            // not a nested copy with incompatible nominal types and drop glue.
            *use_std = true;
            if namespace != "std" {
                loader.dependency_aliases.insert(namespace, "std".into());
            }
            continue;
        }
        let dep_root = root
            .join(path)
            .canonicalize()
            .map_err(|e| format!("dependency '{alias}' ({}): {e}", root.join(path).display()))?;
        // A real dependency cycle is still an error, even when the package has
        // already been seen by the identity cache on the current DFS path.
        if active.contains(&dep_root) {
            return Err(format!(
                "cyclic path dependency involving {}",
                dep_root.display()
            ));
        }
        if let Some(existing) = packages.get(&dep_root) {
            // Multiple import routes of the same on-disk package must share
            // one struct identity, one set of functions and one destructor.
            loader
                .dependency_aliases
                .insert(namespace, existing.clone());
            continue;
        }
        packages.insert(dep_root.clone(), namespace.clone());
        loader.dependency_roots.insert(namespace.clone());
        let dep = recursive(
            &dep_root, active, lock, loader, &namespace, packages, use_std,
        )?;
        ast.items.insert(
            0,
            Item::Namespace {
                name: alias.clone(),
                items: dep.items,
            },
        );
    }
    active.remove(&root);
    Ok(ast)
}

/// The same loader is used for standalone sources, manifests, and tests.
pub fn load_detailed(path: &Path) -> Result<(ast::Program, Vec<SourceFile>), String> {
    let mut loader = Loader::default();
    if crate::editor_overlay::exists(path) && path.extension().is_some_and(|ext| ext == "nova") {
        let root = crate::editor_overlay::normalized_path(path)
            .map_err(|e| e.to_string())?
            .parent()
            .ok_or("source has no parent")?
            .to_owned();
        let items = loader.load_file(path, &root, "")?;
        let linked = modules::link(ast::Program { items }, &loader)?;
        return Ok((linked, loader.sources));
    }
    let mut lock =
        vec!["# Nova.lock v2 - path dependencies and source snapshots (non-cryptographic)".into()];
    let mut use_std = false;
    let mut program = recursive(
        path,
        &mut HashSet::new(),
        &mut lock,
        &mut loader,
        "",
        &mut HashMap::new(),
        &mut use_std,
    )?;
    if use_std {
        loader.dependency_roots.insert("std".into());
        loader.builtin_roots.insert("std".into());
        let mut all_sources = String::new();
        for (name, source) in modules::EMBEDDED_STD {
            all_sources.push_str(name);
            all_sources.push('\n');
            all_sources.push_str(source);
        }
        lock.push(format!(
            "std 0.20.0 builtin hash={}",
            fingerprint(&all_sources)
        ));
        program.items.insert(
            0,
            Item::Namespace {
                name: "std".into(),
                items: loader.embedded_std("std")?,
            },
        );
    }
    let program = modules::link(program, &loader)?;
    for source in &loader.sources {
        if !source.path.starts_with('<') {
            lock.push(format!(
                "source {} hash={}",
                source.path,
                fingerprint(&source.text)
            ));
        }
    }
    let lockfile = path.join("Nova.lock");
    // The LSP must never write Nova.lock just because a user typed a key.
    if std::env::var_os("NOVA_LSP_OVERLAY_MAP").is_none() {
        fs::write(&lockfile, format!("{}\n", lock.join("\n")))
            .map_err(|e| format!("{}: {e}", lockfile.display()))?;
    }
    Ok((program, loader.sources))
}
pub fn load(path: &Path) -> Result<ast::Program, String> {
    Ok(load_detailed(path)?.0)
}
pub fn new(name: &str) -> Result<(), String> {
    if !valid_identifier(name) {
        return Err("package name must be a valid Nova identifier".into());
    }
    let root = PathBuf::from(name);
    if root.exists() {
        return Err(format!("{} already exists", root.display()));
    }
    fs::create_dir_all(root.join("src")).map_err(|e| e.to_string())?;
    fs::write(root.join("Nova.toml"),format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nentry = \"src/main.nova\"\n\n[dependencies]\nstd = {{ builtin = \"std\" }}\n")).map_err(|e|e.to_string())?;
    fs::write(root.join("src/main.nova"),"fn main() -> i32 {\n    std::io::native_println(\"Hello from Nova!\");\n    return 0;\n}\n").map_err(|e|e.to_string())?;
    println!("Created Nova package '{name}'");
    Ok(())
}
pub fn init() -> Result<(), String> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    if root.join("Nova.toml").exists() {
        return Err("Nova.toml already exists".into());
    }
    let name = root
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("nova_app");
    let name = if valid_identifier(name) {
        name
    } else {
        "nova_app"
    };
    fs::create_dir_all(root.join("src")).map_err(|e| e.to_string())?;
    fs::write(root.join("Nova.toml"),format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nentry = \"src/main.nova\"\n\n[dependencies]\nstd = {{ builtin = \"std\" }}\n")).map_err(|e|e.to_string())?;
    if !root.join("src/main.nova").exists() {
        fs::write(
            root.join("src/main.nova"),
            "fn main() -> i32 { return 0; }\n",
        )
        .map_err(|e| e.to_string())?;
    }
    println!("Initialized package '{name}'");
    Ok(())
}
pub fn add_builtin_std() -> Result<(), String> {
    let file = Path::new("Nova.toml");
    let text = fs::read_to_string(file).map_err(|e| format!("Nova.toml: {e}"))?;
    let existing = manifest(Path::new("."))?;
    if existing.deps.iter().any(|(alias, _)| alias == "std") {
        return Err("dependency 'std' already exists".into());
    }
    let declaration = "std = { builtin = \"std\" }";
    let mut lines = Vec::new();
    let mut inserted = false;
    for line in text.lines() {
        lines.push(line.to_string());
        if line.trim() == "[dependencies]" {
            lines.push(declaration.to_string());
            inserted = true;
        }
    }
    if !inserted {
        lines.push("[dependencies]".into());
        lines.push(declaration.into());
    }
    fs::write(file, format!("{}\n", lines.join("\n"))).map_err(|e| e.to_string())?;
    println!("Added built-in Nova std");
    Ok(())
}
pub fn add(name: &str, path: &str) -> Result<(), String> {
    if !valid_identifier(name) {
        return Err("invalid dependency name".into());
    }
    if path.contains('"') || path.contains('\n') {
        return Err("invalid characters in dependency path".into());
    }
    let file = Path::new("Nova.toml");
    let text = fs::read_to_string(file).map_err(|e| format!("Nova.toml: {e}"))?;
    if !Path::new(path).join("Nova.toml").exists() {
        return Err(format!("dependency '{}' has no Nova.toml", path));
    }
    let mut found = false;
    let mut lines = vec![];
    for line in text.lines() {
        if line.trim() == "[dependencies]" {
            found = true;
            lines.push(line.into());
            lines.push(format!("{name} = {{ path = \"{path}\" }}"));
        } else {
            lines.push(line.to_owned());
        }
    }
    if !found {
        lines.push("[dependencies]".into());
        lines.push(format!("{name} = {{ path = \"{path}\" }}"));
    }
    let candidate = lines.join("\n");
    // Validate without mutating on duplicate dependency: inspect existing file first.
    let before = manifest(Path::new("."))?;
    if before.deps.iter().any(|(alias, _)| alias == name) {
        return Err(format!("dependency '{name}' already exists"));
    }
    fs::write(file, format!("{candidate}\n")).map_err(|e| e.to_string())?;
    println!("Added local dependency {name} -> {path}");
    Ok(())
}

pub fn tree(path: &Path) -> Result<(), String> {
    fn visit(root: &Path, prefix: &str, stack: &mut HashSet<PathBuf>) -> Result<(), String> {
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        if !stack.insert(root.clone()) {
            return Err(format!("cycle in package graph: {}", root.display()));
        }
        let m = manifest(&root)?;
        println!("{prefix}{} v{}", m.name, m.version);
        for (alias, path) in &m.deps {
            if path == "@builtin/std" {
                println!("{prefix}  [{alias}] std v0.9.3 (builtin)");
            } else {
                visit(&root.join(path), &format!("{prefix}  [{alias}] "), stack)?;
            }
        }
        stack.remove(&root);
        Ok(())
    }
    visit(path, "", &mut HashSet::new())
}
