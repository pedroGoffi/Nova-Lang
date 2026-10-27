//! Statically parsed format strings for Nova's built-in output macros.
//! Expansion produces ordinary AST statements; only the value-write dispatch
//! marker is handled by the surface type checker after types are inferred.
use crate::ast::{Expr, Stmt};

pub const WRITE_VALUE: &str = "__nova_format_internal_write_value";
/// Private AST nodes: consumed by the surface typer, never emitted to HIR.
pub const BUILD: &str = "__nova_format_internal_build";
pub const TEXT_PART: &str = "__nova_format_internal_text_part";
pub const VALUE_PART: &str = "__nova_format_internal_value_part";

fn quoted(text: &str) -> Expr {
    let mut escaped = String::from("\"");
    for ch in text.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c => escaped.push(c),
        }
    }
    escaped.push('"');
    Expr::Str(escaped)
}

fn decode_string(raw: &str) -> Result<String, String> {
    let Some(raw) = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Err("error[NOVA-FMT001]: format string must be a literal".into());
    };
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        out.push(match chars.next() {
            Some('n') => '\n',
            Some('r') => '\r',
            Some('t') => '\t',
            Some('0') => '\0',
            Some('\\') => '\\',
            Some('"') => '"',
            Some(other) => {
                return Err(format!(
                    "error[NOVA-FMT004]: unsupported format-string escape `\\{other}`"
                ))
            }
            None => return Err("error[NOVA-FMT004]: incomplete escape in format string".into()),
        });
    }
    Ok(out)
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn write_text(text: &str, stderr: bool) -> Stmt {
    Stmt::Expr(Expr::Call(
        if stderr {
            "std::io::native_eprint"
        } else {
            "std::io::native_print"
        }
        .into(),
        vec![quoted(text)],
    ))
}

fn flush_literal(buffer: &mut String, output: &mut Vec<Stmt>, stderr: bool) {
    if !buffer.is_empty() {
        output.push(write_text(buffer, stderr));
        buffer.clear();
    }
}

/// Return None for user-defined macros; built-in macros cannot be overridden
/// accidentally, including within nested user macro_rules! expansions.
pub fn expand(name: &str, arguments: &[Expr]) -> Option<Result<Vec<Stmt>, String>> {
    let (stderr, newline) = match name {
        "print" => (false, false),
        "println" => (false, true),
        "eprint" => (true, false),
        "eprintln" => (true, true),
        _ => return None,
    };
    Some(expand_output(name, arguments, stderr, newline))
}

fn expand_output(
    name: &str,
    arguments: &[Expr],
    stderr: bool,
    newline: bool,
) -> Result<Vec<Stmt>, String> {
    if arguments.is_empty() {
        return Ok(if newline {
            vec![write_text("\n", stderr)]
        } else {
            vec![]
        });
    }
    let Expr::Str(raw_format) = &arguments[0] else {
        return Err(format!("error[NOVA-FMT001]: `{name}!` requires a compile-time string literal as its first argument"));
    };
    let format = decode_string(raw_format)?;
    let mut cursor = format.chars().peekable();
    let mut output = Vec::new();
    let mut literal = String::new();
    let mut used_positional = 0_usize;
    while let Some(ch) = cursor.next() {
        match ch {
            '{' if cursor.peek() == Some(&'{') => {
                cursor.next();
                literal.push('{');
            }
            '}' if cursor.peek() == Some(&'}') => {
                cursor.next();
                literal.push('}');
            }
            '{' => {
                let mut inside = String::new();
                let mut closed = false;
                for part in cursor.by_ref() {
                    if part == '}' {
                        closed = true;
                        break;
                    }
                    if part == '{' {
                        return Err("error[NOVA-FMT003]: nested `{` in a format placeholder".into());
                    }
                    inside.push(part);
                }
                if !closed {
                    return Err("error[NOVA-FMT003]: unclosed `{` in format string".into());
                }
                let (binding, spec) = inside.split_once(':').unwrap_or((&inside, ""));
                let (debug, precision) = if spec.is_empty() {
                    (false, -1)
                } else if spec == "?" {
                    (true, -1)
                } else if let Some(digits) = spec.strip_prefix('.') {
                    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(format!(
                            "error[NOVA-FMT004]: invalid precision `{{:{spec}}}`"
                        ));
                    }
                    let parsed: i32 = digits
                        .parse()
                        .map_err(|_| "error[NOVA-FMT004]: precision is too large".to_owned())?;
                    if parsed > 18 {
                        return Err("error[NOVA-FMT004]: precision must be at most 18".into());
                    }
                    (false, parsed)
                } else {
                    return Err(format!(
                        "error[NOVA-FMT004]: unsupported format specifier `{{:{spec}}}`"
                    ));
                };
                let value = if binding.is_empty() {
                    let value = arguments.get(used_positional + 1).ok_or_else(|| {
                        format!("error[NOVA-FMT002]: `{name}!` has more `{{}}` placeholders than arguments")
                    })?;
                    used_positional += 1;
                    value.clone()
                } else if valid_name(binding) {
                    Expr::Var(binding.into())
                } else {
                    return Err(format!(
                        "error[NOVA-FMT004]: invalid named placeholder `{{{binding}}}`"
                    ));
                };
                flush_literal(&mut literal, &mut output, stderr);
                output.push(Stmt::Expr(Expr::Call(
                    WRITE_VALUE.into(),
                    vec![
                        value,
                        Expr::Int(precision.to_string()),
                        Expr::Bool(debug),
                        Expr::Bool(stderr),
                    ],
                )));
            }
            '}' => return Err("error[NOVA-FMT003]: unmatched `}` in format string".into()),
            other => literal.push(other),
        }
    }
    if used_positional + 1 != arguments.len() {
        return Err(format!(
            "error[NOVA-FMT002]: `{name}!` received {} positional argument(s), but used {used_positional}",
            arguments.len() - 1
        ));
    }
    if newline {
        literal.push('\n');
    }
    flush_literal(&mut literal, &mut output, stderr);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn literal(format: &str, args: Vec<Expr>) -> Result<Vec<Stmt>, String> {
        let mut all = vec![Expr::Str(format.into())];
        all.extend(args);
        expand("println", &all).unwrap()
    }
    #[test]
    fn splits_literals_and_preserves_argument_order() {
        let stmts = literal(
            "\"Olá {}: {:.2}\"",
            vec![Expr::Var("name".into()), Expr::Float("29.90".into())],
        )
        .unwrap();
        assert_eq!(stmts.len(), 5);
        assert!(
            matches!(&stmts[1],Stmt::Expr(Expr::Call(n,v)) if n==WRITE_VALUE && matches!(&v[0],Expr::Var(name) if name=="name"))
        );
        assert!(
            matches!(&stmts[3],Stmt::Expr(Expr::Call(n,v)) if n==WRITE_VALUE && matches!(&v[1],Expr::Int(p) if p=="2"))
        );
    }
    #[test]
    fn escaped_braces_and_named_captures() {
        let stmts = literal("\"{{ok}} {name}\"", vec![]).unwrap();
        assert_eq!(stmts.len(), 3);
        assert!(
            matches!(&stmts[0],Stmt::Expr(Expr::Call(_,v)) if matches!(&v[0],Expr::Str(s) if s=="\"{ok} \""))
        );
        assert!(
            matches!(&stmts[1],Stmt::Expr(Expr::Call(_,v)) if matches!(&v[0],Expr::Var(s) if s=="name"))
        );
    }
    #[test]
    fn no_argument_print_and_println_are_valid() {
        assert!(expand("print", &[]).unwrap().unwrap().is_empty());
        assert_eq!(expand("println", &[]).unwrap().unwrap().len(), 1);
    }
    #[test]
    fn malformed_formats_are_compile_errors() {
        assert!(literal("\"{} {}\"", vec![Expr::Int("1".into())])
            .unwrap_err()
            .contains("NOVA-FMT002"));
        assert!(literal("\"{\"", vec![])
            .unwrap_err()
            .contains("NOVA-FMT003"));
        assert!(literal("\"{:x}\"", vec![Expr::Int("1".into())])
            .unwrap_err()
            .contains("NOVA-FMT004"));
        assert!(literal("\"hello\"", vec![Expr::Int("1".into())])
            .unwrap_err()
            .contains("NOVA-FMT002"));
    }
}

/// Build an owned String expression using exactly the same format parser as
/// print!/println!. The resulting private AST markers are replaced by typed
/// standard-library calls in surface lowering, not emitted to LLVM as-is.
pub fn expand_format(arguments: &[Expr]) -> Result<Expr, String> {
    if arguments.is_empty() {
        return Err("error[NOVA-FMT001]: `format!` requires a literal format string".into());
    }
    let output = expand_output("format", arguments, false, false)?;
    let mut parts = Vec::with_capacity(output.len());
    for statement in output {
        let Stmt::Expr(Expr::Call(name, mut values)) = statement else {
            unreachable!("format AST parser emitted a non-call statement")
        };
        if name == "std::io::native_print" {
            parts.push(Expr::Call(TEXT_PART.into(), values));
        } else if name == WRITE_VALUE {
            debug_assert_eq!(values.len(), 4);
            values.pop(); // stderr flag: formatting has no output stream
            parts.push(Expr::Call(VALUE_PART.into(), values));
        } else {
            unreachable!("unknown format AST statement")
        }
    }
    Ok(Expr::Call(BUILD.into(), parts))
}

#[cfg(test)]
mod format_expression_tests {
    use super::*;

    #[test]
    fn literal_and_args_share_print_parser() {
        let parts = expand_format(&[
            Expr::Str(r#""Item: {} / {:.2}""#.into()),
            Expr::Var("name".into()),
            Expr::Float("19.75".into()),
        ])
        .unwrap();
        let Expr::Call(name, parts) = parts else {
            panic!("expected format build AST");
        };
        assert_eq!(name, BUILD);
        assert_eq!(parts.len(), 4);
        assert!(matches!(&parts[0], Expr::Call(tag, _) if tag == TEXT_PART));
        assert!(matches!(&parts[1], Expr::Call(tag, _) if tag == VALUE_PART));
        assert!(matches!(&parts[3], Expr::Call(tag, _) if tag == VALUE_PART));
    }

    #[test]
    fn format_inherits_placeholder_validation() {
        for (parts, diagnostic) in [
            (
                vec![Expr::Str(r#""{} {}""#.into()), Expr::Int("1".into())],
                "NOVA-FMT002",
            ),
            (vec![Expr::Str(r#""{""#.into())], "NOVA-FMT003"),
            (
                vec![Expr::Str(r#""{:x}""#.into()), Expr::Int("1".into())],
                "NOVA-FMT004",
            ),
        ] {
            assert!(expand_format(&parts).unwrap_err().contains(diagnostic));
        }
    }
}
