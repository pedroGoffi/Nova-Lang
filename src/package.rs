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
    main: Option<String>,
    deps: Vec<(String, String)>,
    native_link: NativeLink,
}

#[derive(Debug, Clone, Default)]
pub struct NativeLink {
    /// Dynamic/system libraries passed to the platform linker as `-l<name>`.
    pub libraries: Vec<String>,
    /// Static archives passed by absolute/manifest-relative path.
    pub static_libraries: Vec<PathBuf>,
    /// Native library search directories (`-L`).
    pub search: Vec<PathBuf>,
    /// Runtime search directories (`-Wl,-rpath,...`).
    pub rpath: Vec<PathBuf>,
}

fn string_array(raw: &str) -> Result<Vec<String>, String> {
    let raw = raw.trim();
    if !raw.starts_with('[') || !raw.ends_with(']') {
        return Err(format!("expected TOML string array, got '{raw}'"));
    }
    let inner = raw[1..raw.len() - 1].trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    inner.split(',').map(|part| value(part.trim())).collect()
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
    let mut native_link = NativeLink::default();
    for (i, line) in raw.lines().enumerate() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_string();
            if section != "package" && section != "dependencies" && section != "link" {
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
                if !["name", "version", "entry", "main"].contains(&key) {
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
            "link" => {
                let values = string_array(raw_value)
                    .map_err(|e| format!("{}:{}: [link].{key}: {e}", path.display(), i + 1))?;
                match key {
                    "libraries" => native_link.libraries.extend(values),
                    "static" => native_link
                        .static_libraries
                        .extend(values.into_iter().map(PathBuf::from)),
                    "search" => native_link
                        .search
                        .extend(values.into_iter().map(PathBuf::from)),
                    "rpath" => native_link
                        .rpath
                        .extend(values.into_iter().map(PathBuf::from)),
                    _ => return Err(format!("unknown [link] key '{key}'")),
                }
            }
            _ => {
                return Err(format!(
                    "{}:{}: expected [package], [dependencies], or [link]",
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
    let main = package.remove("main");
    Ok(Manifest {
        name,
        version,
        entry,
        main,
        deps,
        native_link,
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

fn imported_name(path: &str) -> &str {
    path.split("::").next().unwrap_or(path)
}

/// Resolve an `import foo;` against configured Nova module roots. A root may
/// point at a package directory, a `src` directory, or a flat module tree.
/// This keeps the manifest focused on build metadata while imports describe
/// the source graph, like Go package imports.
fn search_import(loader: &Loader, name: &str) -> Option<(PathBuf, PathBuf)> {
    for root in &loader.search_paths {
        let package = root.join(name);
        let candidates = [
            (package.join("src/lib.nova"), package.clone()),
            (package.join("src/main.nova"), package.clone()),
            (package.join("lib.nova"), package.clone()),
            (package.join("main.nova"), package.clone()),
            (root.join(format!("{name}.nova")), root.clone()),
            (root.join(name).join("mod.nova"), root.clone()),
        ];
        if let Some((file, package_root)) = candidates
            .into_iter()
            .find(|(file, _)| crate::editor_overlay::exists(file))
        {
            return Some((file, package_root));
        }
    }
    None
}

fn load_search_imports(items: &mut Vec<Item>, loader: &mut Loader) -> Result<(), String> {
    let imports = items
        .iter()
        .filter_map(|item| match item {
            Item::Import(path) => Some(path.clone()),
            Item::Public(inner) => match inner.as_ref() {
                Item::Import(path) => Some(path.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut loaded = HashSet::new();
    for path in imports {
        let name = imported_name(&path);
        if name == "std" || !loaded.insert(name.to_owned()) {
            continue;
        }
        let Some((file, package_root)) = search_import(loader, name) else {
            continue;
        };
        let children = loader.load_file(&file, &package_root, name)?;
        items.insert(
            0,
            Item::Namespace {
                name: name.to_owned(),
                items: children,
            },
        );
    }
    Ok(())
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
    // A package distribution is intentionally source-free: package.nvi plus
    // lib<package>.a. Its interface is parsed into extern declarations so the
    // normal Nova semantic pipeline can typecheck calls without package sources.
    if !root.join("Nova.toml").is_file() && root.join("package.nvi").is_file() {
        let interface = read_interface(&root.join("package.nvi"))?;
        let expected = scope.rsplit("::").next().unwrap_or(scope);
        if !scope.is_empty() && expected != interface.package {
            return Err(format!(
                "dependency alias '{expected}' must match compiled package identity '{}' for this experimental ABI",
                interface.package
            ));
        }
        let mut declarations = String::new();
        for export in &interface.exports {
            if export.starts_with("pub extern fn ") || export.starts_with("pub struct ") {
                declarations.push_str(export);
                declarations.push('\n');
            } else if export.starts_with("fn ") || export.starts_with("extern fn ") {
                return Err(format!("package '{}' has an interface record that is not publicly importable: {export}", interface.package));
            } else {
                return Err(format!("package '{}' exports a declaration kind not supported by source-free imports yet: {export}", interface.package));
            }
        }
        if declarations.is_empty() {
            return Err(format!(
                "package '{}' has no supported public declarations",
                interface.package
            ));
        }
        let parsed = crate::grammar::ProgramParser::new()
            .parse(&crate::strip_comments(&declarations))
            .map_err(|error| {
                format!(
                    "invalid typed interface for package '{}': {error:?}",
                    interface.package
                )
            })?;
        let archive = root.join(format!("lib{}.a", interface.package));
        if !archive.is_file() {
            return Err(format!(
                "compiled package '{}' is missing archive {}",
                interface.package,
                archive.display()
            ));
        }
        lock.push(format!(
            "{} binary-interface={} archive={}",
            interface.package,
            interface.hash,
            archive.display()
        ));
        active.remove(&root);
        return Ok(parsed);
    }
    let m = manifest(&root)?;
    let source_path = crate::editor_overlay::normalized_path(&root.join(&m.entry))
        .map_err(|e| format!("entry '{}': {e}", m.entry))?;
    if !source_path.starts_with(&root) {
        return Err("package entry escapes its root".into());
    }
    let source = crate::editor_overlay::read_to_string(&source_path)
        .map_err(|e| format!("{}: {e}", source_path.display()))?;
    let mut items = loader.load_file(&source_path, &root, scope)?;
    load_search_imports(&mut items, loader)?;
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

/// Read native linker requirements from a package manifest. Paths are resolved
/// relative to the manifest directory, never relative to the caller's cwd.
pub fn native_link_config(path: &Path) -> Result<NativeLink, String> {
    let root = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    };
    if !root.join("Nova.toml").is_file() {
        return Ok(NativeLink::default());
    }

    fn visit(
        root: &Path,
        active: &mut HashSet<PathBuf>,
        visited: &mut HashSet<PathBuf>,
        result: &mut NativeLink,
    ) -> Result<(), String> {
        let root = root
            .canonicalize()
            .map_err(|e| format!("{}: {e}", root.display()))?;
        if visited.contains(&root) {
            return Ok(());
        }
        if !active.insert(root.clone()) {
            return Err(format!(
                "cyclic package dependency involving {}",
                root.display()
            ));
        }
        if !root.join("Nova.toml").is_file() && root.join("package.nvi").is_file() {
            let interface = read_interface(&root.join("package.nvi"))?;
            let archive = root.join(format!("lib{}.a", interface.package));
            if !archive.is_file() {
                return Err(format!(
                    "compiled package '{}' is missing archive {}",
                    interface.package,
                    archive.display()
                ));
            }
            result.static_libraries.push(archive);
            active.remove(&root);
            visited.insert(root);
            return Ok(());
        }
        let manifest = manifest(&root)?;
        let resolve = |items: Vec<PathBuf>| -> Vec<PathBuf> {
            items
                .into_iter()
                .map(|p| if p.is_absolute() { p } else { root.join(p) })
                .collect()
        };
        result.libraries.extend(manifest.native_link.libraries);
        result
            .static_libraries
            .extend(resolve(manifest.native_link.static_libraries));
        result.search.extend(resolve(manifest.native_link.search));
        result
            .rpath
            .extend(manifest.native_link.rpath.into_iter().map(|p| {
                let raw = p.to_string_lossy();
                if raw.starts_with("$ORIGIN") {
                    p
                } else if p.is_absolute() {
                    p
                } else {
                    root.join(p)
                }
            }));
        for (_, dependency) in manifest.deps {
            if dependency == "@builtin/std" {
                continue;
            }
            visit(&root.join(dependency), active, visited, result)?;
        }
        active.remove(&root);
        visited.insert(root);
        Ok(())
    }

    let mut result = NativeLink::default();
    visit(&root, &mut HashSet::new(), &mut HashSet::new(), &mut result)?;
    // Preserve dependency order: static linkers resolve archives from left to
    // right, so sorting these lists can silently create undefined references.
    fn dedup_stable<T: Eq + std::hash::Hash + Clone>(values: &mut Vec<T>) {
        let mut seen = HashSet::new();
        values.retain(|value| seen.insert(value.clone()));
    }
    dedup_stable(&mut result.libraries);
    dedup_stable(&mut result.static_libraries);
    dedup_stable(&mut result.search);
    dedup_stable(&mut result.rpath);
    Ok(result)
}

/// A decoded public package interface. The serialized representation deliberately
/// stores each declaration as UTF-8 hex: type debug strings may contain spaces,
/// punctuation and future syntax without making the file format ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInterface {
    pub package: String,
    pub hash: String,
    pub exports: Vec<String>,
}

fn hex_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn hex_decode(value: &str) -> Result<String, String> {
    if value.len() % 2 != 0 {
        return Err("odd-length hexadecimal field".into());
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let digit = |byte: u8| -> Option<u8> {
            match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            }
        };
        let high = digit(pair[0]).ok_or("invalid hexadecimal field")?;
        let low = digit(pair[1]).ok_or("invalid hexadecimal field")?;
        bytes.push((high << 4) | low);
    }
    String::from_utf8(bytes).map_err(|_| "interface field is not valid UTF-8".into())
}

/// Read and validate a `.nvi` file. The checksum is verified before a caller
/// can use its exports, preventing stale or partially-written interfaces from
/// silently entering semantic analysis.
pub fn read_interface(path: &Path) -> Result<PackageInterface, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut lines = raw.lines();
    if lines.next() != Some("NOVA-NVI 2") {
        return Err(format!(
            "{}: unsupported Nova interface version (expected NOVA-NVI 2)",
            path.display()
        ));
    }
    let package_line = lines.next().ok_or("truncated .nvi: missing package")?;
    let package_encoded = package_line
        .strip_prefix("package ")
        .ok_or("invalid .nvi package header")?;
    let package = hex_decode(package_encoded)?;
    if package.is_empty() {
        return Err("empty package name in .nvi".into());
    }
    let hash_line = lines
        .next()
        .ok_or("truncated .nvi: missing interface hash")?;
    let hash = hash_line
        .strip_prefix("interface-hash ")
        .ok_or("invalid .nvi interface hash header")?
        .to_owned();
    let count_line = lines.next().ok_or("truncated .nvi: missing export count")?;
    let count: usize = count_line
        .strip_prefix("exports ")
        .ok_or("invalid .nvi export count")?
        .parse()
        .map_err(|_| "invalid .nvi export count")?;
    // The record count must match the remaining physical records before we
    // allocate. This prevents a corrupt interface from requesting huge memory.
    let remaining_records = lines.clone().count();
    if count != remaining_records {
        return Err(format!(
            ".nvi declares {count} exports but contains {remaining_records} export records"
        ));
    }
    let mut exports = Vec::with_capacity(count);
    for _ in 0..count {
        let line = lines.next().ok_or("truncated .nvi: missing export")?;
        let encoded = line
            .strip_prefix("export ")
            .ok_or("invalid .nvi export record")?;
        exports.push(hex_decode(encoded)?);
    }
    if lines.next().is_some() {
        return Err("unexpected trailing records in .nvi".into());
    }
    if exports.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(".nvi exports must be strictly sorted and unique".into());
    }
    let actual_hash = fingerprint(&exports.join("\n"));
    if hash != actual_hash {
        return Err(format!(
            ".nvi checksum mismatch: expected {hash}, calculated {actual_hash}"
        ));
    }
    Ok(PackageInterface {
        package,
        hash,
        exports,
    })
}

fn interface_type(ty: &ast::TypeRef) -> String {
    match ty {
        ast::TypeRef::Named(name) => name.clone(),
        ast::TypeRef::Applied(name, args) => format!(
            "{}<{}>",
            name,
            args.iter()
                .map(interface_type)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ast::TypeRef::Array(inner, size) => format!("[{}; {}]", interface_type(inner), size),
        ast::TypeRef::Pointer { mutable, inner } => format!(
            "*{}{}",
            if *mutable { "mut " } else { "const " },
            interface_type(inner)
        ),
        ast::TypeRef::Reference { mutable, inner } => format!(
            "&{}{}",
            if *mutable { "mut " } else { "" },
            interface_type(inner)
        ),
        ast::TypeRef::Function { params, result } => format!(
            "fn({}) -> {}",
            params
                .iter()
                .map(interface_type)
                .collect::<Vec<_>>()
                .join(", "),
            interface_type(result)
        ),
    }
}

/// Emit deterministic package interfaces. Public functions use a source-like
/// declaration record that can be parsed back into typed extern declarations.
/// Other public item kinds remain descriptive records and are rejected by the
/// source-free importer until their nominal layout ABI is specified.
pub fn write_interfaces(path: &Path, program: &ast::Program) -> Result<Vec<PathBuf>, String> {
    fn collect(items: &[Item], output: &mut Vec<String>, prefix: &str) {
        for item in items {
            match item {
                Item::Public(inner) => {
                    match inner.as_ref() {
                        Item::Function(f) if f.generics.is_empty() && prefix.is_empty() => output
                            .push(format!(
                                "pub extern fn {}({}) -> {};",
                                f.name,
                                f.params
                                    .iter()
                                    .map(|p| format!("{}: {}", p.name, interface_type(&p.ty)))
                                    .collect::<Vec<_>>()
                                    .join(", "),
                                interface_type(&f.result)
                            )),
                        Item::Function(f) if !f.generics.is_empty() => output
                            .push(format!("unsupported generic function {}{}", prefix, f.name)),
                        Item::Function(f) => {
                            output.push(format!("unsupported nested function {}{}", prefix, f.name))
                        }
                        Item::Extern(f) => {
                            output.push(format!("unsupported extern export {}{}", prefix, f.name))
                        }
                        Item::Struct(x)
                            if prefix.is_empty()
                                && x.generics.is_empty()
                                && x.concept_bounds.is_empty()
                                && x.attributes.is_empty()
                                && !x.is_class
                                && x.parent.is_none()
                                && x.fields.iter().all(|field| {
                                    field.default.is_none() && field.attributes.is_empty()
                                }) =>
                        {
                            output.push(format!(
                                "pub struct {} {{ {} }}",
                                x.name,
                                x.fields
                                    .iter()
                                    .map(|field| format!(
                                        "{}: {}",
                                        field.name,
                                        interface_type(&field.ty)
                                    ))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }
                        Item::Struct(x) => output.push(format!(
                            "unsupported struct {}{}: generic, nested, attributed, class, inherited, or defaulted-field layouts are not in the source-free ABI",
                            prefix, x.name
                        )),
                        Item::Enum(x) => output.push(format!(
                            "enum {prefix}{} generics={:?} variants={:?}",
                            x.name, x.generics, x.variants
                        )),
                        Item::TypeAlias(x) => {
                            output.push(format!("type {prefix}{} = {:?}", x.name, x.target))
                        }
                        Item::Trait(x) => output.push(format!(
                            "trait {prefix}{} parents={:?} associated={:?}",
                            x.name, x.parents, x.associated_types
                        )),
                        Item::Concept(x) => output.push(format!(
                            "concept {prefix}{} generics={:?}",
                            x.name, x.generics
                        )),
                        Item::Namespace { name, items } => {
                            collect(items, output, &format!("{prefix}{name}::"))
                        }
                        _ => {}
                    }
                }
                Item::Namespace { name, items } => {
                    collect(items, output, &format!("{prefix}{name}::"))
                }
                _ => {}
            }
        }
    }
    let root = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(Path::new("."))
    };
    let target = root.join("target").join("nova").join("packages");
    let mut written = Vec::new();
    for item in &program.items {
        let Item::Namespace { name, items } = item else {
            continue;
        };
        if name == "std" {
            continue;
        }
        let mut exports = Vec::new();
        collect(items, &mut exports, "");
        exports.sort();
        exports.dedup();
        let hash = fingerprint(&exports.join("\n"));
        let package_dir = target.join(name.replace("::", "/"));
        fs::create_dir_all(&package_dir).map_err(|e| format!("{}: {e}", package_dir.display()))?;
        let interface = package_dir.join("package.nvi");
        let mut content = format!(
            "NOVA-NVI 2\npackage {}\ninterface-hash {hash}\nexports {}\n",
            hex_encode(name),
            exports.len()
        );
        // Package identifiers are also encoded to keep the record grammar simple.
        for export in &exports {
            content.push_str("export ");
            content.push_str(&hex_encode(export));
            content.push('\n');
        }
        if fs::read_to_string(&interface).ok().as_deref() != Some(content.as_str()) {
            let tmp = interface.with_extension("nvi.tmp");
            fs::write(&tmp, content).map_err(|e| format!("{}: {e}", tmp.display()))?;
            fs::rename(&tmp, &interface).map_err(|e| format!("{}: {e}", interface.display()))?;
        }
        // Validate our own output. This also makes writer/reader drift fail at build time.
        let parsed = read_interface(&interface)?;
        if parsed.package != *name || parsed.hash != hash || parsed.exports != exports {
            return Err(format!(
                "generated interface {} failed round-trip validation",
                interface.display()
            ));
        }
        written.push(interface);
    }
    Ok(written)
}

/// Load exactly one source package as an independent compilation unit. The
/// Stage 7 binary consumer path is handled by `recursive`, which synthesizes
/// extern declarations from `.nvi` without opening provider source files.
pub fn load_standalone_package(
    path: &Path,
) -> Result<(String, String, ast::Program, Vec<SourceFile>), String> {
    let root = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    };
    let root = root
        .canonicalize()
        .map_err(|e| format!("{}: {e}", root.display()))?;
    let manifest = manifest(&root)?;
    if !manifest.deps.is_empty() {
        let names = manifest
            .deps
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "package '{}' has Nova dependencies ({names}); independent package compilation currently requires dependency interfaces and ABI metadata. Build a dependency-free package for this Stage 6 compiler, or use `nova build` for the existing whole-program pipeline",
            manifest.name
        ));
    }
    let entry = crate::editor_overlay::normalized_path(&root.join(&manifest.entry))
        .map_err(|e| format!("entry '{}': {e}", manifest.entry))?;
    if !entry.starts_with(&root) {
        return Err("package entry escapes its root".into());
    }
    let mut loader = Loader::default();
    let items = loader.load_file(&entry, &root, &manifest.name)?;
    let program = modules::link(
        ast::Program {
            items: vec![ast::Item::Namespace {
                name: manifest.name.clone(),
                items,
            }],
        },
        &loader,
    )?;
    Ok((manifest.name, manifest.version, program, loader.sources))
}

/// The same loader is used for standalone sources, manifests, and tests.
pub fn load_detailed(path: &Path) -> Result<(ast::Program, Vec<SourceFile>), String> {
    load_detailed_with_paths(path, &[])
}

/// Load a package while allowing module/package roots outside Nova.toml.
/// `NOVA_PATH` follows the usual platform convention (`:` on Unix, `;` on
/// Windows); explicit CLI paths are appended and take precedence.
pub fn load_detailed_with_paths(
    path: &Path,
    module_paths: &[PathBuf],
) -> Result<(ast::Program, Vec<SourceFile>), String> {
    let mut loader = Loader::default();
    let mut search_paths = module_paths.to_vec();
    if let Some(raw) = std::env::var_os("NOVA_PATH") {
        search_paths.extend(std::env::split_paths(&raw));
    }
    loader.search_paths = search_paths
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    if crate::editor_overlay::exists(path) && path.extension().is_some_and(|ext| ext == "nova") {
        let root = crate::editor_overlay::normalized_path(path)
            .map_err(|e| e.to_string())?
            .parent()
            .ok_or("source has no parent")?
            .to_owned();
        let mut items = loader.load_file(path, &root, "")?;
        load_search_imports(&mut items, &mut loader)?;
        let linked = modules::link(ast::Program { items }, &loader)?;
        return Ok((linked, loader.sources));
    }
    let selected_main = manifest(path)?.main;
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
    let program = match selected_main {
        Some(method) => crate::entrypoint::generate(&program, &method)?,
        None => program,
    };
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
    fs::write(
        root.join("src/main.nova"),
        "fn main() -> i32 {\n    std::old::io::println(\"Hello from Nova!\");\n    return 0;\n}\n",
    )
    .map_err(|e| e.to_string())?;
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

#[cfg(test)]
mod native_link_tests {
    use super::*;

    #[test]
    fn writes_stable_nvi_interfaces_for_public_package_items() {
        let root = std::env::temp_dir().join(format!("nova-nvi-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let program = ast::Program {
            items: vec![Item::Namespace {
                name: "math".into(),
                items: vec![Item::Public(Box::new(Item::TypeAlias(ast::TypeAlias {
                    name: "Scalar".into(),
                    target: ast::TypeRef::Named("f64".into()),
                })))],
            }],
        };
        let files = write_interfaces(&root, &program).unwrap();
        assert_eq!(files.len(), 1);
        let first = fs::read_to_string(&files[0]).unwrap();
        assert!(first.starts_with("NOVA-NVI 2\npackage 6d617468\n"));
        let parsed = read_interface(&files[0]).unwrap();
        assert_eq!(parsed.package, "math");
        assert_eq!(
            parsed.exports,
            vec!["type Scalar = Named(\"f64\")".to_string()]
        );
        write_interfaces(&root, &program).unwrap();
        assert_eq!(fs::read_to_string(&files[0]).unwrap(), first);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn build_plan_accepts_source_free_binary_dependency() {
        let root = std::env::temp_dir().join(format!("nova-binary-dep-{}", std::process::id()));
        let app = root.join("app");
        let dep = app.join("vendor/math_core");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(app.join("src")).unwrap();
        fs::create_dir_all(&dep).unwrap();
        fs::write(app.join("Nova.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nmath_core = { path = \"vendor/math_core\" }\n").unwrap();
        let export = "pub extern fn add(left: i32, right: i32) -> i32;";
        let hash = fingerprint(export);
        let nvi = format!(
            "NOVA-NVI 2\npackage {}\ninterface-hash {hash}\nexports 1\nexport {}\n",
            hex_encode("math_core"),
            hex_encode(export)
        );
        fs::write(dep.join("package.nvi"), nvi).unwrap();
        fs::write(dep.join("libmath_core.a"), b"archive fixture").unwrap();
        let plan = build_plan(&app).unwrap();
        assert_eq!(plan.packages.len(), 2);
        assert_eq!(plan.packages[0].name, "math_core");
        assert_eq!(plan.packages[0].version, "binary");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_free_package_imports_public_struct_layouts() {
        let root = std::env::temp_dir().join(format!("nova-binary-struct-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let export = "pub struct Point { x: i32, y: f64 }";
        let hash = fingerprint(export);
        let nvi = format!(
            "NOVA-NVI 2\\npackage {}\\ninterface-hash {hash}\\nexports 1\\nexport {}\\n",
            hex_encode("geometry"),
            hex_encode(export)
        );
        fs::write(root.join("package.nvi"), nvi).unwrap();
        fs::write(root.join("libgeometry.a"), b"archive fixture").unwrap();

        let imported = recursive(
            &root,
            &mut HashSet::new(),
            &mut Vec::new(),
            &mut crate::modules::Loader::default(),
            "geometry",
            &mut HashMap::new(),
            &mut false,
        )
        .unwrap();

        assert!(matches!(
            imported.items.first(),
            Some(Item::Public(inner))
                if matches!(inner.as_ref(), Item::Struct(structure)
                    if structure.name == "Point" && structure.fields.len() == 2)
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn public_struct_interfaces_round_trip_as_typed_declarations() {
        let root = std::env::temp_dir().join(format!("nova-nvi-structs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let source = "pub struct Point { x: i32, y: f64 }";
        let parsed = crate::grammar::ProgramParser::new().parse(source).unwrap();
        let program = ast::Program {
            items: vec![Item::Namespace {
                name: "geometry".into(),
                items: parsed.items,
            }],
        };

        let files = write_interfaces(&root, &program).unwrap();
        let interface = read_interface(&files[0]).unwrap();
        assert_eq!(
            interface.exports,
            vec!["pub struct Point { x: i32, y: f64 }".to_string()]
        );

        let imported = crate::grammar::ProgramParser::new()
            .parse(&interface.exports.join("\\n"))
            .unwrap();
        assert!(matches!(
            imported.items.first(),
            Some(Item::Public(inner))
                if matches!(inner.as_ref(), Item::Struct(structure)
                    if structure.name == "Point"
                        && structure.fields.len() == 2
                        && structure.fields[0].name == "x")
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn public_function_interfaces_are_parseable_extern_declarations() {
        let root = std::env::temp_dir().join(format!("nova-nvi-functions-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = "pub fn add(left: i32, right: i32) -> i32 { return left + right; }";
        let parsed = crate::grammar::ProgramParser::new().parse(source).unwrap();
        let program = ast::Program {
            items: vec![Item::Namespace {
                name: "math_core".into(),
                items: parsed.items,
            }],
        };
        let files = write_interfaces(&root, &program).unwrap();
        let interface = read_interface(&files[0]).unwrap();
        assert_eq!(interface.package, "math_core");
        assert_eq!(
            interface.exports,
            vec!["pub extern fn add(left: i32, right: i32) -> i32;".to_string()]
        );
        let declarations = interface.exports.join("\n");
        let imported = crate::grammar::ProgramParser::new()
            .parse(&declarations)
            .unwrap();
        assert!(
            matches!(imported.items.first(), Some(Item::Public(inner)) if matches!(inner.as_ref(), Item::Extern(_)))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_native_link_string_arrays() {
        assert_eq!(
            string_array(r#"["m", "SDL2"]"#).unwrap(),
            vec!["m".to_string(), "SDL2".to_string()]
        );
        assert!(string_array(r#"["unterminated]"#).is_err());
    }

    #[test]
    fn reads_native_link_configuration_relative_to_manifest() {
        let root =
            std::env::temp_dir().join(format!("nova-native-link-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("Nova.toml"),
            r#"[package]
name = "native_test"
version = "0.1.0"

[dependencies]

[link]
libraries = ["math_extra"]
static = ["native/libhelper.a"]
search = ["native/lib"]
rpath = ["native/lib"]
"#,
        )
        .unwrap();

        let config = native_link_config(&root).unwrap();
        assert_eq!(config.libraries, vec!["math_extra"]);
        assert_eq!(
            config.static_libraries,
            vec![root.join("native/libhelper.a")]
        );
        assert_eq!(config.search, vec![root.join("native/lib")]);
        assert_eq!(config.rpath, vec![root.join("native/lib")]);

        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn collects_transitive_native_link_requirements_in_link_order() {
        let root = std::env::temp_dir().join(format!("nova-native-graph-{}", std::process::id()));
        let app = root.join("app");
        let dep = root.join("dep");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(app.join("src")).unwrap();
        fs::create_dir_all(dep.join("src")).unwrap();
        fs::write(
            app.join("Nova.toml"),
            r#"[package]
name = "app"
version = "0.1.0"
[dependencies]
dep = { path = "../dep" }
[link]
libraries = ["app_native"]
"#,
        )
        .unwrap();
        fs::write(
            dep.join("Nova.toml"),
            r#"[package]
name = "dep"
version = "0.1.0"
[link]
libraries = ["dep_native"]
"#,
        )
        .unwrap();
        let config = native_link_config(&app).unwrap();
        assert_eq!(config.libraries, vec!["app_native", "dep_native"]);
        fs::remove_dir_all(root).unwrap();
    }
}

/// One node in the resolved local package graph. `root` is canonical so two
/// aliases pointing at the same directory share one compilation identity.
#[derive(Debug, Clone)]
pub struct PackageNode {
    pub name: String,
    pub version: String,
    pub root: PathBuf,
    pub dependencies: Vec<PathBuf>,
    pub source_files: Vec<PathBuf>,
    pub fingerprint: String,
}

/// Dependency-first deterministic package build order. This is deliberately
/// separate from AST expansion: it is the stable input to the per-package
/// compiler/artifact cache introduced by the native package backend.
#[derive(Debug, Clone, Default)]
pub struct PackageBuildPlan {
    pub packages: Vec<PackageNode>,
}

fn package_sources(root: &Path, entry: &str) -> Result<Vec<PathBuf>, String> {
    fn walk(dir: &Path, root: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            let relative = path.strip_prefix(root).unwrap_or(&path);
            if relative.components().any(|part| matches!(part, std::path::Component::Normal(x) if x == "target" || x == ".git")) {
                continue;
            }
            if path.is_dir() {
                walk(&path, root, output)?;
            } else if path.extension().is_some_and(|ext| ext == "nova") {
                output.push(path);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    let source_root = root.join("src");
    if source_root.is_dir() {
        walk(&source_root, root, &mut files)?;
    }
    let entry_path = root.join(entry);
    if entry_path.is_file() && !files.contains(&entry_path) {
        files.push(entry_path);
    }
    files.sort();
    Ok(files)
}

/// Resolve the package graph without expanding dependency source into the
/// consumer AST. Packages are deduplicated by canonical filesystem identity.
pub fn build_plan(path: &Path) -> Result<PackageBuildPlan, String> {
    let root = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    };
    if !root.join("Nova.toml").is_file() {
        return Ok(PackageBuildPlan::default());
    }
    fn visit(
        root: &Path,
        active: &mut HashSet<PathBuf>,
        emitted: &mut HashSet<PathBuf>,
        output: &mut Vec<PackageNode>,
    ) -> Result<(), String> {
        let root = root
            .canonicalize()
            .map_err(|e| format!("{}: {e}", root.display()))?;
        if emitted.contains(&root) {
            return Ok(());
        }
        if !active.insert(root.clone()) {
            return Err(format!(
                "cyclic package dependency involving {}",
                root.display()
            ));
        }
        if !root.join("Nova.toml").is_file() && root.join("package.nvi").is_file() {
            let interface = read_interface(&root.join("package.nvi"))?;
            let archive = root.join(format!("lib{}.a", interface.package));
            if !archive.is_file() {
                return Err(format!(
                    "binary dependency '{}' is missing {}",
                    interface.package,
                    archive.display()
                ));
            }
            let node = PackageNode {
                name: interface.package,
                version: "binary".into(),
                root: root.clone(),
                dependencies: Vec::new(),
                source_files: Vec::new(),
                fingerprint: interface.hash,
            };
            active.remove(&root);
            emitted.insert(root);
            output.push(node);
            return Ok(());
        }
        let m = manifest(&root)?;
        let mut dependencies = Vec::new();
        for (_, location) in &m.deps {
            if location == "@builtin/std" {
                continue;
            }
            let dep = root.join(location).canonicalize().map_err(|e| {
                format!(
                    "dependency '{}' ({}): {e}",
                    location,
                    root.join(location).display()
                )
            })?;
            if !dep.join("Nova.toml").is_file() && !(dep.join("package.nvi").is_file()) {
                return Err(format!(
                    "dependency {} has neither Nova.toml nor a compiled package.nvi",
                    dep.display()
                ));
            }
            dependencies.push(dep);
        }
        dependencies.sort();
        dependencies.dedup();
        for dependency in &dependencies {
            visit(dependency, active, emitted, output)?;
        }
        let sources = package_sources(&root, &m.entry)?;
        let manifest_text =
            fs::read_to_string(root.join("Nova.toml")).map_err(|e| e.to_string())?;
        let mut payload = format!(
            "{}\n{}\nentry={}\n{}\n",
            m.name, m.version, m.entry, manifest_text
        );
        for source in &sources {
            let content = fs::read(source).map_err(|e| format!("{}: {e}", source.display()))?;
            payload.push_str(
                &source
                    .strip_prefix(&root)
                    .unwrap_or(source)
                    .display()
                    .to_string(),
            );
            payload.push('\n');
            payload.push_str(&fingerprint(&String::from_utf8_lossy(&content)));
            payload.push('\n');
        }
        for dependency in &dependencies {
            payload.push_str(&dependency.display().to_string());
            payload.push(' ');
            if let Some(child) = output.iter().find(|node| node.root == *dependency) {
                payload.push_str(&child.fingerprint);
            }
            payload.push('\n');
        }
        let node = PackageNode {
            name: m.name,
            version: m.version,
            root: root.clone(),
            dependencies,
            source_files: sources,
            fingerprint: fingerprint(&payload),
        };
        active.remove(&root);
        emitted.insert(root);
        output.push(node);
        Ok(())
    }
    let mut plan = PackageBuildPlan::default();
    visit(
        &root,
        &mut HashSet::new(),
        &mut HashSet::new(),
        &mut plan.packages,
    )?;
    Ok(plan)
}

/// Persist the resolved graph and per-package fingerprints. These records are
/// inputs to incremental package compilation; they do not pretend that an
/// archive has been produced before the independent MIR/object backend exists.
pub fn write_build_plan(path: &Path, plan: &PackageBuildPlan) -> Result<Vec<PathBuf>, String> {
    let app_root = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(Path::new("."))
    };
    let mut written = Vec::new();
    for (index, package) in plan.packages.iter().enumerate() {
        let package_key = format!(
            "{}-{}",
            package.name,
            fingerprint(&package.root.display().to_string())[..8].to_string()
        );
        let output = app_root.join("target/nova/packages").join(package_key);
        fs::create_dir_all(&output).map_err(|e| format!("{}: {e}", output.display()))?;
        let deps = package
            .dependencies
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>();
        let mut content = format!("NOVA-PACKAGE 1\nname {}\nversion {}\nfingerprint {}\nbuild-order {}\nroot {}\ndependencies {}\n",
            package.name, package.version, package.fingerprint, index, package.root.display(), deps.len());
        for dep in deps {
            content.push_str(&format!("dependency {dep}\n"));
        }
        content.push_str(&format!("sources {}\n", package.source_files.len()));
        for source in &package.source_files {
            content.push_str(&format!(
                "source {}\n",
                source
                    .strip_prefix(&package.root)
                    .unwrap_or(source)
                    .display()
            ));
        }
        let metadata = output.join("package.nvp");
        if fs::read_to_string(&metadata).ok().as_deref() != Some(content.as_str()) {
            let temporary = metadata.with_extension("nvp.tmp");
            fs::write(&temporary, content).map_err(|e| format!("{}: {e}", temporary.display()))?;
            fs::rename(&temporary, &metadata)
                .map_err(|e| format!("{}: {e}", metadata.display()))?;
        }
        written.push(metadata);
    }
    Ok(written)
}

#[cfg(test)]
mod package_build_plan_tests {
    use super::*;

    #[test]
    fn build_plan_is_dependency_first_and_tracks_transitive_source_changes() {
        let root = std::env::temp_dir().join(format!("nova-package-plan-{}", std::process::id()));
        let app = root.join("app");
        let dep = root.join("dep");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(app.join("src")).unwrap();
        fs::create_dir_all(dep.join("src")).unwrap();
        fs::write(app.join("Nova.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\ndep = { path = \"../dep\" }\n").unwrap();
        fs::write(
            dep.join("Nova.toml"),
            "[package]\nname = \"dep\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(app.join("src/main.nova"), "fn main() -> i32 { return 0; }").unwrap();
        fs::write(
            dep.join("src/lib.nova"),
            "pub fn answer() -> i32 { return 42; }",
        )
        .unwrap();

        let first = build_plan(&app).unwrap();
        assert_eq!(first.packages.len(), 2);
        assert_eq!(first.packages[0].name, "dep");
        assert_eq!(first.packages[1].name, "app");
        let app_hash = first.packages[1].fingerprint.clone();

        fs::write(
            dep.join("src/lib.nova"),
            "pub fn answer() -> i32 { return 43; }",
        )
        .unwrap();
        let second = build_plan(&app).unwrap();
        assert_ne!(
            second.packages[0].fingerprint,
            first.packages[0].fingerprint
        );
        assert_ne!(second.packages[1].fingerprint, app_hash);
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod package_interface_v2_tests {
    use super::*;

    #[test]
    fn interface_v2_round_trips_utf8_and_punctuation() {
        let root = std::env::temp_dir().join(format!("nova-nvi-v2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let exports = vec![
            "fn api::answer(x:i32) -> i32".to_string(),
            "struct api::Result fields=[(\"value\", Option<String>)]".to_string(),
            "type api::Texto = String // ação".to_string(),
        ];
        let mut sorted = exports.clone();
        sorted.sort();
        let hash = fingerprint(&sorted.join("\n"));
        let mut raw = format!(
            "NOVA-NVI 2\npackage {}\ninterface-hash {hash}\nexports {}\n",
            hex_encode("sample::api"),
            sorted.len()
        );
        for export in &sorted {
            raw.push_str(&format!("export {}\n", hex_encode(export)));
        }
        let path = root.join("package.nvi");
        fs::write(&path, raw).unwrap();
        let parsed = read_interface(&path).unwrap();
        assert_eq!(parsed.package, "sample::api");
        assert_eq!(parsed.exports, sorted);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interface_v2_rejects_tampering_and_trailing_data() {
        let root = std::env::temp_dir().join(format!("nova-nvi-invalid-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("package.nvi");
        let export = "fn answer() -> i32";
        let hash = fingerprint(export);
        let valid = format!(
            "NOVA-NVI 2\npackage {}\ninterface-hash {hash}\nexports 1\nexport {}\n",
            hex_encode("math"),
            hex_encode(export)
        );
        fs::write(
            &path,
            valid.replace(&hex_encode(export), &hex_encode("fn answer() -> i64")),
        )
        .unwrap();
        assert!(read_interface(&path)
            .unwrap_err()
            .contains("checksum mismatch"));
        fs::write(&path, format!("{valid}unexpected\n")).unwrap();
        assert!(read_interface(&path).unwrap_err().contains("trailing"));
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod standalone_package_tests {
    use super::*;

    #[test]
    fn standalone_loader_uses_package_identity_and_rejects_dependencies() {
        let root =
            std::env::temp_dir().join(format!("nova-standalone-package-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("Nova.toml"),
            r#"[package]
name = "math_core"
version = "0.1.0"
entry = "src/lib.nova"
"#,
        )
        .unwrap();
        fs::write(
            root.join("src/lib.nova"),
            "pub fn add(a: i32, b: i32) -> i32 { return a + b; }",
        )
        .unwrap();

        let (name, version, program, _) = load_standalone_package(&root).unwrap();
        assert_eq!(name, "math_core");
        assert_eq!(version, "0.1.0");
        assert!(!program.items.is_empty());

        fs::write(
            root.join("Nova.toml"),
            r#"[package]
name = "math_core"
version = "0.1.0"
[dependencies]
other = { path = "../other" }
"#,
        )
        .unwrap();
        assert!(load_standalone_package(&root)
            .unwrap_err()
            .contains("has Nova dependencies"));
        fs::remove_dir_all(root).unwrap();
    }
}
