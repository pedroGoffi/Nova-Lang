//! File-backed modules, lexical imports, visibility and source ownership.
//! The parser still owns syntax; this loader never concatenates source strings.
use crate::{
    ast::{self, Expr, Item, Pattern, Stmt, TypeRef},
    diagnostics, grammar, strip_comments,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
    pub namespace: String,
}

#[derive(Default)]
pub struct Loader {
    pub sources: Vec<SourceFile>,
    active: Vec<PathBuf>,
    loaded: HashSet<PathBuf>,
    pub module_paths: HashSet<String>,
    pub dependency_roots: HashSet<String>,
    pub builtin_roots: HashSet<String>,
    // Dependency paths that refer to an already-loaded package (including
    // the singleton built-in STD). These aliases never create duplicate AST.
    pub dependency_aliases: HashMap<String, String>,
}

fn qualified(ns: &str, name: &str) -> String {
    if ns.is_empty() {
        name.to_owned()
    } else {
        format!("{ns}::{name}")
    }
}
fn bare(item: &Item) -> (&Item, bool) {
    if let Item::Public(inner) = item {
        (inner.as_ref(), true)
    } else {
        (item, false)
    }
}
fn pub_wrap(item: Item, public: bool) -> Item {
    if public {
        Item::Public(Box::new(item))
    } else {
        item
    }
}
// Embedded standard library is a recursive module tree, not one main.nova.
// Each file is embedded individually, then resolved through public mod declarations.
// Generated at compile time from std/src/**/*.nova by build.rs.
// New modules are embedded automatically, eliminating registry drift.
pub(crate) const EMBEDDED_STD: &[(&str, &str)] =
    include!(concat!(env!("OUT_DIR"), "/nova_embedded_std.rs"));

// Keep the source list in sync with std/src. Missing entries produce a
// module-loading failure that may only appear during bootstrap, so detect
// them in regular Rust tests instead of waiting for the selfhost build.
#[cfg(test)]
mod embedded_std_registry_tests {
    use super::{Loader, EMBEDDED_STD};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    fn collect_sources(dir: &Path, base: &Path, paths: &mut HashSet<String>) {
        for item in std::fs::read_dir(dir).expect("read standard library directory") {
            let path = item.expect("read standard library entry").path();
            if path.is_dir() {
                collect_sources(&path, base, paths);
            } else if path.extension().is_some_and(|ext| ext == "nova") {
                let relative = path.strip_prefix(base).expect("path inside std/src");
                paths.insert(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    #[test]
    fn every_standard_library_source_is_embedded_exactly_once() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("std/src");
        let mut expected = HashSet::new();
        collect_sources(&base, &base, &mut expected);
        let mut actual = HashSet::new();
        for (path, _) in EMBEDDED_STD {
            assert!(actual.insert((*path).to_string()), "duplicate embedded std module: {path}");
        }
        assert_eq!(actual, expected, "EMBEDDED_STD must match std/src/**/*.nova");
    }

    #[test]
    fn builtin_char_is_loaded_with_the_standard_library() {
        let mut loader = Loader::default();
        let _items = loader.embedded_std("std").expect("built-in standard library must load");
        assert!(loader.module_paths.contains("std::char"), "missing std::char namespace");
        assert!(loader.sources.iter().any(|source| source.path == "<builtin:std/src/char.nova>"),
            "char.nova must be included as its own source");
    }
}

impl Loader {
    pub fn load_file(
        &mut self,
        file: &Path,
        package_root: &Path,
        namespace: &str,
    ) -> Result<Vec<Item>, String> {
        let root = package_root
            .canonicalize()
            .map_err(|e| format!("{}: {e}", package_root.display()))?;
        let file = crate::editor_overlay::normalized_path(file)
            .map_err(|e| format!("{}: {e}", file.display()))?;
        if !file.starts_with(&root) {
            return Err(format!(
                "error[NOVA-M001]: module {} escapes package root {}",
                file.display(),
                root.display()
            ));
        }
        if self.active.contains(&file) {
            let chain = self
                .active
                .iter()
                .chain(std::iter::once(&file))
                .map(|x| x.display().to_string())
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(format!(
                "error[NOVA-M002]: cyclic module declaration: {chain}"
            ));
        }
        if !self.loaded.insert(file.clone()) {
            return Err(format!(
                "error[NOVA-M003]: file '{}' declared as a module more than once",
                file.display()
            ));
        }
        self.active.push(file.clone());
        let result = self.parse_and_expand(&file, &root, namespace);
        self.active.pop();
        result
    }

    fn parse_and_expand(
        &mut self,
        file: &Path,
        root: &Path,
        namespace: &str,
    ) -> Result<Vec<Item>, String> {
        let text = crate::editor_overlay::read_to_string(file)
            .map_err(|e| format!("{}: {e}", file.display()))?;
        // Optional stage-1 syntax pass implemented in Nova itself. Rust
        // LALRPOP still materializes the semantic AST for HIR/MIR/codegen.
        if let Some(binary) = std::env::var_os("NOVA_SELFHOST_PARSER") {
            let output = Command::new(&binary)
                .env("NOVA_SOURCE", crate::editor_overlay::source_path(file))
                .output()
                .map_err(|e| format!("unable to launch selfhost parser {:?}: {e}", binary))?;
            if !output.status.success() {
                return Err(format!(
                    "selfhost parser rejected {}:\n{}{}",
                    file.display(),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
        let code = strip_comments(&text);
        let parsed = grammar::ProgramParser::new()
            .parse(&code)
            .map_err(|e| diagnostics::parser(&file.display().to_string(), &text, e))?;
        self.sources.push(SourceFile {
            path: file.display().to_string(),
            text: text.clone(),
            namespace: namespace.into(),
        });
        let mut result = Vec::new();
        let mut names = HashSet::new();
        for item in parsed.items {
            let (inner, public) = bare(&item);
            if let Item::Mod { name, at } = inner {
                if !names.insert(name.clone()) {
                    return Err(diagnostics::render(
                        "NOVA-M004",
                        &file.display().to_string(),
                        &text,
                        *at,
                        name.len(),
                        &format!("duplicate module '{name}'"),
                    ));
                }
                let dir = file.parent().ok_or("module has no containing directory")?;
                let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                // foo.nova's children are in foo/, while main/lib/mod.nova's
                // children live next to the declaring source file.
                let dir = if ["main", "lib", "mod"].contains(&stem) {
                    dir.to_path_buf()
                } else {
                    dir.join(stem)
                };
                let direct = dir.join(format!("{name}.nova"));
                let nested = dir.join(name).join("mod.nova");
                let matches = [direct.as_path(), nested.as_path()]
                    .iter()
                    .filter(|p| crate::editor_overlay::exists(p))
                    .copied()
                    .collect::<Vec<_>>();
                let target = match matches.len() {
                    1 => matches[0],
                    0 => {
                        return Err(diagnostics::render(
                            "NOVA-M005",
                            &file.display().to_string(),
                            &text,
                            *at,
                            name.len(),
                            &format!(
                                "module '{name}' not found; expected {} or {}",
                                direct.display(),
                                nested.display()
                            ),
                        ))
                    }
                    _ => {
                        return Err(diagnostics::render(
                            "NOVA-M006",
                            &file.display().to_string(),
                            &text,
                            *at,
                            name.len(),
                            &format!(
                                "ambiguous module '{name}': both {} and {} exist",
                                direct.display(),
                                nested.display()
                            ),
                        ))
                    }
                };
                self.module_paths.insert(qualified(namespace, name));
                let children = self
                    .load_file(target, root, &qualified(namespace, name))
                    .map_err(|e| {
                        if e.starts_with("error[NOVA-M001]")
                            || e.starts_with("error[NOVA-M002]")
                            || e.starts_with("error[NOVA-M003]")
                        {
                            diagnostics::render(
                                "NOVA-M014",
                                &file.display().to_string(),
                                &text,
                                *at,
                                name.len(),
                                &format!("cannot load module '{name}': {e}"),
                            )
                        } else {
                            e
                        }
                    })?;
                result.push(pub_wrap(
                    Item::Namespace {
                        name: name.clone(),
                        items: children,
                    },
                    public,
                ));
            } else {
                result.push(item);
            }
        }
        Ok(result)
    }

    /// Load nested builtin STD modules with the same `pub mod foo;` layout
    /// used by filesystem packages. No source concatenation or main.nova.
    fn embedded_module(
        &mut self,
        module_path: &str,
        namespace: &str,
        visiting: &mut HashSet<String>,
    ) -> Result<Vec<Item>, String> {
        if !visiting.insert(module_path.to_owned()) {
            return Err(format!("cycle in built-in std modules: {module_path}"));
        }
        let source = EMBEDDED_STD
            .iter()
            .find(|(path, _)| *path == module_path)
            .map(|(_, text)| *text)
            .ok_or_else(|| format!("missing embedded std module '{module_path}'"))?;
        let display_path = format!("<builtin:std/src/{module_path}>");
        let parsed = grammar::ProgramParser::new()
            .parse(&strip_comments(source))
            .map_err(|e| diagnostics::parser(&display_path, source, e))?;
        self.sources.push(SourceFile {
            path: display_path,
            text: source.into(),
            namespace: namespace.into(),
        });
        let mut result = Vec::new();
        for item in parsed.items {
            let (inner, public) = bare(&item);
            if let Item::Mod { name, .. } = inner {
                let directory = module_path
                    .rsplit_once('/')
                    .map(|(dir, _)| format!("{dir}/"))
                    .unwrap_or_default();
                let direct = format!("{directory}{name}.nova");
                let nested = format!("{directory}{name}/mod.nova");
                let candidates = [direct.as_str(), nested.as_str()];
                let choices = candidates
                    .iter()
                    .filter(|candidate| EMBEDDED_STD.iter().any(|(path, _)| path == *candidate))
                    .collect::<Vec<_>>();
                let next = match choices.as_slice() {
                    [only] => (*only).to_string(),
                    [] => {
                        return Err(format!(
                            "embedded std module '{name}' not found under std/src/{directory} (tried {direct}, {nested}; {} modules embedded)", EMBEDDED_STD.len()
                        ))
                    }
                    _ => {
                        return Err(format!(
                            "ambiguous embedded std module '{name}' under {directory}"
                        ))
                    }
                };
                let ns = qualified(namespace, name);
                self.module_paths.insert(ns.clone());
                let children = self.embedded_module(&next, &ns, visiting)?;
                result.push(pub_wrap(
                    Item::Namespace {
                        name: name.clone(),
                        items: children,
                    },
                    public,
                ));
            } else {
                result.push(item);
            }
        }
        visiting.remove(module_path);
        Ok(result)
    }
    pub fn embedded_std(&mut self, namespace: &str) -> Result<Vec<Item>, String> {
        self.embedded_module("lib.nova", namespace, &mut HashSet::new())
    }
}

#[derive(Clone)]
struct Symbol {
    public: bool,
    owner: String,
    module: bool,
}
#[derive(Default)]
struct Index {
    symbols: HashMap<String, Symbol>,
    files: Vec<SourceFile>,
    file_modules: HashSet<String>,
    dependency_roots: HashSet<String>,
    builtin_roots: HashSet<String>,
    dependency_aliases: HashMap<String, String>,
}
impl Index {
    fn register(&mut self, items: &[Item], ns: &str) -> Result<(), String> {
        for item in items {
            let (item, public) = bare(item);
            let (name, is_mod) = match item {
                Item::Namespace { name, .. } => (name.as_str(), true),
                Item::Function(f) => (f.name.as_str(), false),
                Item::Macro(m) => (m.name.as_str(), false),
                Item::Extern(f) => (f.name.as_str(), false),
                Item::Struct(s) => (s.name.as_str(), false),
                Item::Enum(e) => (e.name.as_str(), false),
                Item::TypeAlias(a) => (a.name.as_str(), false),
                Item::Trait(t) => (t.name.as_str(), false),
                Item::Concept(c) => (c.name.as_str(), false),
                _ => continue,
            };
            let q = qualified(ns, name);
            // Inline namespace is a public organizational scope for compatibility
            // with legacy packages; file-backed mod defaults to private.
            let public = public
                || (is_mod && !self.file_modules.contains(&q))
                || self.dependency_roots.contains(ns)
                || self
                    .builtin_roots
                    .iter()
                    .any(|root| ns == root || ns.starts_with(&format!("{root}::")));
            if self
                .symbols
                .insert(
                    q.clone(),
                    Symbol {
                        public,
                        owner: ns.into(),
                        module: is_mod,
                    },
                )
                .is_some()
            {
                return Err(self.error(
                    ns,
                    name,
                    "NOVA-M007",
                    &format!("duplicate module or symbol '{q}'"),
                ));
            }
            if let Item::Namespace {
                items: children, ..
            } = item
            {
                self.register(children, &q)?;
            }
        }
        Ok(())
    }
    /// Canonicalize a dependency alias at a namespace boundary, choosing the
    /// longest matching prefix. Rewriting a prefix (rather than entire symbol
    /// names) also works for methods and generic specializations, which are
    /// not all registered as standalone symbols in the module index.
    fn canonical_path(&self, name: &str) -> String {
        let mut value = name.to_owned();
        for _ in 0..=self.dependency_aliases.len() {
            let alias = self
                .dependency_aliases
                .keys()
                .filter(|alias| {
                    value.as_str() == alias.as_str() || value.starts_with(&format!("{alias}::"))
                })
                .max_by_key(|alias| alias.len());
            let Some(alias) = alias else { break };
            let canonical = &self.dependency_aliases[alias];
            let next = format!("{canonical}{}", &value[alias.len()..]);
            if next == value {
                break;
            }
            value = next;
        }
        value
    }

    fn path(&self, original: &str, scope: &str, aliases: &HashMap<String, String>) -> String {
        let mut bits = original.split("::");
        let first = bits.next().unwrap_or("");
        let rest = bits.collect::<Vec<_>>();
        if let Some(to) = aliases.get(first) {
            let path = if rest.is_empty() {
                to.clone()
            } else {
                format!("{}::{}", to, rest.join("::"))
            };
            return self.canonical_path(&path);
        }
        if first == "crate" {
            return self.canonical_path(&rest.join("::"));
        }
        if first == "self" {
            return self.canonical_path(&qualified(scope, &rest.join("::")));
        }
        if first == "super" {
            let parent = scope.rsplit_once("::").map(|(p, _)| p).unwrap_or("");
            return self.canonical_path(&qualified(parent, &rest.join("::")));
        }

        // A package's dependencies are visible from its child modules. Check
        // lexical ancestors before the top-level symbols, so `std::String`
        // inside a package picks that package's alias (which resolves to the
        // shared STD), and a sibling module may use `nova_view::...`.
        let mut ancestor = scope;
        while !ancestor.is_empty() {
            let candidate = qualified(ancestor, original);
            if self.symbols.contains_key(&candidate)
                || self.dependency_aliases.contains_key(&candidate)
                || self
                    .dependency_aliases
                    .keys()
                    .any(|alias| candidate.starts_with(&format!("{alias}::")))
            {
                return self.canonical_path(&candidate);
            }
            ancestor = ancestor.rsplit_once("::").map(|(p, _)| p).unwrap_or("");
        }
        self.canonical_path(original)
    }
    // Identify the first inaccessible component, not merely the final type.
    // Example: `mod lexer;` and `pub struct Scan` means the private *module*
    // is the blocker for a sibling module referring to `lexer::Scan`.
    fn access_blocker(&self, target: &str, caller: &str) -> Option<(String, bool)> {
        let mut path = String::new();
        for segment in target.split("::") {
            path = qualified(&path, segment);
            if let Some(symbol) = self.symbols.get(&path) {
                let owner_access = caller == symbol.owner
                    || (!symbol.owner.is_empty()
                        && caller.starts_with(&format!("{}::", symbol.owner)));
                let self_access =
                    symbol.module && (caller == path || caller.starts_with(&format!("{path}::")));
                if !symbol.public && !owner_access && !self_access {
                    return Some((path, symbol.module));
                }
            }
        }
        None
    }
    fn visible(&self, target: &str, caller: &str) -> bool {
        self.access_blocker(target, caller).is_none()
    }
    fn privacy_message(&self, target: &str, caller: &str) -> String {
        match self.access_blocker(target, caller) {
            Some((path, true)) => {
                let segment = path.rsplit("::").next().unwrap_or(&path);
                format!("cannot access '{target}': enclosing module '{path}' is private; declare it as `pub mod {segment};`")
            }
            Some((path, false)) => {
                format!("'{path}' is private; add `pub` to its declaration to expose it")
            }
            None => format!("'{target}' is inaccessible"),
        }
    }
    fn source(&self, scope: &str) -> Option<&SourceFile> {
        self.files
            .iter()
            .rev()
            .find(|s| s.namespace == scope)
            .or_else(|| {
                self.files.iter().rev().find(|s| {
                    scope.starts_with(&format!("{}::", s.namespace)) && !s.namespace.is_empty()
                })
            })
            .or_else(|| self.files.iter().find(|s| s.namespace.is_empty()))
    }
    fn error_at(&self, scope: &str, at: usize, len: usize, code: &str, message: &str) -> String {
        if let Some(file) = self.source(scope) {
            diagnostics::render(code, &file.path, &file.text, at, len, message)
        } else {
            format!("error[{code}]: {message}")
        }
    }
    fn error(&self, scope: &str, token: &str, code: &str, message: &str) -> String {
        if let Some(file) = self.source(scope) {
            let pos = find_last_token(&strip_comments(&file.text), token).unwrap_or(0);
            diagnostics::render(
                code,
                &file.path,
                &file.text,
                pos,
                token.len().max(1),
                message,
            )
        } else {
            format!("error[{code}]: {message}")
        }
    }
    fn ensure_visible(&self, target: &str, scope: &str, spelling: &str) -> Result<(), String> {
        // Only the shared built-in STD definition may access its sealed ABI,
        // even when application code spells the path using a transitive alias
        // (e.g. `vendor::std::internal::map`).
        for root in &self.builtin_roots {
            let prefix = format!("{root}::internal");
            if target == prefix || target.starts_with(&format!("{prefix}::")) {
                if scope != root && !scope.starts_with(&format!("{root}::")) {
                    return Err(self.error(scope,spelling,"NOVA-M015",
                        "the standard-library runtime layer is private; use std::map, std::vec, std::string or another safe wrapper"));
                }
            }
        }
        if self.symbols.contains_key(target) && !self.visible(target, scope) {
            return Err(self.error(
                scope,
                spelling,
                "NOVA-M008",
                &self.privacy_message(target, scope),
            ));
        }
        Ok(())
    }
    fn process_items(&self, items: Vec<Item>, scope: &str) -> Result<Vec<Item>, String> {
        let mut imports = HashMap::new();
        for item in &items {
            let (actual, public) = bare(item);
            if let Item::Use { path: name, at } = actual {
                if public {
                    return Err(self.error_at(
                        scope,
                        *at,
                        name.len(),
                        "NOVA-M009",
                        "pub use re-exports are not supported yet; use pub fn or pub mod instead",
                    ));
                }
                let alias = name.rsplit("::").next().unwrap_or(name);
                if ["crate", "self", "super"].contains(&alias) {
                    return Err(self.error_at(
                        scope,
                        *at,
                        name.len(),
                        "NOVA-M010",
                        "use must end in a symbol name",
                    ));
                }
                let dest = self.path(name, scope, &HashMap::new());
                if !self.symbols.contains_key(&dest) {
                    return Err(self.error_at(
                        scope,
                        *at,
                        name.len(),
                        "NOVA-M011",
                        &format!("cannot resolve use '{name}' (resolved as '{dest}')"),
                    ));
                }
                if !self.visible(&dest, scope) {
                    return Err(self.error_at(
                        scope,
                        *at,
                        name.len(),
                        "NOVA-M008",
                        &self.privacy_message(&dest, scope),
                    ));
                }
                if self.symbols.contains_key(&qualified(scope, alias)) {
                    return Err(self.error_at(
                        scope,
                        *at,
                        name.len(),
                        "NOVA-M013",
                        &format!("import '{alias}' conflicts with a symbol defined in this module"),
                    ));
                }
                if imports.insert(alias.to_owned(), dest).is_some() {
                    return Err(self.error_at(
                        scope,
                        *at,
                        name.len(),
                        "NOVA-M012",
                        &format!("duplicate use alias '{alias}'"),
                    ));
                }
            }
        }
        let mut out = vec![];
        for item in items {
            let (inner, public) = bare(&item);
            if matches!(inner, Item::Use { .. }) {
                continue;
            }
            let rewritten = match inner {
                Item::Namespace { name, items } => Item::Namespace {
                    name: name.clone(),
                    items: self.process_items(items.clone(), &qualified(scope, name))?,
                },
                Item::Function(f) => {
                    let mut f = f.clone();
                    for p in &mut f.params {
                        self.ty(&mut p.ty, scope, &imports)?;
                    }
                    self.ty(&mut f.result, scope, &imports)?;
                    self.stmts(&mut f.body, scope, &imports)?;
                    self.concept_bounds(&mut f.concept_bounds, scope, &imports)?;
                    Item::Function(f)
                }
                Item::Macro(m) => {
                    let mut m = m.clone();
                    // Resolve every arm in its definition's namespace. The
                    // macro expander will select one arm after AST matching.
                    for arm in &mut m.arms {
                        for param in &mut arm.params {
                            if let Some(concept) = &mut param.concept {
                                let mut bound = ast::ConceptBound {
                                    name: concept.name.clone(),
                                    args: concept.additional_args.clone(),
                                };
                                // Resolve concept names and explicit type arguments in
                                // the macro definition scope, not at the call site.
                                self.concept_bounds(
                                    std::slice::from_mut(&mut bound),
                                    scope,
                                    &imports,
                                )?;
                                concept.name = bound.name;
                                concept.additional_args = bound.args;
                            }
                        }
                        match &mut arm.body {
                            ast::MacroBody::Expression(template) => {
                                self.expr(template, scope, &imports)?
                            }
                            ast::MacroBody::Statements(statements) => {
                                self.stmts(statements, scope, &imports)?
                            }
                        }
                    }
                    Item::Macro(m)
                }
                Item::Extern(e) => {
                    let mut e = e.clone();
                    for p in &mut e.params {
                        self.ty(&mut p.ty, scope, &imports)?;
                    }
                    self.ty(&mut e.result, scope, &imports)?;
                    Item::Extern(e)
                }
                Item::Struct(s) => {
                    let mut s = s.clone();
                    for field in &mut s.fields {
                        self.ty(&mut field.ty, scope, &imports)?;
                    }
                    if let Some(parent) = &mut s.parent {
                        self.ty(parent, scope, &imports)?;
                    }
                    Item::Struct(s)
                }
                Item::Enum(e) => {
                    let mut e = e.clone();
                    for v in &mut e.variants {
                        for t in &mut v.payload {
                            self.ty(t, scope, &imports)?;
                        }
                    }
                    Item::Enum(e)
                }
                Item::TypeAlias(alias) => {
                    let mut alias = alias.clone();
                    self.ty(&mut alias.target, scope, &imports)?;
                    Item::TypeAlias(alias)
                }
                Item::Trait(definition) => {
                    let mut definition = definition.clone();
                    if let Some(restriction) = &mut definition.restriction {
                        for ty in &mut restriction.permitted_types {
                            self.ty(ty, scope, &imports)?;
                        }
                    }
                    for parent in &mut definition.parents {
                        *parent = self.path(parent, scope, &imports);
                        self.ensure_visible(parent, scope, parent)?;
                    }
                    for method in &mut definition.methods {
                        for parameter in &mut method.params {
                            self.ty(&mut parameter.ty, scope, &imports)?;
                        }
                        self.ty(&mut method.result, scope, &imports)?;
                    }
                    Item::Trait(definition)
                }
                Item::Concept(c) => {
                    let mut c = c.clone();
                    self.concept_predicate(&mut c.predicate, scope, &imports, &c.generics)?;
                    Item::Concept(c)
                }
                Item::ConceptAssert(expr) => {
                    let mut expr = expr.clone();
                    self.concept_predicate(&mut expr, scope, &imports, &[])?;
                    Item::ConceptAssert(expr)
                }
                Item::Impl(i) => {
                    let mut i = i.clone();
                    self.ty(&mut i.target, scope, &imports)?;
                    for (_, ty) in &mut i.associated_types {
                        self.ty(ty, scope, &imports)?;
                    }
                    if let Some(trait_name) = &mut i.trait_name {
                        let original = trait_name.clone();
                        *trait_name = self.path(&original, scope, &imports);
                        self.ensure_visible(trait_name, scope, &original)?;
                    }
                    for method in &mut i.methods {
                        for p in &mut method.params {
                            self.ty(&mut p.ty, scope, &imports)?;
                        }
                        self.ty(&mut method.result, scope, &imports)?;
                        self.stmts(&mut method.body, scope, &imports)?;
                        self.concept_bounds(&mut method.concept_bounds, scope, &imports)?;
                    }
                    Item::Impl(i)
                }
                Item::Import(path) => Item::Import(self.path(path, scope, &imports)),
                other => other.clone(),
            };
            out.push(pub_wrap(rewritten, public));
        }
        Ok(out)
    }
    fn concept_bounds(
        &self,
        bounds: &mut [ast::ConceptBound],
        scope: &str,
        imports: &HashMap<String, String>,
    ) -> Result<(), String> {
        for bound in bounds {
            // Intrinsics are predeclared by the compiler, not module symbols.
            if ![
                "integer",
                "floating",
                "numeric",
                "signed",
                "unsigned",
                "boolean",
                "pointer",
                "reference",
                "same_as",
                "implements",
                "has_field",
                "has_method",
            ]
            .contains(&bound.name.as_str())
            {
                let original = bound.name.clone();
                bound.name = self.path(&original, scope, imports);
                self.ensure_visible(&bound.name, scope, &original)?;
            }
            for ty in &mut bound.args {
                self.ty(ty, scope, imports)?;
            }
        }
        Ok(())
    }
    fn concept_predicate(
        &self,
        expr: &mut ast::ConceptExpr,
        scope: &str,
        imports: &HashMap<String, String>,
        type_parameters: &[String],
    ) -> Result<(), String> {
        match expr {
            ast::ConceptExpr::Apply { name, args } => {
                if ![
                    "integer",
                    "floating",
                    "numeric",
                    "signed",
                    "unsigned",
                    "boolean",
                    "pointer",
                    "reference",
                    "same_as",
                    "implements",
                    "has_field",
                    "has_method",
                ]
                .contains(&name.as_str())
                {
                    let original = name.clone();
                    *name = self.path(&original, scope, imports);
                    self.ensure_visible(name, scope, &original)?;
                }
                for arg in args {
                    self.ty_in_scope(arg, scope, imports, type_parameters)?;
                }
            }
            ast::ConceptExpr::Not(x) => self.concept_predicate(x, scope, imports, type_parameters)?,
            ast::ConceptExpr::And(a, b) | ast::ConceptExpr::Or(a, b) => {
                self.concept_predicate(a, scope, imports, type_parameters)?;
                self.concept_predicate(b, scope, imports, type_parameters)?;
            }
            ast::ConceptExpr::Requires { variables, rules } => {
                for (_, ty) in variables {
                    self.ty_in_scope(ty, scope, imports, type_parameters)?;
                }
                for rule in rules {
                    match rule {
                        ast::Requirement::Field { result, .. }
                        | ast::Requirement::Method { result, .. }
                        | ast::Requirement::Binary { result, .. } => {
                            self.ty_in_scope(result, scope, imports, type_parameters)?
                        }
                    }
                }
            }
            ast::ConceptExpr::Literal(_) => {}
        }
        Ok(())
    }
    fn ty(
        &self,
        ty: &mut TypeRef,
        scope: &str,
        imports: &HashMap<String, String>,
    ) -> Result<(), String> {
        self.ty_in_scope(ty, scope, imports, &[])
    }
    /// Resolve a type in the concept's lexical scope: generic parameters
    /// shadow module symbols, even if another package declares a private
    /// type with the same name. All other types still undergo privacy checks.
    fn ty_in_scope(
        &self,
        ty: &mut TypeRef,
        scope: &str,
        imports: &HashMap<String, String>,
        type_parameters: &[String],
    ) -> Result<(), String> {
        match ty {
            TypeRef::Named(name) => {
                // Builtin scalar names are types, not relative module paths.
                // In particular, inside `std::char` a `char` annotation and
                // a character literal must not become `std::char` (the module).
                // Generic parameters keep their own lexical priority.
                if type_parameters.contains(name)
                    || crate::types::is_primitive_type_name(name)
                {
                    return Ok(());
                }
                let raw = name.clone();
                let resolved = self.path(name, scope, imports);
                self.ensure_visible(&resolved, scope, &raw)?;
                if resolved != raw {
                    *name = resolved;
                }
            }
            TypeRef::Applied(name, args) => {
                let raw = name.clone();
                let resolved = self.path(name, scope, imports);
                self.ensure_visible(&resolved, scope, &raw)?;
                *name = resolved;
                for arg in args {
                    self.ty_in_scope(arg, scope, imports, type_parameters)?;
                }
            }
            TypeRef::Array(inner, _)
            | TypeRef::Pointer { inner, .. }
            | TypeRef::Reference { inner, .. } => self.ty_in_scope(inner, scope, imports, type_parameters)?,
            TypeRef::Function { params, result } => {
                for p in params {
                    self.ty_in_scope(p, scope, imports, type_parameters)?;
                }
                self.ty_in_scope(result, scope, imports, type_parameters)?;
            }
        }
        Ok(())
    }
    fn expr(
        &self,
        e: &mut Expr,
        scope: &str,
        imports: &HashMap<String, String>,
    ) -> Result<(), String> {
        match e {
            Expr::Var(name) if name.contains("::") => {
                let raw = name.clone();
                let target = self.path(name, scope, imports);
                self.ensure_visible(&target, scope, &raw)?;
                *name = target;
            }
            Expr::Call(name, _)
            | Expr::MacroCall(name, _)
            | Expr::GenericCall(name, _, _)
            | Expr::StructInit(name, _)
            | Expr::GenericStructInit(name, _, _) => {
                let raw = name.clone();
                let target = self.path(name, scope, imports);
                self.ensure_visible(&target, scope, &raw)?;
                *name = target;
            }
            _ => {}
        }
        match e {
            Expr::If { condition, then_body, else_body } => {
                self.expr(condition, scope, imports)?;
                self.stmts(then_body, scope, imports)?;
                self.stmts(else_body, scope, imports)?;
            }
            Expr::Block(body) => self.stmts(body, scope, imports)?,
            Expr::Call(_, args) | Expr::MacroCall(_, args) => {
                for a in args {
                    self.expr(a, scope, imports)?;
                }
            }
            Expr::FunctionRef(name) => {
                let raw = name.clone();
                let target = self.path(name, scope, imports);
                self.ensure_visible(&target, scope, &raw)?;
                *name = target;
            }
            Expr::CallIndirect(callee, args) => {
                self.expr(callee, scope, imports)?;
                for arg in args {
                    self.expr(arg, scope, imports)?;
                }
            }
            Expr::Lambda {
                params,
                result,
                body,
            } => {
                for p in params {
                    self.ty(&mut p.ty, scope, imports)?;
                }
                self.ty(result, scope, imports)?;
                self.stmts(body, scope, imports)?;
            }
            Expr::GenericCall(_, tys, args) => {
                for t in tys {
                    self.ty(t, scope, imports)?;
                }
                for a in args {
                    self.expr(a, scope, imports)?;
                }
            }
            Expr::StructInit(_, fields) | Expr::GenericStructInit(_, _, fields) => {
                for (_, value) in fields {
                    self.expr(value, scope, imports)?;
                }
            }
            Expr::Unary(_, value)
            | Expr::Spread(value)
            | Expr::Deref(value)
            | Expr::Field(value, _) => self.expr(value, scope, imports)?,
            Expr::Binary(a, _, b) | Expr::Index(a, b) => {
                self.expr(a, scope, imports)?;
                self.expr(b, scope, imports)?;
            }
            Expr::Array(values) => {
                for value in values {
                    self.expr(value, scope, imports)?;
                }
            }
            Expr::AddressOf { value, .. } => self.expr(value, scope, imports)?,
            Expr::MethodCall(value, _, args) => {
                self.expr(value, scope, imports)?;
                for a in args {
                    self.expr(a, scope, imports)?;
                }
            }
            Expr::Yield(inner) | Expr::Await(inner) => self.expr(inner, scope, imports)?,
            Expr::Zero(ty) => self.ty(ty, scope, imports)?,
            Expr::Cast(value, ty) => {
                self.expr(value, scope, imports)?;
                self.ty(ty, scope, imports)?;
            }
            _ => {}
        }
        Ok(())
    }
    fn stmts(
        &self,
        stmts: &mut [Stmt],
        scope: &str,
        imports: &HashMap<String, String>,
    ) -> Result<(), String> {
        for stmt in stmts {
            match stmt {
                Stmt::Let {
                    ty, init, defer, ..
                } => {
                    if let Some(t) = ty {
                        self.ty(t, scope, imports)?;
                    }
                    self.expr(init, scope, imports)?;
                    if let Some(cleanup) = defer {
                        self.expr(cleanup, scope, imports)?;
                    }
                }
                Stmt::Set { value, .. }
                | Stmt::FieldSet { value, .. }
                | Stmt::DerefSet { value, .. }
                | Stmt::CompoundSet { value, .. }
                | Stmt::Expr(value) | Stmt::Tail(value)
                | Stmt::Defer(value) => self.expr(value, scope, imports)?,
                Stmt::Return(Some(value)) => self.expr(value, scope, imports)?,
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    self.expr(condition, scope, imports)?;
                    self.stmts(then_body, scope, imports)?;
                    if let Some(other) = else_body {
                        self.stmts(other, scope, imports)?;
                    }
                }
                Stmt::While { condition, body } => {
                    self.expr(condition, scope, imports)?;
                    self.stmts(body, scope, imports)?;
                }
                Stmt::For {
                    start, end, body, ..
                } => {
                    self.expr(start, scope, imports)?;
                    self.expr(end, scope, imports)?;
                    self.stmts(body, scope, imports)?;
                }
                Stmt::ForEach { iterable, body, .. } => {
                    self.expr(iterable, scope, imports)?;
                    self.stmts(body, scope, imports)?;
                }
                Stmt::Loop(body) => self.stmts(body, scope, imports)?,
                Stmt::Break | Stmt::Continue => {}
                Stmt::Unsafe(body) => self.stmts(body, scope, imports)?,
                Stmt::Match { value, arms } => {
                    self.expr(value, scope, imports)?;
                    for arm in arms {
                        if let Pattern::Variant { path, .. } = &mut arm.pattern {
                            let raw = path.clone();
                            let target = self.path(path, scope, imports);
                            self.ensure_visible(&target, scope, &raw)?;
                            *path = target;
                        }
                        self.stmts(&mut arm.body, scope, imports)?;
                    }
                }
                Stmt::Return(None) => {}
            }
        }
        Ok(())
    }
}
fn find_last_token(source: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    source
        .match_indices(needle)
        .filter(|(at, _)| {
            let before = source[..*at].chars().last();
            let after = source[at + needle.len()..].chars().next();
            !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
        .map(|(at, _)| at)
        .last()
}
pub fn link(program: ast::Program, loader: &Loader) -> Result<ast::Program, String> {
    let mut index = Index {
        files: loader.sources.clone(),
        file_modules: loader.module_paths.clone(),
        dependency_roots: loader.dependency_roots.clone(),
        builtin_roots: loader.builtin_roots.clone(),
        dependency_aliases: loader.dependency_aliases.clone(),
        ..Index::default()
    };
    index.register(&program.items, "")?;
    Ok(ast::Program {
        items: index.process_items(program.items, "")?,
    })
}

#[cfg(test)]
mod builtin_instance_resolution_tests {
    use super::*;

    #[test]
    fn transitive_aliases_share_std_types_and_method_paths() {
        let mut index = Index::default();
        index
            .dependency_aliases
            .insert("frame::std".into(), "std".into());
        index
            .dependency_aliases
            .insert("frame::web::view::std".into(), "std".into());
        index
            .dependency_aliases
            .insert("frame::other_view".into(), "frame::web::view".into());
        index.builtin_roots.insert("std".into());
        index.symbols.insert(
            "std::string::String".into(),
            Symbol {
                public: true,
                owner: "std::string".into(),
                module: false,
            },
        );
        index.symbols.insert(
            "std::internal::map::native_create".into(),
            Symbol {
                public: true,
                owner: "std::internal::map".into(),
                module: false,
            },
        );
        let imports = HashMap::new();
        assert_eq!(
            index.path("std::string::String", "frame::ui", &imports),
            "std::string::String"
        );
        assert_eq!(
            index.path("std::string::String", "frame::web::view::html", &imports),
            "std::string::String"
        );
        assert_eq!(
            index.path("std::string::String::from_text", "frame::ui", &imports),
            "std::string::String::from_text"
        );
        assert_eq!(
            index.path("frame::other_view::html::Html", "", &imports),
            "frame::web::view::html::Html"
        );
        assert!(index
            .ensure_visible(
                &index.path("frame::std::internal::map::native_create", "", &imports),
                "frame::ui",
                "frame::std::internal::map::native_create"
            )
            .is_err());
    }

    #[test]
    fn builtin_char_remains_primitive_even_with_same_named_module() {
        let mut index = Index::default();
        index.symbols.insert(
            "std::char".into(),
            Symbol { public: true, owner: "std".into(), module: true },
        );
        index.symbols.insert(
            "std::char::is_ascii".into(),
            Symbol { public: true, owner: "std::char".into(), module: false },
        );
        let imports = HashMap::new();
        // Module paths are still valid in expression positions.
        assert_eq!(index.path("char::is_ascii", "std", &imports), "std::char::is_ascii");
        // But the primitive type must never be rewritten to a module path.
        let mut ty = TypeRef::Named("char".into());
        index.ty(&mut ty, "std::char", &imports).unwrap();
        assert!(matches!(ty, TypeRef::Named(ref n) if n == "char"));
        let mut arg_ty = TypeRef::Pointer {
            mutable: false,
            inner: Box::new(TypeRef::Named("char".into())),
        };
        index.ty(&mut arg_ty, "std::char", &imports).unwrap();
        assert!(matches!(arg_ty, TypeRef::Pointer { inner, .. }
            if matches!(*inner, TypeRef::Named(ref n) if n == "char")));
        // The same syntax also appears as a target in character literals.
        let mut literal = Expr::Cast(
            Box::new(Expr::Str("'\\u{007F}'".into())),
            TypeRef::Named("char".into()),
        );
        index.expr(&mut literal, "std::char", &imports).unwrap();
        assert!(matches!(literal, Expr::Cast(_, TypeRef::Named(ref n)) if n == "char"));
        // Explicit paths keep the independently named module accessible.
        let mut qualified_type = TypeRef::Named("std::char".into());
        index.ty(&mut qualified_type, "std::char", &imports).unwrap();
        assert!(matches!(qualified_type, TypeRef::Named(ref n) if n == "std::char"));
    }

    #[test]
    fn builtin_type_names_stay_stable_in_module_scopes() {
        let index = Index::default();
        for name in ["()", "bool", "char", "str", "i8", "u16", "i64", "u64", "f32", "f64", "isize", "usize"] {
            let mut ty = TypeRef::Named(name.into());
            index.ty(&mut ty, "std::char", &HashMap::new()).unwrap();
            assert!(matches!(&ty, TypeRef::Named(n) if n == name), "{name} became {ty:?}");
        }
        assert!(!crate::types::is_primitive_type_name("std::char"));
        assert!(!crate::types::is_primitive_type_name("String"));
    }

    #[test]
    fn nested_std_resolves_its_own_internal_runtime() {
        let mut index = Index::default();
        index.builtin_roots.insert("std".into());
        index.builtin_roots.insert("nexo::std".into());
        for name in [
            "std::internal::map::native_create",
            "nexo::std::internal::map::native_create",
        ] {
            index.symbols.insert(
                name.into(),
                Symbol {
                    public: true,
                    owner: String::new(),
                    module: false,
                },
            );
        }
        let resolved = index.path(
            "std::internal::map::native_create",
            "nexo::std::map",
            &HashMap::new(),
        );
        assert_eq!(resolved, "nexo::std::internal::map::native_create");
        assert!(index
            .ensure_visible(
                &resolved,
                "nexo::std::map",
                "std::internal::map::native_create"
            )
            .is_ok());
        assert!(index
            .ensure_visible(&resolved, "nexo::app", "std::internal::map::native_create")
            .is_err());
    }
}
