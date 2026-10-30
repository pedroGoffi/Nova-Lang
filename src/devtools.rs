//! Conservative source formatting and local-only developer workflows.
//! This is intentionally NOT a stable formatter, linter or package registry.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

fn collect(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if root.is_file() {
        if root.extension().and_then(|ext| ext.to_str()) == Some("nova") {
            out.push(root.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(root).map_err(|e| format!("{}: {e}", root.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if entry.file_type().map_err(|e| e.to_string())?.is_symlink() {
            continue;
        }
        if path.is_dir() {
            if !matches!(
                path.file_name().and_then(|s| s.to_str()),
                Some("target" | ".git" | "node_modules")
            ) {
                collect(&path, out)?;
            }
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("nova") {
            out.push(path);
        }
    }
    Ok(())
}
pub fn sources(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = vec![];
    if !root.exists() {
        return Err(format!("path {} does not exist", root.display()));
    }
    collect(root, &mut files)?;
    files.sort();
    Ok(files)
}

/// Layout-only formatter. Does not rewrite tokens, comments, or spaces within
/// strings. Multiline strings are tracked, so their whitespace is preserved.
pub fn format_source(source: &str) -> String {
    let mut result = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for line in source.split_inclusive('\n') {
        let (content, ending) = line.strip_suffix('\n').map_or((line, ""), |s| (s, "\n"));
        let in_string_at_start = quoted;
        let mut chars = content.chars().peekable();
        while let Some(c) = chars.next() {
            if quoted {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    quoted = false;
                }
            } else if c == '"' {
                quoted = true;
            } else if c == '/' && chars.peek() == Some(&'/') {
                break;
            }
        }
        // Trimming is safe only outside a quoted multi-line literal.
        let cooked = if in_string_at_start || quoted {
            content.to_owned()
        } else {
            let line = content.trim_end_matches(|c: char| matches!(c, ' ' | '\t' | '\r'));
            let prefix_len = line.len() - line.trim_start_matches('\t').len();
            // Use spaces only for leading indentation. Leave interior tabs untouched.
            if prefix_len > 0 {
                format!("{}{}", "    ".repeat(prefix_len), &line[prefix_len..])
            } else {
                line.to_owned()
            }
        };
        result.push_str(&cooked);
        result.push_str(ending);
    }
    if !result.is_empty() && !result.ends_with('\n') && !quoted {
        result.push('\n');
    }
    result
}

pub fn fmt(root: &Path, check: bool) -> Result<(), String> {
    let files = sources(root)?;
    if files.is_empty() {
        return Err("no .nova source files found".into());
    }
    let mut changes = vec![];
    for file in &files {
        let original = fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let formatted = format_source(&original);
        if formatted != original {
            changes.push(file.clone());
            if !check {
                fs::write(file, formatted).map_err(|e| format!("{}: {e}", file.display()))?;
            }
        }
    }
    if check && !changes.is_empty() {
        return Err(format!(
            "{} files need layout formatting:\n{}",
            changes.len(),
            changes
                .iter()
                .map(|p| format!("  {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    println!(
        "Nova fmt: {} source files, {} {}",
        files.len(),
        changes.len(),
        if check { "need changes" } else { "formatted" }
    );
    Ok(())
}

pub fn clean(root: &Path) -> Result<(), String> {
    if !root.join("Nova.toml").is_file() {
        return Err("nova clean requires a Nova.toml package directory".into());
    }
    let target = root.join("target");
    if target.is_symlink() {
        return Err("refusing to remove symlinked target directory".into());
    }
    if target.is_dir() {
        fs::remove_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    println!("Cleaned {}", target.display());
    Ok(())
}

pub fn local_tests<F>(root: &Path, mut compile: F) -> Result<(), String>
where
    F: FnMut(&Path, &Path) -> Result<(), String>,
{
    let directory = root.join("tests");
    if !directory.is_dir() {
        return Err(format!("no tests/ directory in {}", root.display()));
    }
    let mut cases = vec![];
    for entry in fs::read_dir(&directory).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("nova") {
            cases.push(path);
        } else if path.is_dir() && path.join("Nova.toml").is_file() {
            cases.push(path);
        }
    }
    cases.sort();
    if cases.is_empty() {
        return Err("no standalone tests/*.nova or tests/*/Nova.toml found".into());
    }
    let output_dir = root.join("target").join("nova-tests");
    fs::create_dir_all(&output_dir).map_err(|e| e.to_string())?;
    for (i, path) in cases.iter().enumerate() {
        let artifact = output_dir.join(format!("test-{i}"));
        compile(path, &artifact)?;
        let status = Command::new(&artifact)
            .status()
            .map_err(|e| format!("{}: {e}", artifact.display()))?;
        if !status.success() {
            return Err(format!("test {} failed with {status}", path.display()));
        }
        println!("PASS {}", path.display());
    }
    println!("Nova test: {}/{} passed", cases.len(), cases.len());
    Ok(())
}

pub fn bench(binary: &Path, runs: usize) -> Result<(), String> {
    if runs == 0 || runs > 1000 {
        return Err("benchmark runs must be within 1..=1000".into());
    }
    let mut total = 0u128;
    let mut min = u128::MAX;
    let mut max = 0u128;
    for _ in 0..runs {
        let start = Instant::now();
        let status = Command::new(binary)
            .status()
            .map_err(|e| format!("{}: {e}", binary.display()))?;
        if !status.success() {
            return Err(format!("benchmark program failed with {status}"));
        }
        let elapsed = start.elapsed().as_micros();
        min = min.min(elapsed);
        max = max.max(elapsed);
        total += elapsed;
    }
    println!(
        "Nova bench: {runs} runs, min {min} µs, avg {} µs, max {max} µs (process startup included)",
        total / (runs as u128)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_string_content() {
        let s = r#"fn main() -> i32 {  
    let text = "spaces   ";  
    return 0; 
}"#;
        let f = format_source(s);
        assert!(f.contains("\"spaces   \""));
        assert!(f.ends_with("}\n"));
        assert_eq!(format_source(&f), f);
    }
    #[test]
    fn multiline_strings_are_not_rewritten() {
        let s = "let s = \"first   \nsecond  \";  \n";
        assert!(format_source(s).contains("first   \nsecond  \""));
    }
}

fn type_string(ty: &crate::ast::TypeRef) -> String {
    use crate::ast::TypeRef;
    match ty {
        TypeRef::Named(s) => s.clone(),
        TypeRef::Applied(name, params) => format!(
            "{}<{}>",
            name,
            params
                .iter()
                .map(type_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeRef::Array(elem, len) => format!("[{}; {}]", type_string(elem), len),
        TypeRef::Function { params, result } => format!(
            "fn({})->{}",
            params
                .iter()
                .map(type_string)
                .collect::<Vec<_>>()
                .join(", "),
            type_string(result)
        ),
        TypeRef::Pointer { mutable, inner } => format!(
            "*{} {}",
            if *mutable { "mut" } else { "const" },
            type_string(inner)
        ),
        TypeRef::Reference { mutable, inner } => format!(
            "&{}{}",
            if *mutable { "mut " } else { "" },
            type_string(inner)
        ),
    }
}
pub fn documentation(program: &crate::ast::Program) -> String {
    use crate::ast::Item;
    fn walk(items: &[Item], module: &str, public_scope: bool, out: &mut Vec<String>) {
        for item in items {
            let (item, public) = match item {
                Item::Public(inner) => (inner.as_ref(), true),
                _ => (item, false),
            };
            match item {
                Item::Namespace { name, items } => {
                    let qualified = if module.is_empty() {
                        name.clone()
                    } else {
                        format!("{module}::{name}")
                    };
                    walk(items, &qualified, public_scope && public, out);
                }
                Item::Function(f) if public_scope || public => {
                    let name = if module.is_empty() {
                        f.name.clone()
                    } else {
                        format!("{module}::{}", f.name)
                    };
                    let generic = if f.generics.is_empty() {
                        String::new()
                    } else {
                        format!("<{}>", f.generics.join(", "))
                    };
                    let args = f
                        .params
                        .iter()
                        .map(|p| format!("{}: {}", p.name, type_string(&p.ty)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    out.push(format!(
                        "- `fn {name}{generic}({args}) -> {}`",
                        type_string(&f.result)
                    ));
                }
                Item::Struct(s) if public_scope || public => {
                    let name = if module.is_empty() {
                        s.name.clone()
                    } else {
                        format!("{module}::{}", s.name)
                    };
                    out.push(format!("- `struct {name}`"));
                }
                Item::Enum(e) if public_scope || public => {
                    let name = if module.is_empty() {
                        e.name.clone()
                    } else {
                        format!("{module}::{}", e.name)
                    };
                    out.push(format!("- `enum {name}`"));
                }
                Item::Concept(c) if public_scope || public => {
                    let name = if module.is_empty() {
                        c.name.clone()
                    } else {
                        format!("{module}::{}", c.name)
                    };
                    out.push(format!("- `concept {name}<{}>`", c.generics.join(", ")));
                }
                _ => {}
            }
        }
    }
    let mut lines = vec![];
    walk(&program.items, "", true, &mut lines);
    lines.sort();
    lines.dedup();
    format!(
        "# Nova public API\n\nGenerated symbol index (not doc comments).\n\n{}\n",
        if lines.is_empty() {
            "_No public declarations._".into()
        } else {
            lines.join("\n")
        }
    )
}

pub fn lint(hir: &crate::hir::Program) -> usize {
    use crate::hir::{Expr, ExprKind, FunctionKind, Stmt};
    use std::collections::HashSet;
    fn expr(e: &Expr, used: &mut HashSet<usize>) {
        match &e.kind {
            ExprKind::Local(id) | ExprKind::MoveLocal(id)
            | ExprKind::DropArray { root: id, .. } => { used.insert(*id); }
            ExprKind::AddressOf { place, .. } => {
                used.insert(place.root);
                for p in &place.projections {
                    if let crate::hir::PlaceProjection::Index(index) = p { expr(index, used); }
                }
            }
            ExprKind::Unary(_, x) | ExprKind::Field(x, _) | ExprKind::TakeMatchField(x, _) | ExprKind::Deref(x) => expr(x, used),
            ExprKind::Binary(a, _, b) | ExprKind::Index(a, b) => {
                expr(a, used);
                expr(b, used);
            }
            ExprKind::Call(_, values)
            | ExprKind::StructInit(_, values)
            | ExprKind::Array(values) => {
                for v in values {
                    expr(v, used);
                }
            }
            _ => {}
        }
    }
    fn stmts(statements: &[Stmt], used: &mut HashSet<usize>, modified: &mut HashSet<usize>) {
        for stmt in statements {
            match stmt {
                Stmt::Let { init, .. } => expr(init, used),
                Stmt::Set { local, value } => {
                    modified.insert(*local);
                    expr(value, used);
                }
                Stmt::FieldSet { root, value, .. } => {
                    modified.insert(*root);
                    used.insert(*root);
                    expr(value, used);
                }
                Stmt::DerefSet { local, value } => {
                    used.insert(*local);
                    expr(value, used);
                }
                Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Defer { expr: e, .. } => {
                    expr(e, used)
                }
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    expr(condition, used);
                    stmts(then_body, used, modified);
                    if let Some(other) = else_body {
                        stmts(other, used, modified);
                    }
                }
                Stmt::While { condition, body } => {
                    expr(condition, used);
                    stmts(body, used, modified);
                }
                Stmt::For {
                    local,
                    start,
                    end,
                    body,
                } => {
                    modified.insert(*local);
                    expr(start, used);
                    expr(end, used);
                    stmts(body, used, modified);
                }
                Stmt::Loop(body) | Stmt::Unsafe(body) => stmts(body, used, modified),
                _ => {}
            }
        }
    }
    let mut n = 0;
    for function in hir
        .functions
        .iter()
        .filter(|f| matches!(f.kind, FunctionKind::Defined))
    {
        let mut used = HashSet::new();
        let mut modified = HashSet::new();
        stmts(&function.body, &mut used, &mut modified);
        for (id, local) in function.locals.iter().enumerate() {
            if local.name.starts_with('_') {
                continue;
            }
            if !used.contains(&id) {
                eprintln!(
                    "warning[NOVA-L001]: '{}' in function '{}' is never read",
                    local.name, function.name
                );
                n += 1;
            }
            if local.mutable && !modified.contains(&id) {
                eprintln!("warning[NOVA-L002]: '{}' in function '{}' is declared mut but never reassigned",local.name,function.name);
                n += 1;
            }
        }
    }
    n
}
