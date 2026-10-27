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
pub(crate) const EMBEDDED_STD: &[(&str, &str)] = &[
    (
        "internal/mod.nova",
        include_str!("../std/src/internal/mod.nova"),
    ),
    (
        "internal/map.nova",
        include_str!("../std/src/internal/map.nova"),
    ),
    ("arena.nova", include_str!("../std/src/arena.nova")),
    ("atomic.nova", include_str!("../std/src/atomic.nova")),
    ("sync.nova", include_str!("../std/src/sync.nova")),
    ("thread.nova", include_str!("../std/src/thread.nova")),
    ("ops.nova", include_str!("../std/src/ops.nova")),
    ("concepts.nova", include_str!("../std/src/concepts.nova")),
    ("builder.nova", include_str!("../std/src/builder.nova")),
    ("convert.nova", include_str!("../std/src/convert.nova")),
    ("coro.nova", include_str!("../std/src/coro.nova")),
    ("env.nova", include_str!("../std/src/env.nova")),
    ("fs.nova", include_str!("../std/src/fs.nova")),
    ("generator.nova", include_str!("../std/src/generator.nova")),
    ("net.nova", include_str!("../std/src/net.nova")),
    ("io.nova", include_str!("../std/src/io.nova")),
    ("json.nova", include_str!("../std/src/json.nova")),
    ("lib.nova", include_str!("../std/src/lib.nova")),
    (
        "libc/ctype.nova",
        include_str!("../std/src/libc/ctype.nova"),
    ),
    (
        "libc/errno.nova",
        include_str!("../std/src/libc/errno.nova"),
    ),
    (
        "libc/fcntl.nova",
        include_str!("../std/src/libc/fcntl.nova"),
    ),
    ("libc/math.nova", include_str!("../std/src/libc/math.nova")),
    ("libc/mod.nova", include_str!("../std/src/libc/mod.nova")),
    (
        "libc/stdio.nova",
        include_str!("../std/src/libc/stdio.nova"),
    ),
    (
        "libc/stdlib.nova",
        include_str!("../std/src/libc/stdlib.nova"),
    ),
    (
        "libc/string.nova",
        include_str!("../std/src/libc/string.nova"),
    ),
    ("libc/time.nova", include_str!("../std/src/libc/time.nova")),
    (
        "libc/unistd.nova",
        include_str!("../std/src/libc/unistd.nova"),
    ),
    ("math.nova", include_str!("../std/src/math.nova")),
    ("memory.nova", include_str!("../std/src/memory.nova")),
    ("raw.nova", include_str!("../std/src/raw.nova")),
    ("string.nova", include_str!("../std/src/string.nova")),
    ("task.nova", include_str!("../std/src/task.nova")),
    ("ownership.nova", include_str!("../std/src/ownership.nova")),
    ("time.nova", include_str!("../std/src/time.nova")),
    ("vec.nova", include_str!("../std/src/vec.nova")),
    ("set.nova", include_str!("../std/src/set.nova")),
    ("map.nova", include_str!("../std/src/map.nova")),
    ("symbol.nova", include_str!("../std/src/symbol.nova")),
    ("option.nova", include_str!("../std/src/option.nova")),
    ("result.nova", include_str!("../std/src/result.nova")),
    ("error.nova", include_str!("../std/src/error.nova")),
    ("panic.nova", include_str!("../std/src/panic.nova")),
];

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
                            "embedded std module '{name}' not found under {directory}"
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
    fn path(&self, original: &str, scope: &str, aliases: &HashMap<String, String>) -> String {
        let mut bits = original.split("::");
        let first = bits.next().unwrap_or("");
        let rest = bits.collect::<Vec<_>>();
        if let Some(to) = aliases.get(first) {
            return if rest.is_empty() {
                to.clone()
            } else {
                format!("{}::{}", to, rest.join("::"))
            };
        }
        if first == "crate" {
            return rest.join("::");
        }
        if first == "self" {
            return qualified(scope, &rest.join("::"));
        }
        if first == "super" {
            let parent = scope.rsplit_once("::").map(|(p, _)| p).unwrap_or("");
            return qualified(parent, &rest.join("::"));
        }
        // Inside an embedded STD instance, `std::...` names that *same*
        // instance. A transitive dependency may also import STD (e.g.
        // `nexo::std`), and falling back to the application's top-level
        // `std` would both pick the wrong ABI and trigger a spurious M015.
        // Application scopes never get this special treatment.
        if let Some(rest) = original.strip_prefix("std::") {
            if let Some(root) = self
                .builtin_roots
                .iter()
                .filter(|root| scope == root.as_str() || scope.starts_with(&format!("{root}::")))
                .max_by_key(|root| root.len())
            {
                let local = qualified(root, rest);
                if self.symbols.contains_key(&local)
                    || self
                        .symbols
                        .keys()
                        .any(|s| s.starts_with(&format!("{local}::")))
                {
                    return local;
                }
            }
        }
        // Locally declared symbols shadow names from the package root.
        let lexical = qualified(scope, original);
        if !scope.is_empty() && self.symbols.contains_key(&lexical) {
            return lexical;
        }
        if self.symbols.contains_key(original) {
            return original.to_owned();
        }
        // A qualified path may refer to a member declared in the child module.
        if original.contains("::")
            && self
                .symbols
                .keys()
                .any(|s| s.starts_with(&format!("{lexical}::")))
        {
            return lexical;
        }
        original.to_owned()
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
        // Only the particular built-in STD instance that owns the symbol may
        // use its sealed ABI, even when imported under a renamed alias or via
        // a transitive dependency (e.g. `vendor::std::internal::map`).
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
                    self.concept_predicate(&mut c.predicate, scope, &imports)?;
                    Item::Concept(c)
                }
                Item::ConceptAssert(expr) => {
                    let mut expr = expr.clone();
                    self.concept_predicate(&mut expr, scope, &imports)?;
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
                    self.ty(arg, scope, imports)?;
                }
            }
            ast::ConceptExpr::Not(x) => self.concept_predicate(x, scope, imports)?,
            ast::ConceptExpr::And(a, b) | ast::ConceptExpr::Or(a, b) => {
                self.concept_predicate(a, scope, imports)?;
                self.concept_predicate(b, scope, imports)?;
            }
            ast::ConceptExpr::Requires { variables, rules } => {
                for (_, ty) in variables {
                    self.ty(ty, scope, imports)?;
                }
                for rule in rules {
                    match rule {
                        ast::Requirement::Field { result, .. }
                        | ast::Requirement::Method { result, .. }
                        | ast::Requirement::Binary { result, .. } => {
                            self.ty(result, scope, imports)?
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
        match ty {
            TypeRef::Named(name) => {
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
                    self.ty(arg, scope, imports)?;
                }
            }
            TypeRef::Array(inner, _)
            | TypeRef::Pointer { inner, .. }
            | TypeRef::Reference { inner, .. } => self.ty(inner, scope, imports)?,
            TypeRef::Function { params, result } => {
                for p in params {
                    self.ty(p, scope, imports)?;
                }
                self.ty(result, scope, imports)?;
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
                | Stmt::Expr(value)
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
