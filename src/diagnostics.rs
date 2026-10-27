//! Span-aware compiler diagnostics with neighboring source lines, Unicode-safe
//! caret positioning, and precise byte offsets for parser/module-loader errors.
use lalrpop_util::ParseError;

fn visual_width(source: &str) -> usize {
    source
        .chars()
        .map(|c| {
            let cp = c as u32;
            if c == '\t' {
                4
            } else if (0x0300..=0x036F).contains(&cp) {
                0
            } else if (0x1100..=0x11FF).contains(&cp)
                || (0x2E80..=0xA4CF).contains(&cp)
                || (0xAC00..=0xD7AF).contains(&cp)
                || (0xF900..=0xFAFF).contains(&cp)
                || (0xFE10..=0xFE6F).contains(&cp)
                || (0x1F300..=0x1FAFF).contains(&cp)
            {
                2
            } else {
                1
            }
        })
        .sum()
}

pub fn render(
    code: &str,
    path: &str,
    source: &str,
    byte_start: usize,
    byte_len: usize,
    error: &str,
) -> String {
    let mut start = byte_start.min(source.len());
    while !source.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = start.saturating_add(byte_len).min(source.len());
    while end < source.len() && !source.is_char_boundary(end) {
        end += 1;
    }
    let before = &source[..start];
    let line_num = before.bytes().filter(|b| *b == b'\n').count() + 1;
    let line_start = before.rfind('\n').map(|p| p + 1).unwrap_or(0);
    let column = source[line_start..start].chars().count() + 1;
    let lines = source.lines().collect::<Vec<_>>();
    let digits = (line_num + 2).to_string().len();
    let mut out = format!(
        "error[{code}]: {error}\n  --> {path}:{line_num}:{column}\n{:>width$} |\n",
        "",
        width = digits
    );
    let from = line_num.saturating_sub(2).max(1);
    let to = (line_num + 2).min(lines.len().max(line_num));
    for line in from..=to {
        let content = lines.get(line - 1).copied().unwrap_or("");
        out.push_str(&format!(
            "{line:>digits$} | {}\n",
            content.replace('\t', "    ")
        ));
        if line == line_num {
            let content_end = line_start + content.len();
            let pointed = end.min(content_end).max(start);
            let width = visual_width(&source[start..pointed]).max(1).min(100);
            let indent = visual_width(&source[line_start..start]);
            out.push_str(&format!(
                "{:>digits$} | {}^{}\n",
                "",
                " ".repeat(indent),
                "~".repeat(width.saturating_sub(1))
            ));
        }
    }
    out
}

fn expected_tokens(expected: &[String]) -> String {
    let mut unique = expected.to_vec();
    unique.sort();
    unique.dedup();
    let shown = unique
        .iter()
        .take(8)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if unique.len() > 8 {
        format!("{shown}, ... ({} alternatives)", unique.len())
    } else {
        shown
    }
}

/// Keep the original code of an already classified error (e.g. NOVA-C001).
/// For plain semantic errors choose a stable, machine-readable family instead
/// of reporting every unrelated failure as NOVA-T001.
fn diagnostic_parts(detail: &str) -> (&str, &str) {
    if let Some(rest) = detail.strip_prefix("error[") {
        if let Some((code, message)) = rest.split_once("]: ") {
            if code.starts_with("NOVA-")
                && code.len() <= 16
                && code
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-')
            {
                return (code, message);
            }
        }
    }
    let first = detail.lines().next().unwrap_or(detail);
    if first.contains("expected ") && first.contains("found ") {
        ("NOVA-T002", detail)
    } else if first.contains("unknown field") || first.contains("has no field") {
        ("NOVA-T003", detail)
    } else if first.contains("unknown function") || first.contains("cannot resolve use") {
        ("NOVA-T004", detail)
    } else if first.contains("cannot infer generic") || first.contains("generic '") {
        ("NOVA-G001", detail)
    } else {
        ("NOVA-T001", detail)
    }
}

/// The compiler currently retains only enclosing function spans for many
/// lowered constructs. Show candidate `sync` lines as *context*, never as a
/// fabricated precise location for a type error.
fn sync_context(source: &crate::modules::SourceFile, declaration: usize) -> Vec<(usize, String)> {
    let clean = crate::strip_comments(&source.text);
    let Some(open_offset) = clean[declaration..].find('{') else {
        return Vec::new();
    };
    let open = declaration + open_offset;
    let mut depth = 0usize;
    let mut closing = None;
    let mut string = false;
    let mut escaped = false;
    for (relative, ch) in clean[open..].char_indices() {
        if string {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                string = false;
            }
        } else {
            match ch {
                '"' => string = true,
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        closing = Some(open + relative + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    let Some(end) = closing else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let start_line = clean[..declaration].bytes().filter(|c| *c == b'\n').count() + 1;
    for (index, line) in clean[declaration..end].lines().enumerate() {
        if line.contains("sync(") {
            lines.push((start_line + index, line.trim().to_owned()));
            if lines.len() == 5 {
                break;
            }
        }
    }
    lines
}

/// Carefully scoped suggestions: never assume a field borrow is safe or that an
/// API supports a feature which has not reached typed MIR yet.
fn suggestions(detail: &str) -> Vec<String> {
    let mut tips = Vec::new();
    if detail.contains("requires a named receiver for implicit borrow") {
        tips.push("the method borrows its receiver, but this call uses a field or temporary instead of a named local".into());
        tips.push("the current compiler cannot safely auto-borrow a field projection; expose an operation on the owning type or implement place-borrow lowering in the compiler".into());
        tips.push("do not copy an owning field into a temporary just to silence this error: that can violate ownership".into());
    } else if detail.contains("is not defined for") && detail.contains("receiver type") {
        tips.push("check the receiver type and whether the method expects `self`, `&self`, or `&mut self`".into());
        tips.push(
            "methods from other structs with the same name do not apply to this receiver".into(),
        );
    } else if detail.contains("is missing required argument") {
        tips.push("provide the missing argument, or use a declared default parameter if the function supports one".into());
        if detail.contains("'insert'") {
            tips.push("for example, `Map.insert(key, value)` takes two arguments while `Set.insert(value)` takes one".into());
        }
    } else if detail.contains("expects") && detail.contains("explicit arguments") {
        tips.push(
            "check the receiver type and pass the number of arguments declared for that method"
                .into(),
        );
    } else if detail.contains("unknown field") || detail.contains("has no field") {
        tips.push(
            "check the struct declaration for available field names; fields are case-sensitive"
                .into(),
        );
    } else if detail.contains("unknown function") || detail.contains("cannot resolve use") {
        tips.push(
            "check the function/module name, import path and visibility (`pub` where appropriate)"
                .into(),
        );
    } else if detail.contains("cannot assign to field of immutable local") {
        tips.push("declare the owning local as `let mut` when mutation is intended, or use a mutable receiver if the language's ownership rules permit it".into());
    } else if detail.contains("constraint") || detail.contains("does not satisfy concept") {
        tips.push("inspect the concept's required methods and types, then use a type that satisfies that contract".into());
        if detail.contains("MapKey")
            || detail.contains("MapValue")
            || detail.contains("VectorElement")
        {
            tips.push("this STD bootstrap currently supports signed integral container elements; arbitrary owned objects need dedicated compiler/runtime support".into());
        }
    } else if detail.contains("expected") && detail.contains("found") {
        if detail.contains("std::sync::acquire") || detail.contains("std::sync::acquire_pair") {
            tips.push("`sync(mutex)` already borrows its named mutex for the guard; supplying a local whose type is `&MutexI64` results in a nested reference (`&&MutexI64`)".into());
            tips.push("in this compiler version, use `sync` on a named owning mutex local; borrowed mutex parameters in `sync` are not supported yet. Do not remove borrowing checks or insert an unsafe cast".into());
        } else if detail.contains("found &")
            || detail.contains("found &mut ")
            || detail.contains("supplied argument: &")
        {
            tips.push("a borrowed value (`&T` or `&mut T`) and an owned value (`T`) are different types; check the parameter signature and the borrow operator (`&`) at the call site".into());
            tips.push("do not add a cast or clone just to silence an ownership mismatch; the callee may require a different API or borrowing mode".into());
        } else {
            tips.push("compare the declared parameter type with the supplied argument type; check generic substitutions before adding an explicit conversion".into());
        }
    }
    if detail.contains("NOVA-M015") || detail.contains("standard-library runtime layer is private")
    {
        tips.push("applications must use the public `std::map`, `std::vec`, and other safe wrappers; do not call `std::internal` directly".into());
        tips.push("if this originates in `<builtin:std/...>`, check whether a transitive STD import was resolved against the wrong instance".into());
    }
    if detail.contains("only std = { builtin = \"std\" } is supported") {
        tips.push("declare `[dependencies] std = { builtin = \"std\" }`; this compiler does not allow renaming the built-in STD dependency".into());
    }
    tips
}

fn add_suggestions(out: &mut String, detail: &str) {
    for (index, help) in suggestions(detail).iter().enumerate() {
        let label = if index == 0 { "note" } else { "help" };
        out.push_str(&format!("  = {label}: {help}\n"));
    }
}

/// An enclosing function is navigation context, never the exact expression
/// span. Includes embedded STD sources when specializing dependency code.
fn related_declaration<'a>(
    frames: &[&str],
    sources: &'a [crate::modules::SourceFile],
) -> Option<(&'a crate::modules::SourceFile, usize, String)> {
    let active = frames
        .first()?
        .trim()
        .strip_prefix("specializing function '")?
        .strip_suffix('\'')?;
    let (namespace, name) = active.rsplit_once("::").unwrap_or(("", active));
    let needle = format!("fn {name}(");
    let mut candidates = Vec::new();
    for source in sources {
        let matches_ns = source.namespace == namespace
            || (!source.namespace.is_empty()
                && namespace.starts_with(&format!("{}::", source.namespace)))
            || (source.namespace.is_empty() && namespace.is_empty());
        if !matches_ns {
            continue;
        }
        let clean = crate::strip_comments(&source.text);
        for (at, _) in clean.match_indices(&needle) {
            candidates.push((source, at, active.to_owned()));
        }
    }
    if candidates.len() == 1 {
        candidates.pop()
    } else {
        None
    }
}

pub fn parser<T: std::fmt::Debug, E: std::fmt::Debug>(
    path: &str,
    source: &str,
    error: ParseError<usize, T, E>,
) -> String {
    let (at,len,message)=match error {
        ParseError::InvalidToken{location}=>(location.saturating_sub(1),1,"invalid token".to_string()),
        ParseError::UnrecognizedEof{location,expected}=>(location,1,format!("unexpected end of file; expected {}",expected_tokens(&expected))),
        ParseError::UnrecognizedToken{token:(start,token,end),expected}=>(start,end.saturating_sub(start),format!("unexpected token {token:?}; expected {}",expected_tokens(&expected))),
        ParseError::ExtraToken{token:(start,token,end)}=>(start,end.saturating_sub(start),format!("unexpected extra token {token:?}")),
        ParseError::User{error}=>return format!("error[NOVA-P001]: lexer error: {error:?}\n  --> {path}\n  = note: the lexer did not provide a location"),
    };
    let mut out = render("NOVA-P001", path, source, at, len, &message);
    if message.starts_with("unexpected end of file") {
        out.push_str("  = help: look for an unclosed `}`, `)`, `]`, or a missing final statement terminator near the end of the file\n");
    } else if message.starts_with("unexpected token") {
        out.push_str("  = help: check delimiters and the previous statement; an unexpected token often follows a missing `;`, `)` or `}`\n");
    } else if message.starts_with("unexpected extra token") {
        out.push_str("  = help: remove the extra token or check whether the previous declaration was closed too early\n");
    }
    out
}

/// Search for an unambiguous range header without pretending the AST has spans.
/// Returns a source location only when precisely one textual candidate exists.
/// Source spans on expressions remain a future compiler architectural change.
fn locate_for_variable<'a>(
    name: &str,
    sources: &'a [crate::modules::SourceFile],
) -> Vec<(&'a crate::modules::SourceFile, usize, usize)> {
    let mut result = Vec::new();
    for source in sources.iter().filter(|s| !s.path.starts_with('<')) {
        // strip_comments preserves byte offsets, even for Unicode comments.
        let clean = crate::strip_comments(&source.text);
        let data = clean.as_bytes();
        let mut cursor = 0usize;
        while cursor + 3 <= data.len() {
            if &data[cursor..cursor + 3] != b"for" {
                cursor += 1;
                continue;
            }
            let before = cursor.checked_sub(1).and_then(|p| data.get(p)).copied();
            let after = data.get(cursor + 3).copied();
            let ident = |v: u8| v.is_ascii_alphanumeric() || v == b'_';
            if before.is_some_and(ident) || after.is_some_and(ident) {
                cursor += 3;
                continue;
            }
            let mut at = cursor + 3;
            while data.get(at).is_some_and(|x| x.is_ascii_whitespace()) {
                at += 1;
            }
            if !clean[at..].starts_with(name) {
                cursor += 3;
                continue;
            }
            at += name.len();
            if data.get(at).is_some_and(|x| ident(*x)) {
                cursor += 3;
                continue;
            }
            while data.get(at).is_some_and(|x| x.is_ascii_whitespace()) {
                at += 1;
            }
            if clean[at..].starts_with("in") && data.get(at + 2).map_or(true, |x| !ident(*x)) {
                let header_end = clean[at..].find('{').map(|n| at + n).unwrap_or(at + 2);
                result.push((source, cursor, header_end.saturating_sub(cursor).min(120)));
            }
            cursor += 3;
        }
    }
    result
}

/// Best-effort precise token range for a semantic error in the *active*
/// function specialization. The current AST does not carry expression spans,
/// so only return a range if a single source token satisfies all constraints.
/// This must never pretend an enclosing function declaration is the error.
fn locate_in_active_function<'a>(
    detail: &str,
    frames: &[&str],
    sources: &'a [crate::modules::SourceFile],
) -> Option<(&'a crate::modules::SourceFile, usize, usize)> {
    let active = frames
        .first()?
        .trim()
        .strip_prefix("specializing function '")?
        .strip_suffix('\'')?;
    let function = active.rsplit("::").next()?;
    let namespace = active.rsplit_once("::").map(|(n, _)| n).unwrap_or("");
    let (token, member) = if detail.starts_with("unknown field '") {
        (detail.split('\'').nth(1)?, true)
    } else if detail.contains("' has no field '") {
        (detail.split('\'').nth(3)?, true)
    } else if detail.starts_with("unknown function '") {
        (detail.split('\'').nth(1)?, false)
    } else {
        return None;
    };
    if token.is_empty() {
        return None;
    }
    let ident = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
    let declaration = format!("fn {function}(");
    let mut matches = Vec::new();
    for source in sources.iter().filter(|s| !s.path.starts_with('<')) {
        // A method specialization carries its impl type in the trace, while
        // SourceFile.namespace names the module (without the impl type).
        if !namespace.is_empty()
            && !(source.namespace == namespace
                || (!source.namespace.is_empty()
                    && namespace.starts_with(&format!("{}::", source.namespace)))
                || (source.namespace.is_empty() && !namespace.contains("::")))
        {
            continue;
        }
        let text = &source.text;
        // strip_comments preserves UTF-8 byte offsets.
        let clean = crate::strip_comments(text);
        for (header, _) in clean.match_indices(&declaration) {
            let boundary = header + declaration.len();
            let next = clean[boundary..]
                .find("fn ")
                .map(|relative| boundary + relative)
                .unwrap_or(clean.len());
            let haystack = &clean[header..next];
            let needle = if member {
                format!(".{token}")
            } else {
                token.to_owned()
            };
            for (relative, _) in haystack.match_indices(&needle) {
                let start = header + relative + if member { 1 } else { 0 };
                let end = start + token.len();
                // No partial matches such as .top when looking for .to.
                if clean[end..].chars().next().is_some_and(ident) {
                    continue;
                }
                if !member && clean[..start].chars().last().is_some_and(ident) {
                    continue;
                }
                // Text inside a string literal should not be a semantic token.
                let line_start = clean[..start].rfind('\n').map(|n| n + 1).unwrap_or(0);
                let before = &clean[line_start..start];
                let mut in_string = false;
                let mut escaped = false;
                for c in before.chars() {
                    if escaped {
                        escaped = false;
                        continue;
                    }
                    if in_string && c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        in_string = !in_string;
                    }
                }
                if in_string {
                    continue;
                }
                matches.push((source, start, token.len()));
            }
        }
    }
    if matches.len() == 1 {
        matches.pop()
    } else {
        None
    }
}

/// Return a fully contextualized error for the compilation phase, package and
/// real source candidates. `nova-trace` frames come from actual compiler calls.
/// No runtime stack trace exists for a semantic compile-time type error.
pub fn semantic_with_context(
    error: &str,
    sources: &[crate::modules::SourceFile],
    input: &std::path::Path,
    phase: &str,
) -> String {
    let mut chunks = error.split("\n  = nova-trace: ");
    let raw_detail = chunks.next().unwrap_or(error);
    let (code, detail) = diagnostic_parts(raw_detail);
    let frames = chunks.collect::<Vec<_>>();
    let package_file = if input.is_dir() {
        input.join("Nova.toml")
    } else {
        input.parent().unwrap_or(input).join("Nova.toml")
    };
    let package_name = std::fs::read_to_string(&package_file)
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.trim().strip_prefix("name = "))
                .map(|v| v.trim().trim_matches('"').to_string())
        })
        .unwrap_or_else(|| {
            input
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".into())
        });
    let mut header = format!(
        "  = package: {package_name} ({})\n  = compilation phase: {phase}\n",
        input.display()
    );
    if !frames.is_empty() {
        header.push_str("  = compiler trace (function specialization, innermost first):\n");
        for (i, frame) in frames.iter().enumerate() {
            header.push_str(&format!("    {i}: {}\n", frame.trim()));
        }
    }
    let located = locate_in_active_function(detail, &frames, sources);
    let mut result = if let Some((source, at, len)) = located {
        let mut out = render(code, &source.path, &source.text, at, len, detail);
        out.push_str("  = note: token inferred uniquely within the active specialization; exact AST spans are not yet available\n");
        out
    } else if detail.starts_with("for range bounds must have equal types; variable '") {
        let name = detail.split('\'').nth(1).unwrap_or("");
        let candidates = locate_for_variable(name, sources);
        if candidates.len() == 1 {
            let (source, offset, len) = candidates[0];
            let mut out = render(code, &source.path, &source.text, offset, len, detail);
            out.push_str(
                "  = help: both bounds of `for x in start..end` must have the same integer type\n",
            );
            out.push_str("  = help: use an explicitly typed lower bound, or convert the upper bound to match\n");
            out
        } else {
            let mut out = format!("error[{code}]: {detail}\n");
            if !candidates.is_empty() {
                out.push_str("  = candidate range locations (not uniquely identified):\n");
                for (source, offset, _) in candidates.iter().take(8) {
                    let prefix = &source.text[..(*offset).min(source.text.len())];
                    let line = prefix.bytes().filter(|x| *x == b'\n').count() + 1;
                    let excerpt = source
                        .text
                        .lines()
                        .nth(line.saturating_sub(1))
                        .unwrap_or("")
                        .trim();
                    out.push_str(&format!(
                        "    --> {}:{}\n        {}\n",
                        source.path, line, excerpt
                    ));
                }
            } else {
                out.push_str("  = note: no exact source span was retained for this AST node\n");
            }
            out.push_str("  = help: define both range bounds with the same integer type (e.g. `let zero: i64 = 0;`)\n");
            out
        }
    } else {
        let quoted = detail.split('\'').nth(1).unwrap_or("");
        let mut locations = Vec::new();
        if !quoted.is_empty() {
            for source in sources.iter().filter(|s| !s.path.starts_with('<')) {
                for (at, _) in source.text.match_indices(quoted) {
                    let before = source.text[..at].chars().last();
                    let after = source.text[at + quoted.len()..].chars().next();
                    if before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                        || after.is_some_and(|c| c.is_alphanumeric() || c == '_')
                    {
                        continue;
                    }
                    locations.push((source, at));
                }
            }
        }
        if locations.len() == 1 {
            let (source, at) = locations[0];
            let mut out = render(code, &source.path, &source.text, at, quoted.len(), detail);
            out.push_str(
                "  = note: location inferred from one symbol occurrence, not an AST span\n",
            );
            out
        } else {
            let mut out=format!("error[{code}]: {detail}\n  = note: no exact expression span available; showing reliable enclosing context instead\n");
            if let Some((source, at, qualified)) = related_declaration(&frames, sources) {
                let line = source.text[..at].bytes().filter(|b| *b == b'\n').count() + 1;
                let excerpt = source.text.lines().nth(line - 1).unwrap_or("").trim();
                out.push_str(&format!(
                    "  = in function: {qualified}\n  = related declaration: {}:{}\n    | {}\n",
                    source.path, line, excerpt
                ));
                out.push_str("  = note: this is the enclosing declaration, not the precise failing expression\n");
                if detail.contains("std::sync::acquire")
                    || detail.contains("std::sync::acquire_pair")
                {
                    let candidates = sync_context(source, at);
                    if !candidates.is_empty() {
                        out.push_str("  = related `sync` expressions in this function (candidates, not exact spans):\n");
                        for (line, expression) in candidates {
                            out.push_str(&format!(
                                "    --> {}:{line}: {expression}\n",
                                source.path
                            ));
                        }
                    }
                }
            }
            out
        }
    };
    add_suggestions(&mut result, detail);
    result.push_str(&header);
    result
}

/// Add context to syntax/module-loader errors while keeping their existing
/// precise snippet. Unlike a traceback this records compiler pipeline stages.
pub fn attach_package(error: &str, input: &std::path::Path, phase: &str) -> String {
    let root = if input.is_dir() {
        input
    } else {
        input.parent().unwrap_or(input)
    };
    let manifest = root.join("Nova.toml");
    let name = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|raw| {
            let mut in_package = false;
            for line in raw.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_package = line == "[package]";
                }
                if in_package {
                    if let Some(value) = line.strip_prefix("name = ") {
                        return Some(value.trim().trim_matches('"').to_owned());
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| {
            root.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "standalone".into())
        });
    let mut out = error.to_owned();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    add_suggestions(&mut out, error);
    out.push_str(&format!(
        "  = package: {name} ({})\n  = compilation phase: {phase}\n",
        root.display()
    ));
    out
}

pub fn pipeline_error(
    code: &str,
    error: &str,
    sources: &[crate::modules::SourceFile],
    input: &std::path::Path,
    phase: &str,
) -> String {
    let mut result = format!("error[{code}]: {error}\n");
    let file_list = sources
        .iter()
        .filter(|s| !s.path.starts_with('<'))
        .take(8)
        .collect::<Vec<_>>();
    if !file_list.is_empty() {
        result.push_str("  = related package files (precise failing span unavailable):\n");
        for s in file_list {
            result.push_str(&format!("    {}\n", s.path));
        }
    }
    attach_package(&result, input, phase)
}

/// Compatibility entry point used by older compilation tests.
pub fn semantic_with_sources(error: &str, sources: &[crate::modules::SourceFile]) -> String {
    semantic_with_context(
        error,
        sources,
        std::path::Path::new("<unknown>"),
        "semantic",
    )
}
pub fn semantic(error: &str) -> String {
    let (code, detail) = diagnostic_parts(error);
    format!("error[{code}]: {detail}")
}

#[cfg(test)]
mod rich_diagnostics_tests {
    use super::*;
    use crate::modules::SourceFile;
    fn source(path: &str, text: &str) -> SourceFile {
        SourceFile {
            path: path.into(),
            text: text.into(),
            namespace: String::new(),
        }
    }

    #[test]
    fn borrowed_mutex_error_has_readable_types_and_actionable_sync_context() {
        let source=source("/project/src/main.nova",
            "fn reverse(a: &std::sync::MutexI64, b: &std::sync::MutexI64) -> () {\n    sync(b) as g {\n        sync(a) as h { }\n    }\n}\n");
        let error="expected std::sync::MutexI64, found &std::sync::MutexI64\n  = argument 1 (`mutex`) of call `std::sync::acquire`\n  = declared parameter: &std::sync::MutexI64\n  = supplied argument: &&std::sync::MutexI64\n  = nova-trace: specializing function 'reverse'";
        let diagnostic = semantic_with_context(
            error,
            &[source],
            std::path::Path::new("/project"),
            "surface",
        );
        assert!(
            diagnostic.contains(
                "error[NOVA-T002]: expected std::sync::MutexI64, found &std::sync::MutexI64"
            ),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("argument 1 (`mutex`)"), "{diagnostic}");
        assert!(diagnostic.contains("already borrows"), "{diagnostic}");
        assert!(
            diagnostic.contains("related `sync` expressions"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("/project/src/main.nova:2:"),
            "{diagnostic}"
        );
        assert!(
            !diagnostic.contains("--> /project/src/main.nova:2:1\n"),
            "{diagnostic}"
        );
    }
    #[test]
    fn concurrency_code_is_not_double_wrapped() {
        let source = source("/project/src/main.nova", "fn main() -> i32 { return 0; }");
        let diagnostic = semantic_with_context(
            "error[NOVA-C002]: lock already held\n  help: reuse the guard",
            &[source],
            std::path::Path::new("/project"),
            "surface",
        );
        assert!(
            diagnostic.starts_with("error[NOVA-C002]: lock already held"),
            "{diagnostic}"
        );
        assert!(
            !diagnostic.contains("error[NOVA-T001]: error[NOVA-C002]"),
            "{diagnostic}"
        );
    }
    #[test]
    fn ordinary_primitive_mismatch_is_classified_without_ownership_hint() {
        let s = source("/project/src/main.nova", "fn main() -> i32 { return 0; }");
        let d = semantic_with_context(
            "expected bool, found str",
            &[s],
            std::path::Path::new("/project"),
            "surface",
        );
        assert!(
            d.contains("error[NOVA-T002]: expected bool, found str"),
            "{d}"
        );
        assert!(!d.contains("nested reference"), "{d}");
    }
    #[test]
    fn field_assignment_reports_the_failing_token_not_line_zero() {
        let body = "pub struct Response { descriptor: i32, status_code: i32 }\nimpl Response {\n fn status(self: Response, code: i32) -> Response {\n   self.to = 1;\n   return self;\n }\n}";
        let s = SourceFile {
            path: "/work/packages/nexo/src/response.nova".into(),
            text: body.into(),
            namespace: "nexo::response".into(),
        };
        let err = "'Response' has no field 'to'\n  = nova-trace: specializing function 'nexo::response::Response::status'";
        let output = semantic_with_context(
            err,
            &[s],
            std::path::Path::new("/work/packages/nexo"),
            "surface",
        );
        assert!(output.contains("/response.nova:4:9"), "{output}");
        assert!(!output.contains("related function declaration"), "{output}");
    }
    #[test]
    fn field_location_is_not_guessed_if_two_references_exist() {
        let body = "impl Response {\n fn status(self: Response) -> Response {\n   self.to = 1;\n   self.to = 2;\n   return self;\n }\n}";
        let s = source("/work/src/main.nova", body);
        let err = "'Response' has no field 'to'\n  = nova-trace: specializing function 'Response::status'";
        let output = semantic_with_context(err, &[s], std::path::Path::new("/work"), "surface");
        assert!(!output.contains("/main.nova:3:9"), "{output}");
        assert!(!output.contains("/main.nova:4:9"), "{output}");
    }
    #[test]
    fn wrong_field_in_comment_is_ignored() {
        let body = "impl Response {\n fn status(self: Response) -> Response {\n   // self.to is only in a comment\n   self.to = 1;\n   return self;\n }\n}";
        let s = source("/work/src/main.nova", body);
        let err = "'Response' has no field 'to'\n  = nova-trace: specializing function 'Response::status'";
        let output = semantic_with_context(err, &[s], std::path::Path::new("/work"), "surface");
        assert!(output.contains("/main.nova:4:9"), "{output}");
    }
    #[test]
    fn range_error_reports_path_types_and_compiler_frames() {
        let s = source(
            "/tmp/example/src/main.nova",
            "fn main() -> i32 {\n  let n: i64 = 3;\n  for i in 0..n { }\n  return 0;\n}",
        );
        let err="for range bounds must have equal types; variable 'i' starts as Named(\"i32\"), but ends as Named(\"i64\")\n  = nova-trace: specializing function 'main'";
        let diagnostic =
            semantic_with_context(err, &[s], std::path::Path::new("/tmp/example"), "surface");
        assert!(
            diagnostic.contains("/tmp/example/src/main.nova:3:3"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("for i in 0..n"), "{diagnostic}");
        assert!(diagnostic.contains("package: example"), "{diagnostic}");
        assert!(diagnostic.contains("compiler trace"), "{diagnostic}");
    }
    #[test]
    fn ambiguous_range_never_claims_a_false_precise_location() {
        let s = source(
            "/tmp/demo/src/main.nova",
            "fn main() -> i32 { for i in 0..n { } for i in 0..m { } return 0; }",
        );
        let diagnostic = semantic_with_context(
            "for range bounds must have equal types; variable 'i' starts as i32, but ends as i64",
            &[s],
            std::path::Path::new("/tmp/demo"),
            "surface",
        );
        assert!(
            diagnostic.contains("candidate range locations"),
            "{diagnostic}"
        );
        assert!(
            !diagnostic.contains("--> /tmp/demo/src/main.nova:1:19\n"),
            "{diagnostic}"
        );
    }
    #[test]
    fn field_borrow_limitation_has_actionable_explanation_and_real_std_context() {
        let s=SourceFile{path:"<builtin:std/src/symbol.nova>".into(),namespace:"std::symbol".into(),
            text:"impl SymbolTable {\n fn define(self: &SymbolTable, key: str, id: i64) -> bool {\n  return self.entries.insert(key,id);\n }\n}".into()};
        let err="method 'insert' requires a named receiver for implicit borrow\n  = nova-trace: specializing function 'std::symbol::SymbolTable::define'";
        let out = semantic_with_context(err, &[s], std::path::Path::new("/tmp/demo"), "surface");
        assert!(
            out.contains("cannot safely auto-borrow a field projection"),
            "{out}"
        );
        assert!(out.contains("<builtin:std/src/symbol.nova>:2"), "{out}");
        assert!(out.contains("not the precise failing expression"), "{out}");
        assert!(
            !out.contains("error[NOVA-T001]: related function declaration"),
            "{out}"
        );
    }
    #[test]
    fn method_argument_suggestion_is_contextual_without_claiming_precision() {
        let s = source(
            "/demo/src/main.nova",
            "fn main() -> i32 {\n  seen.insert(42);\n  return 0;\n}",
        );
        let out = semantic_with_context(
            "'insert' is missing required argument 'value'",
            &[s],
            std::path::Path::new("/demo"),
            "surface",
        );
        assert!(out.contains("Map.insert(key, value)"), "{out}");
        assert!(out.contains("Set.insert(value)"), "{out}");
    }
    #[test]
    fn builtin_privacy_diagnostic_has_safe_api_hint() {
        let raw="error[NOVA-M015]: the standard-library runtime layer is private\n  --> <builtin:std/src/map.nova>:54:17\n";
        let out = attach_package(raw, std::path::Path::new("/tmp/demo"), "module loading");
        assert!(out.contains("public `std::map`"), "{out}");
        assert!(out.contains("transitive STD import"), "{out}");
    }
    #[test]
    fn parser_unexpected_eof_suggests_delimiters() {
        let out = parser::<String, String>(
            "test.nova",
            "fn main() {",
            ParseError::UnrecognizedEof {
                location: 11,
                expected: vec!["\"}\"".into()],
            },
        );
        assert!(out.contains("unclosed"), "{out}");
    }
    #[test]
    fn generic_unspanned_error_displays_real_function_context() {
        let s = source(
            "/tmp/demo/src/main.nova",
            "fn main() -> i32 {\n  missing();\n  return 0;\n}",
        );
        let error = "unknown function 'missing'\n  = nova-trace: specializing function 'main'";
        let result =
            semantic_with_context(error, &[s], std::path::Path::new("/tmp/demo"), "surface");
        assert!(result.contains("/tmp/demo/src/main.nova"), "{result}");
        assert!(result.contains("unknown function 'missing'"), "{result}");
    }
}
