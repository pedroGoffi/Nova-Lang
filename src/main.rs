mod ast;
mod call_arguments;
mod codegen;
mod compile_helpers;
mod concepts;
mod concurrency;
mod consteval;
mod coroutine_codegen;
mod coroutines;
mod devtools;
mod diagnostics;
mod editor_overlay;
mod effects;
mod entrypoint;
#[cfg(test)]
mod entrypoint_tests;
mod ergonomic_lowering;
mod hir;
mod intrinsics;
mod iterators;
mod lambdas;
mod llvm_intrinsics;
mod macros;
mod meta;
mod mir;
mod modules;
#[cfg(test)]
mod nb_backend_tests;
#[cfg(test)]
mod nb_tests;
mod out_parameters;
#[cfg(test)]
mod out_parameters_tests;
mod ownership;
mod package;
mod proc_macros;
mod stdlib_audit;
mod surface;
mod type_features;
mod types;
mod validation;
#[cfg(test)]
mod validation_tests;
use lalrpop_util::lalrpop_mod;
use std::{
    env, fs,
    io::IsTerminal,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
lalrpop_mod!(pub grammar);

#[cfg(test)]
mod concepts_integration_tests;
#[cfg(test)]
mod ergonomics_stage18_tests;
#[cfg(test)]
mod ergonomics_stage19_tests;
#[cfg(test)]
mod ergonomics_stage20_tests;
#[cfg(test)]
mod ergonomics_stage21_tests;
mod format_macros;
#[cfg(test)]
mod function_attributes_tests;
#[cfg(test)]
mod grammar_build_regression_tests;
#[cfg(test)]
mod json_stage22_tests;
#[cfg(test)]
mod libc_tests;
#[cfg(test)]
mod module_tests;
#[cfg(test)]
mod parser_regressions;
#[cfg(test)]
mod variadic_tests;

fn usage() -> &'static str {
    r#"Nova 0.25 Alpha: typed standard library in Nova + compiler-reviewed LLVM platform ABI — native compiler (Rust + LALRPOP + HIR/MIR + LLVM)
Usage:
  nova new <name> | init | add std | add <name> --path <directory> | tree [directory]
  nova package build [directory] [--verbose]  Compile a package to .o/.a/.nvi
  nova check [file.nova | package-directory] [--all-errors]
  nova build [file.nova | package-directory] [-o executable] [--emit-llvm] [--all-errors]
  nova run [file.nova | package-directory] [-o executable]
  nova emit-llvm <file.nova> [-o output.ll]
  nova dump-ast|dump-hir|dump-mir <file.nova>
  nova bootstrap-selfhost          Build selfhost/ Nova lexer + syntax parser
  nova parse-selfhost <file.nova>  Validate syntax using Nova-written parser
  nova parse-rust <file.nova>      Syntax-only reference parse (no typecheck)
  nova fmt [file.nova | directory] [--check]
  nova test [package-directory]     Run tests/*.nova and tests/*/Nova.toml
  nova bench [file.nova | package-directory] [runs]
  nova clean [package-directory]
  nova lint [file.nova | package-directory]    Unused locals / mutability checks
  nova lsp-probe                                Check editor overlay protocol support
  nova doctor                                   Audit stdlib/compiler boundaries (no build)
  nova doc [package-directory] [-o path]     Public symbol index
  nova meta [package-directory]              Show .nova metadata contracts
  nova meta inspect|expand [package-directory]
  nova meta verify [package-directory]   Verify CTFE assertions without LLVM
Options:
  -o, --output <path>    Output artifact
  --opt-level <0..3>     Optimization level for Clang (default: 0)
  --target <triple>      Target triple (cross-linker/sysroot are host dependent)
  --debug                Ask Clang to generate debug information
  --runtime-zero         Always on: reject Nova-owned C runtime dependency
  --deny-unapproved-extern  Always on: reject reachable foreign ABI calls outside std::abi::llvm
  --deny-all-extern     Reject ANY reachable foreign ABI symbol except LLVM intrinsics
  --helpers <macro|function>  Use source macros or direct functions for standard helpers
  --module-path <directory>   Add a Go-style module/package search directory
  -l, --library <name>        Link a native shared/system library
  -L <directory>              Add a native library search directory
  --static-library <path>     Link an explicit .a archive
  --rpath <directory>         Add an ELF runtime library search path
  NOVA_PATH                   Colon-separated default module search directories
  --keep-all-functions  Keep unreachable functions in generated LLVM (debug/interop)
  --all-errors           Collect independent surface/HIR errors (max 64 per phase)
  --verbose              Print tool invocations
  -h, --help             Show this help
Source modules: mod foo; use foo::bar; pub fn bar() -> i32 { ... }
Loops: for i in 0..10 { ... }, loop { ... }, break, continue, +=
Types: typedef Integer = i32; trait<T in i8,i16,i32,i64> Integer extends Numeric {...}
Traits: impl Integer for i32 {...}; compile-time contracts and inheritance
Closures: capturing fn(...) -> T {...} become callable structs (immutable by-value captures)
Coroutines: gen fn, coro fn, async fn; yield, await (Linux x86-64 bootstrap)
Variadic packs: fn sum(...xs: i64); sum(1,2,3); sum(...xs); xs.len()\nC variadics: unsafe { std::abi::native::libc::stdio::printf("%d", 5); } (Linux x86-64)
Requires clang 15+ and a 64-bit host for native build. Owning struct/class locals with drop(self) are destroyed on lexical scope exit.
Defer: defer cleanup(); or auto x = make() defer x.close();"#
}

/// Preserve string literals and byte offsets while deleting comments.
pub(crate) fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if let Some(delimiter) = quote {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == delimiter {
                quote = None;
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            out.push(c);
        } else if c == '/' && chars.peek() == Some(&'/') {
            out.push(' ');
            out.push(' ');
            chars.next();
            for next in chars.by_ref() {
                if next == '\n' {
                    out.push('\n');
                    break;
                }
                if next == '\r' {
                    out.push('\r');
                } else {
                    for _ in 0..next.len_utf8() {
                        out.push(' ');
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
#[derive(Default, Clone)]
struct Options {
    output: Option<PathBuf>,
    emit_llvm: bool,
    opt: u8,
    target: Option<String>,
    debug: bool,
    verbose: bool,
    all_errors: bool,
    runtime_zero: bool,
    deny_unapproved_extern: bool,
    deny_all_extern: bool,
    keep_all_functions: bool,
    helper_mode: compile_helpers::HelperMode,
    module_paths: Vec<PathBuf>,
    link_libraries: Vec<String>,
    link_static: Vec<PathBuf>,
    link_search: Vec<PathBuf>,
    link_rpath: Vec<PathBuf>,
    allowed_externs: Vec<String>,
}
fn options(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                o.output = Some(PathBuf::from(
                    args.get(i).ok_or("--output needs a filename")?,
                ));
            }
            "--emit-llvm" => o.emit_llvm = true,
            "--runtime-zero" => o.runtime_zero = true,
            "--deny-unapproved-extern" => o.deny_unapproved_extern = true,
            "--deny-all-extern" => o.deny_all_extern = true,
            "--keep-all-functions" => o.keep_all_functions = true,
            "--helpers" => {
                i += 1;
                o.helper_mode = match args.get(i).map(String::as_str) {
                    Some("macro") => compile_helpers::HelperMode::Macro,
                    Some("function") => compile_helpers::HelperMode::Function,
                    _ => return Err("--helpers must be macro or function".into()),
                };
            }
            "--library" | "-l" => {
                i += 1;
                o.link_libraries
                    .push(args.get(i).ok_or("--library needs a name")?.clone());
            }
            "-L" => {
                i += 1;
                o.link_search
                    .push(PathBuf::from(args.get(i).ok_or("-L needs a directory")?));
            }
            "--static-library" => {
                i += 1;
                o.link_static.push(PathBuf::from(
                    args.get(i).ok_or("--static-library needs a path")?,
                ));
            }
            "--rpath" => {
                i += 1;
                o.link_rpath.push(PathBuf::from(
                    args.get(i).ok_or("--rpath needs a directory")?,
                ));
            }
            "--module-path" | "-I" => {
                i += 1;
                o.module_paths.push(PathBuf::from(
                    args.get(i).ok_or("--module-path needs a directory")?,
                ));
            }
            "--opt-level" => {
                i += 1;
                let v = args.get(i).ok_or("--opt-level needs 0..3")?;
                o.opt = v.parse().map_err(|_| "--opt-level must be 0..3")?;
                if o.opt > 3 {
                    return Err("--opt-level must be 0..3".into());
                }
            }
            "--target" => {
                i += 1;
                o.target = Some(args.get(i).ok_or("--target needs a triple")?.clone());
            }
            "--debug" => o.debug = true,
            "--verbose" => o.verbose = true,
            "--all-errors" => o.all_errors = true,
            other => return Err(format!("unknown option '{other}'")),
        }
        i += 1;
    }
    Ok(o)
}
// Executable entry points use the platform C ABI. The traditional no-argument
// form remains valid; applications that need the original process arguments
// can instead declare `fn main(argc: i32, argv: *const *const u8) -> i32`.
// `argv` is a borrowed C array (NUL-terminated strings); it must not be freed.
fn require_main(hir: &hir::Program) -> Result<(), String> {
    use crate::types::{Primitive, TypeKind};
    const EXPECTED: &str =
        "entry point must be fn main() -> i32 or fn main(argc: i32, argv: *const *const u8) -> i32";
    let f = hir
        .functions
        .iter()
        .find(|f| f.name == "main" && matches!(f.kind, hir::FunctionKind::Defined))
        .ok_or("build/run requires fn main() -> i32 or fn main(argc: i32, argv: *const *const u8) -> i32")?;
    if f.result != hir.types.i32() {
        return Err(EXPECTED.into());
    }
    match f.params.as_slice() {
        [] => Ok(()),
        [argc, argv] => {
            if f.locals[*argc].ty != hir.types.i32() {
                return Err(format!("{EXPECTED}; argc must have type i32"));
            }
            let TypeKind::Pointer { element: inner, .. } = hir.types.kind(f.locals[*argv].ty)
            else {
                return Err(format!(
                    "{EXPECTED}; argv must be a pointer to pointers to u8"
                ));
            };
            let TypeKind::Pointer { element: byte, .. } = hir.types.kind(inner) else {
                return Err(format!(
                    "{EXPECTED}; argv must be a pointer to pointers to u8"
                ));
            };
            if hir.types.kind(byte) != TypeKind::Primitive(Primitive::Unsigned(8)) {
                return Err(format!(
                    "{EXPECTED}; argv must be a pointer to pointers to u8"
                ));
            }
            Ok(())
        }
        _ => Err(EXPECTED.into()),
    }
}

fn llvm_path(output: &Path) -> PathBuf {
    output.with_extension("ll")
}
// A PostgreSQL dependency is optional. Declaration-only imports do not
// activate it; generated calls into the private postgres ABI do.
fn needs_crypto_runtime(ir: &str) -> bool {
    ir.lines()
        .any(|line| line.contains("call ") && line.contains("@__nova_std__old__crypto____"))
}
fn needs_postgres_runtime(ir: &str) -> bool {
    ir.lines()
        .any(|line| line.contains("call ") && line.contains("@__nova_std__postgres____pg_"))
}
// A declaration in embedded STD is not enough to pull a native C module
// into the executable. An actual IR reference (including address-taken FFI
// symbols) is. Keeping this symbol-based avoids guessing from source imports.
fn uses_native_symbols(ir: &str, symbols: &[&str]) -> bool {
    ir.lines().any(|line| {
        let trimmed = line.trim_start();
        !trimmed.starts_with("declare ")
            && !trimmed.starts_with(';')
            && symbols.iter().any(|symbol| trimmed.contains(symbol))
    })
}
fn needs_coroutine_runtime(ir: &str) -> bool {
    uses_native_symbols(
        ir,
        &[
            "@__nova_coro_create",
            "@__nova_std__old__coro____",
            "@__nova_std__old__task____",
            "@__nova_std__old__generator____",
        ],
    )
}
fn needs_net_runtime(ir: &str) -> bool {
    uses_native_symbols(ir, &["@__nova_std__old__net____"])
}
// Kept as a regression-test helper for detecting legacy resource ABI references.
#[cfg(test)]
fn needs_core_runtime(ir: &str) -> bool {
    use std::collections::HashSet;
    fn symbols(line: &str) -> Vec<&str> {
        let mut result = Vec::new();
        for token in line.split('@').skip(1) {
            let end = token
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$')))
                .unwrap_or(token.len());
            if end != 0 {
                result.push(&token[..end]);
            }
        }
        result
    }
    let defined: HashSet<&str> = ir
        .lines()
        .filter(|line| line.trim_start().starts_with("define "))
        .flat_map(symbols)
        .collect();
    ir.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("declare ") && !trimmed.starts_with(';')
        })
        .flat_map(symbols)
        .any(|symbol| {
            ((symbol.starts_with("__nova_std__")
                && !symbol.starts_with("__nova_std__coro____")
                && !symbol.starts_with("__nova_std__task____")
                && !symbol.starts_with("__nova_std__generator____"))
                || symbol.starts_with("__nova_json_"))
                && !defined.contains(symbol)
        })
}

// Native atomics are implemented by a small C adapter using compiler
// atomic builtins. It is linked only when an atomic symbol is reachable.
fn needs_atomic_runtime(ir: &str) -> bool {
    uses_native_symbols(
        ir,
        &[
            "@__nova_atomic_bool_load",
            "@__nova_atomic_bool_store",
            "@__nova_atomic_bool_swap",
            "@__nova_atomic_bool_compare_exchange",
            "@__nova_atomic_bool_is_lock_free",
            "@__nova_atomic_i8_load", "@__nova_atomic_i8_store",
            "@__nova_atomic_i8_swap", "@__nova_atomic_i8_compare_exchange",
            "@__nova_atomic_i8_is_lock_free",
            "@__nova_atomic_i16_load", "@__nova_atomic_i16_store",
            "@__nova_atomic_i16_swap", "@__nova_atomic_i16_compare_exchange",
            "@__nova_atomic_i16_is_lock_free",
            "@__nova_atomic_i32_load", "@__nova_atomic_i32_store",
            "@__nova_atomic_i32_swap", "@__nova_atomic_i32_compare_exchange",
            "@__nova_atomic_i32_is_lock_free",
            "@__nova_atomic_i64_load", "@__nova_atomic_i64_store",
            "@__nova_atomic_i64_swap", "@__nova_atomic_i64_compare_exchange",
            "@__nova_atomic_i64_is_lock_free",
            "@__nova_atomic_u8_load", "@__nova_atomic_u8_store",
            "@__nova_atomic_u8_swap", "@__nova_atomic_u8_compare_exchange",
            "@__nova_atomic_u8_is_lock_free",
            "@__nova_atomic_u16_load", "@__nova_atomic_u16_store",
            "@__nova_atomic_u16_swap", "@__nova_atomic_u16_compare_exchange",
            "@__nova_atomic_u16_is_lock_free",
            "@__nova_atomic_u32_load", "@__nova_atomic_u32_store",
            "@__nova_atomic_u32_swap", "@__nova_atomic_u32_compare_exchange",
            "@__nova_atomic_u32_is_lock_free",
            "@__nova_atomic_u64_load", "@__nova_atomic_u64_store",
            "@__nova_atomic_u64_swap", "@__nova_atomic_u64_compare_exchange",
            "@__nova_atomic_u64_is_lock_free",
            "@__nova_atomic_ptr_load", "@__nova_atomic_ptr_store",
            "@__nova_atomic_ptr_swap", "@__nova_atomic_ptr_compare_exchange",
            "@__nova_atomic_ptr_is_lock_free",
        ],
    )
}

// Strip unreachable *standard-library* function definitions before selecting
// the native platform adapters. This does not remove public user functions,
// non-STD constructors/destructors, generated JSON/coroutine helpers, or any
// function referenced by global data. Foreign function pointers referenced by
// a retained body count as edges. We intentionally keep ALL non-STD roots.
// Keeping extra functions is safe; dropping a reachable function is not.
fn prune_unused_std_ir(ir: &str) -> String {
    use std::collections::{HashMap, HashSet};
    #[derive(Clone)]
    struct Definition {
        name: String,
        start: usize,
        end: usize,
    }
    fn symbols(line: &str) -> Vec<&str> {
        let bytes = line.as_bytes();
        let mut out = Vec::new();
        let mut cursor = 0;
        while cursor < bytes.len() {
            if bytes[cursor] != b'@' {
                cursor += 1;
                continue;
            }
            let start = cursor + 1;
            cursor = start;
            while cursor < bytes.len()
                && (bytes[cursor].is_ascii_alphanumeric()
                    || matches!(bytes[cursor], b'_' | b'.' | b'$' | b'-'))
            {
                cursor += 1;
            }
            if cursor > start {
                out.push(&line[start..cursor]);
            }
        }
        out
    }
    fn std_function(name: &str) -> bool {
        name.starts_with("std__") || name.contains("__std__")
    }
    let lines: Vec<&str> = ir.lines().collect();
    let mut defs = Vec::<Definition>::new();
    let mut definition_by_name = HashMap::<String, usize>::new();
    let mut inside = vec![false; lines.len()];
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim_start();
        if line.starts_with("define ") {
            if let Some(name) = symbols(line).first() {
                let start = index;
                // Codegen emits exactly one standalone closing brace per fn.
                while index + 1 < lines.len() && lines[index].trim() != "}" {
                    index += 1;
                }
                let end = index.min(lines.len() - 1);
                for entry in &mut inside[start..=end] {
                    *entry = true;
                }
                let id = defs.len();
                definition_by_name.insert((*name).to_owned(), id);
                defs.push(Definition {
                    name: (*name).to_owned(),
                    start,
                    end,
                });
            }
        }
        index += 1;
    }
    if defs.is_empty() {
        return ir.to_owned();
    }
    let mut marked = HashSet::<usize>::new();
    let mut work = Vec::<usize>::new();
    for (id, def) in defs.iter().enumerate() {
        if !std_function(&def.name) {
            marked.insert(id);
            work.push(id);
        }
    }
    // A callback/function pointer stored in any non-function global is a root.
    for (line, in_definition) in lines.iter().zip(inside.iter()) {
        if *in_definition || line.trim_start().starts_with("declare ") {
            continue;
        }
        if line.trim_start().starts_with(';') {
            continue;
        }
        for name in symbols(line) {
            if let Some(&id) = definition_by_name.get(name) {
                if marked.insert(id) {
                    work.push(id);
                }
            }
        }
    }
    while let Some(id) = work.pop() {
        let definition = &defs[id];
        for line in &lines[definition.start..=definition.end] {
            for symbol in symbols(line) {
                if let Some(&dependency) = definition_by_name.get(symbol) {
                    if marked.insert(dependency) {
                        work.push(dependency);
                    }
                }
            }
        }
    }
    let mut excluded = vec![false; lines.len()];
    for (id, def) in defs.iter().enumerate() {
        if !marked.contains(&id) {
            for line in &mut excluded[def.start..=def.end] {
                *line = true;
            }
        }
    }
    let mut out = String::with_capacity(ir.len());
    for (line, excluded) in lines.iter().zip(excluded.iter()) {
        if !excluded {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}
// Strictly distinguish source-level foreign ABI calls from compiler-owned LLVM
// platform imports. This is checked on PRUNED IR: unused extern declarations
// in embedded std do not cause failures. All imports used by live code must be
// explicitly approved by std::abi::llvm; adding a new extern will fail closed.
// A strict, link-oriented audit: imported declarations are harmless unless
// actually reached from a surviving function/global after Nova STD pruning.
// LLVM intrinsics are compiler operations, not foreign ABI imports.
fn reachable_foreign_symbols(ir: &str) -> Vec<String> {
    use std::collections::HashSet;
    fn names(line: &str) -> Vec<&str> {
        line.split('@')
            .skip(1)
            .filter_map(|part| {
                let end = part
                    .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$')))
                    .unwrap_or(part.len());
                (end > 0).then_some(&part[..end])
            })
            .collect()
    }
    let declared = ir
        .lines()
        .filter(|line| line.trim_start().starts_with("declare "))
        .flat_map(names)
        .collect::<HashSet<_>>();
    let defined = ir
        .lines()
        .filter(|line| line.trim_start().starts_with("define "))
        .flat_map(names)
        .collect::<HashSet<_>>();
    let mut foreign = ir
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            !line.starts_with("declare ") && !line.starts_with("define ") && !line.starts_with(';')
        })
        .flat_map(names)
        .filter(|symbol| {
            declared.contains(symbol) && !defined.contains(symbol) && !symbol.starts_with("llvm.")
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    foreign.sort();
    foreign.dedup();
    foreign
}
fn unapproved_foreign_symbols(ir: &str, declared_externs: &[String]) -> Vec<String> {
    use std::collections::HashSet;
    let approved = crate::llvm_intrinsics::Intrinsic::ALL
        .iter()
        .flat_map(|intrinsic| intrinsic.declaration().lines())
        .filter_map(|line| line.split('@').nth(1))
        .map(|name| {
            name.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$')))
                .next()
                .unwrap_or("")
        })
        .collect::<HashSet<_>>();
    reachable_foreign_symbols(ir)
        .into_iter()
        .filter(|symbol| {
            !declared_externs.iter().any(|name| name == symbol)
                && !approved.contains(symbol.as_str())
                && !symbol.starts_with("SDL_")
                && !symbol.starts_with("TTF_")
                && !symbol.starts_with("gl")
                && !symbol.starts_with("__nova_coro_")
                && !symbol.starts_with("__nova_std__coro____")
                && !symbol.starts_with("__nova_std__task____")
                && !symbol.starts_with("__nova_std__generator____")
                && symbol != "malloc"
                && symbol != "free"
        })
        .collect()
}

fn build_fingerprint(ir: &str, output: &Path, options: &Options) -> Result<String, String> {
    // FNV-1a is deliberately simple and stable across Nova compiler versions.
    // The compiler version and clang identity are included in the payload.
    let mut payload = String::new();
    payload.push_str(env!("CARGO_PKG_VERSION"));
    payload.push('\n');
    payload.push_str(ir);
    payload.push_str(&format!(
        "\nopt={}\ntarget={:?}\ndebug={}\n",
        options.opt, options.target, options.debug
    ));
    payload.push_str(&format!("deny_all={}\n", options.deny_all_extern));
    for value in &options.link_libraries {
        payload.push_str(&format!("lib={value}\n"));
    }
    for value in &options.link_search {
        payload.push_str(&format!("search={}\n", value.display()));
    }
    for value in &options.link_static {
        payload.push_str(&format!("static={}\n", value.display()));
    }
    for value in &options.link_rpath {
        payload.push_str(&format!("rpath={}\n", value.display()));
    }
    for value in &options.allowed_externs {
        payload.push_str(&format!("extern={value}\n"));
    }
    let clang = Command::new("clang")
        .arg("--version")
        .output()
        .map_err(|e| format!("unable to query clang version: {e}"))?;
    payload.push_str(&String::from_utf8_lossy(&clang.stdout));
    if needs_coroutine_runtime(ir) {
        let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/nova_coroutines.c");
        let bytes = fs::read(&runtime).map_err(|e| format!("{}: {e}", runtime.display()))?;
        payload.push_str(&fingerprint_bytes(&bytes));
    }
    if needs_atomic_runtime(ir) {
        let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/nova_atomics.c");
        let bytes = fs::read(&runtime).map_err(|e| format!("{}: {e}", runtime.display()))?;
        payload.push_str(&fingerprint_bytes(&bytes));
    }
    // Native inputs are part of the cache key: replacing a .a/.so must relink.
    for archive in &options.link_static {
        let bytes = fs::read(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
        payload.push_str(&format!("\n{}:{}", archive.display(), bytes.len()));
        payload.push_str(&fingerprint_bytes(&bytes));
    }
    for directory in &options.link_search {
        if let Ok(entries) = fs::read_dir(directory) {
            let mut paths = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect::<Vec<_>>();
            paths.sort();
            for path in paths.into_iter().filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        options.link_libraries.iter().any(|lib| {
                            name == format!("lib{lib}.so")
                                || name == format!("lib{lib}.a")
                                || name.starts_with(&format!("lib{lib}.so."))
                        })
                    })
            }) {
                if let Ok(bytes) = fs::read(&path) {
                    payload.push_str(&format!("\n{}:{}", path.display(), bytes.len()));
                    payload.push_str(&fingerprint_bytes(&bytes));
                }
            }
        }
    }
    let _ = output;
    Ok(fingerprint_bytes(payload.as_bytes()))
}

fn fingerprint_bytes(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn build(ir: &str, output: &Path, options: &Options) -> Result<(), String> {
    if !cfg!(all(target_os = "linux", target_pointer_width = "64")) {
        return Err("Executable backend currently requires a 64-bit Linux host; Windows target is not linked yet".into());
    }
    if let Some(target) = &options.target {
        if !target.contains("linux") || !(target.contains("x86_64") || target.contains("aarch64")) {
            return Err("The executable linker currently supports Linux x86_64/aarch64 only; Win64 object emission is separate".into());
        }
    }
    let intermediate = llvm_path(output);
    if output == intermediate {
        return Err("executable output path cannot end in .ll".into());
    }
    let optimized_ir = prune_unused_std_ir(ir);
    let ir = optimized_ir.as_str();
    if ir.contains("call ptr @GetStdHandle(")
        || ir.contains("call i32 @WriteFile(")
        || ir.contains("call void @ExitProcess(")
    {
        return Err("Windows ABI cannot be linked into a Linux executable".into());
    }
    // The old JSON/String resource ABI is no longer a runtime-selection
    // switch. Reachable unresolved symbols are diagnosed by the native linker;
    // this allows ordinary extern-backed runtime adapters (e.g. atomics) to
    // coexist with the LLVM/Nova standard library.
    let remaining = [
        ("legacy crypto ABI", needs_crypto_runtime(ir)),
        ("PostgreSQL legacy ABI", needs_postgres_runtime(ir)),
        ("legacy network ABI", needs_net_runtime(ir)),
    ]
    .into_iter()
    .filter_map(|(name, present)| present.then_some(name))
    .collect::<Vec<_>>();
    if !remaining.is_empty() {
        return Err(format!(
            "error[NOVA-RT000]: runtime C was removed; remaining dependencies: {}. Use Nova STD/LLVM implementations or replace this feature before linking.",
            remaining.join(", ")
        ));
    }
    if options.deny_all_extern {
        let foreign = reachable_foreign_symbols(ir);
        if !foreign.is_empty() {
            return Err(format!(
                "error[NOVA-ABI001]: foreign ABI imports forbidden: {}",
                foreign.join(", ")
            ));
        }
    }
    let offenders = unapproved_foreign_symbols(ir, &options.allowed_externs);
    if !offenders.is_empty() {
        return Err(format!(
            "error[NOVA-ABI002]: imports not declared by compiler-reviewed std::abi::llvm intrinsics: {}",
            offenders.join(", ")
        ));
    }
    let cache_path = output.with_extension("nova-cache");
    let cache_key = build_fingerprint(ir, output, options)?;
    if output.is_file()
        && fs::read_to_string(&cache_path).ok().as_deref() == Some(cache_key.as_str())
    {
        if options.verbose {
            eprintln!("Nova build: cache hit for {}", output.display());
        }
        return Ok(());
    }
    fs::write(&intermediate, ir).map_err(|e| format!("{}: {e}", intermediate.display()))?;
    let mut command = Command::new("clang");
    command
        .arg("-x")
        .arg("ir")
        .arg(&intermediate)
        .arg("-x")
        .arg("none")
        .arg(format!("-O{}", options.opt))
        .arg("-ffunction-sections")
        .arg("-fdata-sections")
        .arg("-Wl,--gc-sections")
        .arg("-o")
        .arg(output)
        .arg("-lm");
    if needs_coroutine_runtime(ir) {
        // Coroutine lowering is compiler-owned, but stack switching still
        // needs a small host adapter. Keep it limited to coroutine programs;
        // ordinary Nova executables remain free of a runtime dependency.
        let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/nova_coroutines.c");
        command
            .arg("-x")
            .arg("c")
            .arg(runtime)
            .arg("-x")
            .arg("none");
    }
    if needs_atomic_runtime(ir) {
        let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/nova_atomics.c");
        command
            .arg("-std=c11")
            .arg("-x")
            .arg("c")
            .arg(runtime)
            .arg("-x")
            .arg("none");
    }
    if ir.contains("@pthread_create(") || ir.contains("@pthread_join(") {
        command.arg("-pthread");
    }
    if ir.contains("@SDL_") {
        command.arg("-lSDL2");
    }
    if ir.contains("@TTF_") {
        command.arg("-lSDL2_ttf");
    }
    if ir.contains("@gl") {
        command.arg("-lGL");
    }
    for directory in &options.link_search {
        command.arg("-L").arg(directory);
    }
    for library in &options.link_libraries {
        if library.is_empty() || library.starts_with('-') || library.contains('/') {
            return Err(format!(
                "invalid native library name '{library}'; use [link].static for archive paths"
            ));
        }
        command.arg(format!("-l{library}"));
    }
    for archive in &options.link_static {
        if !archive.is_file() {
            return Err(format!(
                "static library does not exist: {}",
                archive.display()
            ));
        }
        command.arg(archive);
    }
    // Libraries referenced from package archives must appear after those
    // archives on ELF linkers, which resolve static inputs from left to right.
    command.arg("-lm");
    for directory in &options.link_rpath {
        command.arg(format!("-Wl,-rpath,{}", directory.display()));
    }
    if let Some(target) = &options.target {
        command.arg(format!("--target={target}"));
    }
    if options.debug {
        command.arg("-g").arg("-rdynamic");
    }
    if options.verbose {
        eprintln!("{command:?}");
    }
    let status = command
        .status()
        .map_err(|e| format!("unable to launch clang: {e}"))?;
    if !status.success() {
        return Err(format!(
            "clang failed ({status}); LLVM IR preserved at {}",
            intermediate.display()
        ));
    }
    if !options.emit_llvm {
        let _ = fs::remove_file(&intermediate);
    }
    let cache_tmp = cache_path.with_extension("nova-cache.tmp");
    fs::write(&cache_tmp, cache_key).map_err(|e| format!("{}: {e}", cache_tmp.display()))?;
    fs::rename(&cache_tmp, &cache_path).map_err(|e| format!("{}: {e}", cache_path.display()))?;
    Ok(())
}
fn collect_declared_externs(items: &[ast::Item], output: &mut Vec<String>) {
    fn collect(items: &[ast::Item], output: &mut Vec<String>, namespace: &str) {
        for item in items {
            match item {
                ast::Item::Extern(function) => {
                    let symbol = if namespace.is_empty() {
                        function.name.clone()
                    } else {
                        format!("{}__{}", namespace.replace("::", "__"), function.name)
                    };
                    // Keep the leaf symbol for C ABI remapping (e.g. a
                    // namespaced std::libc::printf declaration links as printf)
                    // and the qualified Nova symbol for compiled package ABI.
                    output.push(function.name.clone());
                    if symbol != function.name {
                        output.push(symbol);
                    }
                }
                ast::Item::Public(inner) => collect(std::slice::from_ref(inner), output, namespace),
                ast::Item::Namespace { name, items } => {
                    let nested = if namespace.is_empty() {
                        name.clone()
                    } else {
                        format!("{namespace}::{name}")
                    };
                    collect(items, output, &nested);
                }
                _ => {}
            }
        }
    }
    collect(items, output, "");
}

/// Compile a dependency-free Nova package into a relocatable object and static
/// archive. This is the first independent package artifact path; dependency
/// interface import and stable cross-package ABI are intentionally rejected
/// until their typed representation is available.
fn compile_package_artifacts(input: &Path, opts: &Options) -> Result<PathBuf, String> {
    let plan = package::build_plan(input)?;
    if plan.packages.len() != 1 {
        return Err("`nova package build` currently compiles one dependency-free package at a time; Nova dependency interface consumption is the next ABI milestone".into());
    }
    let (name, version, program, sources) = package::load_standalone_package(input)
        .map_err(|e| diagnostics::attach_package(&e, input, "independent package loading"))?;
    let package_node = plan.packages.first().ok_or("empty package build plan")?;
    let root = if input.is_dir() {
        input.to_path_buf()
    } else {
        input.parent().unwrap_or(Path::new(".")).to_path_buf()
    };
    let artifact_key = format!("{}-{}", name, &package_node.fingerprint[..8]);
    let artifact_dir = root.join("target/debug/packages").join(artifact_key);
    fs::create_dir_all(&artifact_dir).map_err(|e| format!("{}: {e}", artifact_dir.display()))?;

    let lowered = surface::lower_with_helpers(&program, opts.helper_mode)
        .map_err(|e| diagnostics::semantic_with_context(&e, &sources, input, "package surface"))?;
    let hir = hir::lower(&lowered)
        .map_err(|e| diagnostics::semantic_with_context(&e, &sources, input, "package HIR"))?;
    let mir = mir::lower(&hir).map_err(|e| {
        diagnostics::pipeline_error("NOVA-PKG001", &e, &sources, input, "package MIR")
    })?;
    let llvm_ir = codegen::generate_reachable(&mir, true);

    let ir_path = artifact_dir.join("package.ll");
    let object_path = artifact_dir.join("package.o");
    let archive_path = artifact_dir.join(format!("lib{name}.a"));
    fs::write(&ir_path, &llvm_ir).map_err(|e| format!("{}: {e}", ir_path.display()))?;

    let mut clang = Command::new("clang");
    clang
        .arg("-x")
        .arg("ir")
        .arg("-c")
        .arg(&ir_path)
        .arg(format!("-O{}", opts.opt))
        .arg("-fPIC")
        .arg("-o")
        .arg(&object_path);
    if let Some(target) = &opts.target {
        clang.arg(format!("--target={target}"));
    }
    if opts.debug {
        clang.arg("-g");
    }
    if opts.verbose {
        eprintln!("Nova package compiler: {clang:?}");
    }
    let status = clang
        .status()
        .map_err(|e| format!("unable to launch clang for package '{name}': {e}"))?;
    if !status.success() {
        return Err(format!(
            "clang failed compiling package '{name}' ({status}); LLVM IR preserved at {}",
            ir_path.display()
        ));
    }

    let mut archive = Command::new("llvm-ar");
    archive.arg("rcs").arg(&archive_path).arg(&object_path);
    if opts.verbose {
        eprintln!("Nova package archiver: {archive:?}");
    }
    let archive_status = match archive.status() {
        Ok(status) if status.success() => status,
        _ => {
            let mut fallback = Command::new("ar");
            fallback.arg("rcs").arg(&archive_path).arg(&object_path);
            if opts.verbose {
                eprintln!("Nova package archiver fallback: {fallback:?}");
            }
            let status = fallback
                .status()
                .map_err(|e| format!("unable to launch llvm-ar or ar: {e}"))?;
            if !status.success() {
                return Err(format!(
                    "ar failed creating {} ({status})",
                    archive_path.display()
                ));
            }
            status
        }
    };
    let _ = archive_status;

    let interfaces = package::write_interfaces(&root, &program)?;
    let interface_source = interfaces
        .first()
        .ok_or("package interface was not generated")?;
    let interface_output = artifact_dir.join("package.nvi");
    fs::copy(interface_source, &interface_output)
        .map_err(|e| format!("{}: {e}", interface_output.display()))?;

    let metadata = format!(
        "NOVA-PACKAGE-ARTIFACT 1\nname {name}\nversion {version}\nfingerprint {}\nabi experimental-native-v1\nobject package.o\narchive lib{name}.a\ninterface package.nvi\n",
        package_node.fingerprint
    );
    let metadata_path = artifact_dir.join("package.nvp");
    fs::write(&metadata_path, metadata).map_err(|e| format!("{}: {e}", metadata_path.display()))?;
    if opts.verbose {
        eprintln!("Nova package interface: {}", interface_output.display());
        eprintln!("Nova package object: {}", object_path.display());
        eprintln!("Nova package archive: {}", archive_path.display());
    }
    println!(
        "Built Nova package {name} {version}: {}",
        archive_path.display()
    );
    Ok(archive_path)
}

fn compile_executable(input: &Path, output: &Path, opts: &Options) -> Result<(), String> {
    let plan = package::build_plan(input)?;
    let package_metadata = package::write_build_plan(input, &plan)?;
    if opts.verbose {
        for (index, node) in plan.packages.iter().enumerate() {
            eprintln!(
                "Nova package graph: [{}] {} {} ({})",
                index,
                node.name,
                node.version,
                node.root.display()
            );
        }
        for metadata in package_metadata {
            eprintln!("Nova package metadata: {}", metadata.display());
        }
    }
    let (ast, sources) = package::load_detailed(input)
        .map_err(|e| diagnostics::attach_package(&e, input, "module loading"))?;
    let interfaces = package::write_interfaces(input, &ast)?;
    if opts.verbose {
        for interface in interfaces {
            eprintln!("Nova package interface: {}", interface.display());
        }
    }
    let mut build_options = opts.clone();
    let native = package::native_link_config(input)?;
    build_options.link_libraries.extend(native.libraries);
    build_options.link_static.extend(native.static_libraries);
    build_options.link_search.extend(native.search);
    build_options.link_rpath.extend(native.rpath);
    collect_declared_externs(&ast.items, &mut build_options.allowed_externs);
    let ast = surface::lower_with_helpers(&ast, opts.helper_mode)
        .map_err(|e| diagnostics::semantic_with_context(&e, &sources, input, "surface"))?;
    let hir = hir::lower(&ast)
        .map_err(|e| diagnostics::semantic_with_context(&e, &sources, input, "hir"))?;
    let mir = mir::lower(&hir)
        .map_err(|e| diagnostics::pipeline_error("NOVA-IR001", &e, &sources, input, "mir"))?;
    require_main(&hir)?;
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    build(
        &codegen::generate_reachable(&mir, opts.keep_all_functions),
        output,
        &build_options,
    )
}
/// Format each error independently so source attribution and specialization traces
/// never swallow the following diagnostic. The compiler still exits non-zero.
fn render_collected_diagnostics(
    errors: Vec<String>,
    sources: &[modules::SourceFile],
    path: &Path,
    phase: &str,
) -> String {
    let count = errors.len();
    let mut output = format!("error[NOVA-MULTI]: {count} independent error(s) in {phase}\n");
    for (index, error) in errors.iter().enumerate() {
        output.push_str(&format!(
            "\n----- diagnostic {}/{} -----\n",
            index + 1,
            count
        ));
        output.push_str(&diagnostics::semantic_with_context(
            error, sources, path, phase,
        ));
        output.push('\n');
    }
    output
}

fn execute() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || ["-h", "--help"].contains(&args[0].as_str()) {
        println!("{}", usage());
        return Ok(());
    }
    // Explicit handshake for editor clients: semantic lint with an overlay manifest.
    // The actual lint/check commands still run every compiler pass, including
    // ownership/RAII. This does not turn on unsafe build commands for the editor.
    if args[0] == "lsp-probe" {
        if args.len() != 1 {
            return Err("nova lsp-probe takes no arguments".into());
        }
        println!("nova-lsp-overlays-v2");
        return Ok(());
    }
    // NovaLS asks the compiler which real files a package loader reached.
    // This avoids a false "everything OK" for edited examples that sit next to,
    // rather than inside, the package entry/module graph. The LSP provides its
    // overlays through NOVA_LSP_OVERLAY_MAP, so no Nova.lock is written.
    if args[0] == "lsp-sources" {
        if args.len() != 2 {
            return Err("nova lsp-sources <package-directory>".into());
        }
        if env::var_os("NOVA_LSP_OVERLAY_MAP").is_none() {
            return Err(
                "nova lsp-sources is an editor command and requires NOVA_LSP_OVERLAY_MAP".into(),
            );
        }
        let path = PathBuf::from(&args[1]);
        let (_, sources) = package::load_detailed(&path)
            .map_err(|e| diagnostics::attach_package(&e, &path, "module loading"))?;
        for source in sources {
            if !source.path.starts_with('<') {
                println!("NOVA_LSP_SOURCE\t{}", source.path);
            }
        }
        return Ok(());
    }
    // Explicit stage-1 bootstrapping: the Rust compiler still provides LLVM
    // generation; the Nova parser is built as a regular Nova package.
    // Syntax-only reference endpoint for differential tests. No modules,
    // imports, typechecking or linker involved: compare parsers fairly.
    if args[0] == "parse-rust" {
        if args.len() != 2 {
            return Err("nova parse-rust <file.nova>".into());
        }
        let file = PathBuf::from(&args[1]);
        let source = fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        grammar::ProgramParser::new()
            .parse(&strip_comments(&source))
            .map_err(|e| diagnostics::parser(&file.display().to_string(), &source, e))?;
        println!("OK {}", file.display());
        return Ok(());
    }
    if args[0] == "bootstrap-selfhost" || args[0] == "parse-selfhost" {
        let project = std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join("selfhost");
        if !project.join("Nova.toml").exists() {
            return Err("run from the Nova repository root (selfhost/Nova.toml missing)".into());
        }
        let executable = project.join("target/nova-selfhost");
        if args[0] == "bootstrap-selfhost" || !executable.exists() {
            compile_executable(&project, &executable, &Options::default())?;
        }
        if args[0] == "bootstrap-selfhost" {
            println!("Bootstrap parser built at {}", executable.display());
            return Ok(());
        }
        if args.len() != 2 {
            return Err("nova parse-selfhost <file.nova>".into());
        }
        let source = PathBuf::from(&args[1])
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let status = Command::new(&executable)
            .env("NOVA_SOURCE", &source)
            .status()
            .map_err(|e| format!("cannot execute Nova-written parser: {e}"))?;
        if !status.success() {
            return Err(format!(
                "self-host parser rejected {} ({status})",
                source.display()
            ));
        }
        return Ok(());
    }
    if args[0] == "fmt" {
        let path = PathBuf::from(
            args.get(1)
                .filter(|s| !s.starts_with('-'))
                .map(String::as_str)
                .unwrap_or("."),
        );
        let check = args.iter().skip(1).any(|s| s == "--check");
        if args
            .iter()
            .skip(1)
            .any(|s| s.starts_with('-') && s != "--check")
        {
            return Err("nova fmt [path] [--check]".into());
        }
        return devtools::fmt(&path, check);
    }
    if args[0] == "doctor" {
        if args.len() != 1 {
            return Err("nova doctor takes no arguments".into());
        }
        return stdlib_audit::run(Path::new("."));
    }
    if args[0] == "clean" {
        return devtools::clean(&PathBuf::from(
            args.get(1).map(String::as_str).unwrap_or("."),
        ));
    }
    if args[0] == "test" {
        let root = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("."));
        return devtools::local_tests(&root, |file, output| {
            compile_executable(file, output, &Options::default())
        });
    }
    if args[0] == "bench" {
        let path = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("."));
        let runs = match args.get(2) {
            Some(n) => n
                .parse::<usize>()
                .map_err(|_| "bench runs must be a positive integer")?,
            None => 10,
        };
        let output = std::env::temp_dir().join(format!("nova-bench-{}", std::process::id()));
        compile_executable(
            &path,
            &output,
            &Options {
                opt: 2,
                ..Options::default()
            },
        )?;
        let result = devtools::bench(&output, runs);
        let _ = fs::remove_file(&output);
        result?;
        return Ok(());
    }
    if args[0] == "doc" {
        let path = PathBuf::from(
            args.get(1)
                .filter(|p| !p.starts_with('-'))
                .map(String::as_str)
                .unwrap_or("."),
        );
        let docs = devtools::documentation(&package::load_detailed(&path)?.0);
        let output = if let Some(idx) = args.iter().position(|x| x == "-o" || x == "--output") {
            PathBuf::from(args.get(idx + 1).ok_or("nova doc -o needs a filename")?)
        } else {
            path.join("target").join("nova-doc").join("API.md")
        };
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&output, docs).map_err(|e| format!("{}: {e}", output.display()))?;
        println!("Generated {}", output.display());
        return Ok(());
    }
    if args[0] == "meta" {
        let inspecting = args.get(1).is_some_and(|arg| arg == "inspect");
        let expanding = args.get(1).is_some_and(|arg| arg == "expand");
        let verifying = args.get(1).is_some_and(|arg| arg == "verify");
        let path = PathBuf::from(
            args.get(if inspecting || expanding || verifying {
                2
            } else {
                1
            })
            .map(String::as_str)
            .unwrap_or("."),
        );
        let (ast, _) = package::load_detailed(&path)?;
        if verifying {
            let ast = proc_macros::expand(&ast)?;
            let ast = macros::expand(&ast)?;
            let ast = meta::expand(&ast)?;
            meta::validate(&ast)?;
            effects::validate(&ast)?;
            let _ = consteval::fold(&ast)?;
            println!("Nova Meta CTFE: compile-time assertions satisfied");
        } else if inspecting {
            print!("{}", meta::inspect(&ast)?);
        } else if expanding {
            let ast = proc_macros::expand(&ast)?;
            print!("{}", meta::expansion_report(&ast)?);
        } else {
            print!("{}", meta::describe(&ast)?);
        }
        return Ok(());
    }
    if args[0] == "lint" {
        let path = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("."));
        let (ast, sources) = package::load_detailed(&path)
            .map_err(|e| diagnostics::attach_package(&e, &path, "module loading"))?;
        let metadata_warnings = meta::lint(&ast);
        for warning in &metadata_warnings {
            println!("{warning}");
        }
        let ast = surface::lower(&ast)
            .map_err(|e| diagnostics::semantic_with_context(&e, &sources, &path, "surface"))?;
        let hir = hir::lower(&ast)
            .map_err(|e| diagnostics::semantic_with_context(&e, &sources, &path, "hir"))?;
        let _ = mir::lower(&hir)
            .map_err(|e| diagnostics::pipeline_error("NOVA-IR001", &e, &sources, &path, "mir"))?;
        println!(
            "Nova lint: {} warning(s)",
            devtools::lint(&hir) + metadata_warnings.len()
        );
        return Ok(());
    }
    if args[0] == "package" {
        if args.get(1).map(String::as_str) != Some("build") {
            return Err("usage: nova package build [directory] [--verbose]".into());
        }
        let path = PathBuf::from(
            args.get(2)
                .filter(|value| !value.starts_with('-'))
                .map(String::as_str)
                .unwrap_or("."),
        );
        let option_start = if args.get(2).is_some_and(|value| !value.starts_with('-')) {
            3
        } else {
            2
        };
        let opts = options(&args[option_start..])?;
        compile_package_artifacts(&path, &opts)?;
        return Ok(());
    }
    if args[0] == "new" {
        return package::new(args.get(1).ok_or("nova new <package-name>")?);
    }
    if args[0] == "init" {
        return package::init();
    }
    if args[0] == "tree" {
        return package::tree(&PathBuf::from(
            args.get(1).map(String::as_str).unwrap_or("."),
        ));
    }
    if args[0] == "add" {
        if args.get(1).map(String::as_str) == Some("std") && args.len() == 2 {
            return package::add_builtin_std();
        }
        if args.get(2).map(String::as_str) != Some("--path") {
            return Err("nova add std | nova add <name> --path <directory>".into());
        }
        return package::add(
            args.get(1).ok_or("dependency name required")?,
            args.get(3).ok_or("path required")?,
        );
    }
    if args.len() < 2
        && ![
            "check",
            "build",
            "run",
            "emit-llvm",
            "dump-ast",
            "dump-hir",
            "dump-mir",
        ]
        .contains(&args[0].as_str())
    {
        return Err(usage().into());
    }
    let command = args[0].as_str();
    if ![
        "check",
        "build",
        "run",
        "emit-llvm",
        "dump-ast",
        "dump-hir",
        "dump-mir",
    ]
    .contains(&command)
    {
        return Err(usage().into());
    }
    let has_path = args.get(1).map(|x| !x.starts_with('-')).unwrap_or(false);
    let path = if has_path {
        PathBuf::from(&args[1])
    } else {
        PathBuf::from(".")
    };
    let option_start = if has_path { 2 } else { 1 };
    let mut opts = options(&args[option_start..])?;
    let native = package::native_link_config(&path)?;
    opts.link_libraries.extend(native.libraries);
    opts.link_static.extend(native.static_libraries);
    opts.link_search.extend(native.search);
    opts.link_rpath.extend(native.rpath);
    let (ast, sources) = package::load_detailed_with_paths(&path, &opts.module_paths)
        .map_err(|e| diagnostics::attach_package(&e, &path, "module loading"))?;
    if command == "build" {
        let interfaces = package::write_interfaces(&path, &ast)?;
        if opts.verbose {
            for interface in interfaces {
                eprintln!("Nova package interface: {}", interface.display());
            }
        }
    }
    collect_declared_externs(&ast.items, &mut opts.allowed_externs);
    if command == "dump-ast" {
        println!("{ast:#?}");
        return Ok(());
    }
    let ast = if opts.all_errors {
        surface::lower_with_helpers_all_errors(&ast, opts.helper_mode)
            .map_err(|errors| render_collected_diagnostics(errors, &sources, &path, "surface"))?
    } else {
        surface::lower_with_helpers(&ast, opts.helper_mode)
            .map_err(|e| diagnostics::semantic_with_context(&e, &sources, &path, "surface"))?
    };
    let hir = if opts.all_errors {
        hir::lower_all_errors(&ast)
            .map_err(|errors| render_collected_diagnostics(errors, &sources, &path, "hir"))?
    } else {
        hir::lower(&ast)
            .map_err(|e| diagnostics::semantic_with_context(&e, &sources, &path, "hir"))?
    };
    if command == "dump-hir" {
        println!("{hir:#?}");
        return Ok(());
    }
    let mir = mir::lower(&hir)
        .map_err(|e| diagnostics::pipeline_error("NOVA-IR001", &e, &sources, &path, "mir"))?;
    if command == "dump-mir" {
        println!("{mir:#?}");
        return Ok(());
    }
    if command == "check" {
        println!(
            "OK: {} ({} functions, {} structs)",
            path.display(),
            hir.functions.len(),
            hir.types.structs.len()
        );
        return Ok(());
    }
    if command != "emit-llvm" {
        require_main(&hir)?;
    }
    let ir = codegen::generate_reachable(&mir, opts.keep_all_functions);
    if command == "emit-llvm" {
        if let Some(out) = &opts.output {
            fs::write(out, &ir).map_err(|e| format!("{}: {e}", out.display()))?;
            println!("Wrote {}", out.display());
        } else {
            print!("{ir}");
        }
        return Ok(());
    }
    let output = opts.output.clone().unwrap_or_else(|| {
        if path.is_dir() {
            path.join("target").join("nova-app")
        } else {
            path.with_extension("")
        }
    });
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    build(&ir, &output, &opts)?;
    println!("Built {}", output.display());
    if command == "run" {
        let bin = output
            .canonicalize()
            .map_err(|e| format!("{}: {e}", output.display()))?;
        let mut process = Command::new(&bin);
        // Keep the process working directory unchanged: other Nova packages
        // expect paths to be resolved from their caller's directory. Pass the
        // selected package's properties file explicitly instead, without
        // overriding a user-provided NB_PROPERTIES environment variable.
        if std::env::var_os("NB_PROPERTIES").is_none() {
            let project_root = if path.is_dir() {
                path.as_path()
            } else {
                path.parent().unwrap_or_else(|| std::path::Path::new("."))
            };
            let properties = project_root.join("application.properties");
            if properties.is_file() {
                let absolute = properties
                    .canonicalize()
                    .map_err(|e| format!("{}: {e}", properties.display()))?;
                process.env("NB_PROPERTIES", absolute);
            }
        }
        let status = process
            .status()
            .map_err(|e| format!("failed to execute {}: {e}", bin.display()))?;
        println!("Program exit code: {:?}", status.code());
        if !status.success() {
            return Err(format!("program exited with {status}"));
        }
    }
    Ok(())
}
fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if error.starts_with("error[") {
                let color = match env::var("NOVA_COLOR").as_deref() {
                    Ok("always") => true,
                    Ok("never") => false,
                    _ => std::io::stderr().is_terminal(),
                };
                if color {
                    eprintln!("{}", diagnostics::colorize_for_terminal(&error));
                } else {
                    eprintln!("{error}");
                }
            } else {
                eprintln!("nova: {error}");
            }
            ExitCode::FAILURE
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn compile(source: &str) -> Result<String, String> {
        let parsed = grammar::ProgramParser::new()
            .parse(&strip_comments(source))
            .map_err(|e| format!("syntax: {e}"))?;
        let surface = surface::lower(&parsed)?;
        let hir = hir::lower(&surface)?;
        let mir = mir::lower(&hir)?;
        Ok(codegen::generate(&mir))
    }
    #[test]
    fn all_errors_option_is_opt_in() {
        let defaults = options(&[]).unwrap();
        assert!(!defaults.all_errors);
        assert!(options(&["--all-errors".into()]).unwrap().all_errors);
    }

    #[test]
    fn all_errors_collects_independent_surface_specialization_errors() {
        let code = "fn first() -> i32 { return missing_first(); } fn second() -> i32 { return missing_second(); }";
        let ast = grammar::ProgramParser::new()
            .parse(&strip_comments(code))
            .unwrap();
        let errors =
            surface::lower_with_helpers_all_errors(&ast, compile_helpers::HelperMode::Macro)
                .unwrap_err();
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(
            errors.iter().any(|error| error.contains("missing_first")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|error| error.contains("missing_second")),
            "{errors:?}"
        );
    }

    #[test]
    fn all_errors_accumulates_independent_hir_function_bodies() {
        let code = "fn first() -> i32 { return true; } fn second() -> i32 { return \"wrong\"; }";
        let ast = grammar::ProgramParser::new()
            .parse(&strip_comments(code))
            .unwrap();
        let errors = hir::lower_all_errors(&ast).unwrap_err();
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(
            errors.iter().any(|error| error.contains("first")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|error| error.contains("second")),
            "{errors:?}"
        );
        assert!(hir::lower(&ast).is_err(), "default still fails fast");
    }

    #[test]
    fn compiler_helper_mode_flag_parses_and_checks_values() {
        let args = vec![
            "--helpers".into(),
            "function".into(),
            "--runtime-zero".into(),
        ];
        let opts = options(&args).unwrap();
        assert_eq!(opts.helper_mode, compile_helpers::HelperMode::Function);
        assert!(opts.runtime_zero);
        assert!(options(&["--helpers".into(), "invalid".into()]).is_err());
    }
    #[test]
    fn global_const_compiles_without_global_runtime_storage() {
        let ll = compile(
            r#"
            comptime const AT_FDCWD: i32 = -100;
            const O_WRITE: i32 = 1 | 64 | 512;
            fn main()->i32 {return AT_FDCWD + O_WRITE - 477;}
        "#,
        )
        .unwrap();
        assert!(ll.contains("define i32 @main"), "{ll}");
        assert!(!ll.contains("AT_FDCWD"), "{ll}");
        assert!(!ll.contains("O_WRITE"), "{ll}");
    }
    #[test]
    fn namespaces_and_typedef_compile_time_constants_are_lowered() {
        let ll = compile(
            r#"
            typedef LinuxFd = i32;
            namespace posix {
                pub comptime const AT_FDCWD: LinuxFd = -100;
            }
            fn main()->i32 {return posix::AT_FDCWD + 100;}
        "#,
        )
        .unwrap();
        assert!(ll.contains("define i32 @main"), "{ll}");
        assert!(!ll.contains("AT_FDCWD"), "{ll}");
    }
    #[test]
    fn entry_accepts_legacy_main_and_c_argv() {
        for source in [
            "fn main() -> i32 { return 0; }",
            "fn main(argc: i32, argv: *const *const u8) -> i32 { return argc - 1; }",
            "fn main(argc: i32, argv: *mut *mut u8) -> i32 { return argc - 1; }",
        ] {
            let ast = grammar::ProgramParser::new()
                .parse(&strip_comments(source))
                .unwrap();
            let surface = surface::lower(&ast).unwrap();
            let high = hir::lower(&surface).unwrap();
            require_main(&high).unwrap();
            let low = mir::lower(&high).unwrap();
            let llvm = codegen::generate(&low);
            if source.contains("argc:") {
                assert!(
                    llvm.contains("define i32 @main(i32 %arg0, ptr %arg1)"),
                    "{llvm}"
                );
            } else {
                assert!(llvm.contains("define i32 @main()"), "{llvm}");
            }
        }
    }

    #[test]
    fn entry_rejects_wrong_abi_and_wrong_arity() {
        for source in [
            "fn main(argc: i32) -> i32 { return argc; }",
            "fn main(argc: i64, argv: *const *const u8) -> i32 { return 0; }",
            "fn main(argc: i32, argv: *const u8) -> i32 { return 0; }",
            "fn main(argc: i32, argv: *const *const i8) -> i32 { return 0; }",
            "fn main(argc: i32, argv: *const *const u8) -> i64 { return 0; }",
            "fn main(x: i32, y: i32, z: i32) -> i32 { return 0; }",
        ] {
            let ast = grammar::ProgramParser::new()
                .parse(&strip_comments(source))
                .unwrap();
            let surface = surface::lower(&ast).unwrap();
            let high = hir::lower(&surface).unwrap();
            let err = require_main(&high).unwrap_err();
            assert!(err.contains("entry point must be"), "{source}: {err}");
        }
    }

    #[test]
    fn rejects_string_written_to_bool_field_in_method() {
        let source = r#"struct Person { id: i64, active: bool }
impl Person {
    fn test(self: Person) -> () { self.active = "teste"; return; }
}
fn main() -> i32 { return 0; }
"#;
        // Immutable receivers may fail earlier than typechecking the RHS.
        let error = compile(source).unwrap_err();
        assert!(
            error.contains("immutable") || error.contains("expected bool, found str"),
            "{error}"
        );
    }
    #[test]
    fn rejects_str_to_bool_even_when_field_is_mutable() {
        let source = r#"struct Person { id: i64, active: bool }
fn main() -> i32 {
    let mut person = new Person { id: 1, active: true };
    person.active = "teste";
    return 0;
}
"#;
        let error = compile(source).unwrap_err();
        assert!(error.contains("expected bool, found str"), "{error}");
    }
    #[test]
    fn character_literals_have_distinct_unicode_type_and_llvm_abi() {
        let llvm = compile(
            r#"
            fn main() -> i32 {
                let letter: char = 'A';
                let accent: char = 'ç';
                let rocket: char = '\u{1F680}';
                let escaped: char = '\n';
                let slash: i32 = '/';
                if letter != 'A' || !(rocket > accent) { return 1; }
                if escaped as u32 != 10 { return 2; }
                if slash != 47 || '/' != slash { return 3; }
                if rocket as u32 != 128640 { return 4; }
                return 0;
            }
        "#,
        )
        .unwrap();
        assert!(llvm.contains("define i32 @main"));
        assert!(llvm.contains("i32 128640") || llvm.contains("128640"));
    }
    #[test]
    fn character_literals_cannot_be_used_for_implicit_arithmetic() {
        let error = compile("fn main()->i32 { let letter:char='A'; let n=letter+1; return 0; }")
            .unwrap_err();
        assert!(
            error.contains("equal types")
                || error.contains("numeric")
                || error.contains("expected char, found i32"),
            "{error}"
        );
    }
    #[test]
    fn character_literal_out_of_range_is_reported() {
        let error =
            compile("fn main()->i32 { let tiny:u8='🚀'; return tiny as i32; }").unwrap_err();
        assert!(error.contains("NOVA-CHAR003"), "{error}");
    }

    #[test]
    fn final_if_with_explicit_returns_stays_control_flow() {
        // Regression: a final exhaustive if must not be force-converted into
        // an expression if its branches return instead of yielding values.
        for source in [
            "fn main()->i32 { if true { return 0; } else { return 1; } }",
            "fn main()->i32 { let x:i32=2; if x == 1 { return 1; } else if x == 2 { return 0; } else { return 3; } }",
        ] {
            let llvm = compile(source).unwrap();
            assert!(llvm.contains("define i32 @main"), "{source}");
        }
    }

    #[test]
    fn final_if_with_values_still_implicitly_returns() {
        for source in [
            "fn main()->i32 { if true { 0 } else { 1 } }",
            "fn main()->i32 { let x:i32=2; if x == 1 { 1 } else if x == 2 { 0 } else { 3 } }",
        ] {
            let llvm = compile(source).unwrap();
            assert!(llvm.contains("define i32 @main"), "{source}");
        }
    }

    #[test]
    fn else_if_compiles_through_mir() {
        let src="fn main() -> i32 { let x: i32 = 2; if x == 0 { return 1; } else if x == 1 { return 2; } else if x == 2 { return 0; } else { return 3; } }";
        assert!(compile(src).unwrap().contains("define i32 @main"));
    }
    #[test]
    fn result_with_two_generic_parameters_parses() {
        let src="enum Result<T,E> { Ok(T), Err(E) } fn main() -> i32 { let x: Result<i32,i64> = Ok(0); match x { Ok(value) => { return value; } Err(problem) => { return 1; } } }";
        // Parsing the type is not evidence of successful generic instantiation;
        // codegen equivalence will be exercised by the bootstrap integration suite.
        assert!(grammar::ProgramParser::new().parse(src).is_ok());
    }
    #[test]
    fn range_bounds_can_use_explicit_i64_variables() {
        let src="fn main() -> i32 { let low: i64 = 0; let high: i64 = 4; for i in low..high { if i == 3 { return 0; } } return 1; }";
        assert!(compile(src).is_ok());
    }
    #[test]
    fn i64_vector_style_range_bounds_lower_to_llvm() {
        let src="fn sum(limit: i64) -> i64 { let start: i64 = 0; let mut total: i64 = 0; for i in start..limit { total += i; } return total; } fn main() -> i32 { return 0; }";
        let ir = compile(src).unwrap();
        assert!(
            ir.contains("define i64 @sum"),
            "i64 range did not reach LLVM"
        );
    }
    #[test]
    fn different_range_bound_types_explain_both_types_and_function() {
        let src="fn main() -> i32 { let low: i32 = 0; let high: i64 = 4; for i in low..high { } return 0; }";
        let parsed = grammar::ProgramParser::new().parse(src).unwrap();
        let err = surface::lower(&parsed).unwrap_err();
        assert!(err.contains("bounds must have equal types"), "{err}");
        assert!(err.contains("i32") && err.contains("i64"), "{err}");
        assert!(err.contains("specializing function 'main'"), "{err}");
    }
    #[test]
    fn range_for_and_compound_assignment() {
        let source =
            "fn main() -> i32 { let mut sum: i32 = 0; for i in 0..5 { sum += i; } return sum-10; }";
        let ir = compile(source).unwrap();
        assert!(ir.contains("define i32 @main"));
        assert!(ir.contains("br i1"));
    }
    #[test]
    fn loop_break_continue_and_nested_control_flow() {
        let src="fn main() -> i32 { let mut i: i32 = 0; loop { i += 1; if i < 3 { continue; } break; } return i-3; }";
        assert!(compile(src).unwrap().contains("define i32 @main"));
    }
    #[test]
    fn rejects_break_and_continue_outside_loop() {
        assert!(compile("fn main() -> i32 { break; return 0; }")
            .unwrap_err()
            .contains("break is only allowed"));
        assert!(compile("fn main() -> i32 { continue; return 0; }")
            .unwrap_err()
            .contains("continue is only allowed"));
    }
    #[test]
    fn rejects_compound_assignment_to_immutable() {
        assert!(
            compile("fn main() -> i32 { let a:i32=1; a += 2; return a; }")
                .unwrap_err()
                .contains("immutable")
        );
    }
    #[test]
    fn rejects_boolean_range() {
        assert!(
            compile("fn main() -> i32 { for i in true..false { } return 0; }")
                .unwrap_err()
                .contains("integer")
        );
    }
    #[test]
    fn arithmetic_and_cfg() {
        let ir=compile("fn main() -> i32 { let mut x: i32 = 0; while x < 4 { x = x + 1; } if x == 4 && true { return x; } else { return 1; } }").unwrap();
        assert!(ir.contains("define i32 @main"));
        assert!(ir.contains("br i1"));
    }
    #[test]
    fn rejects_immutable_assignment() {
        let err = compile("fn main() -> i32 { let x: i32 = 1; x = 3; return x; }").unwrap_err();
        assert!(err.contains("immutable"));
    }
    #[test]
    fn types_and_inference() {
        let ir = compile("fn main() -> i32 { let x: u8 = 12; let n = 3; return n; }").unwrap();
        assert!(ir.contains("alloca i8"));
    }
    #[test]
    fn struct_with_generated_json_serializer() {
        let src = r#"#[derive(Serialize)] struct Person { name: str, age: i32, active: bool }
        fn main() -> i32 { let p = new Person { name: "Jo\\\"se", age: 20, active: true }; Person_write_json(p); return 0; }"#;
        let ir = compile(src).unwrap();
        assert!(ir.contains("@Person_write_json"));
        assert!(ir.contains("@__nova_json_str"));
    }
    #[test]
    fn deserialize_struct_codegen_and_roundtrip_api() {
        let ir = compile(
            r#"
            #[derive(Serialize, Deserialize)]
            struct User { id: u64, name: str, active: bool }
            fn main() -> i32 {
                let user = User_from_json("{\"id\":42,\"name\":\"Nova\",\"active\":true}");
                if user.id != 42 as u64 { return 1; }
                User_write_json(user);
                return 0;
            }
        "#,
        )
        .unwrap();
        assert!(ir.contains("define %struct.User @User_from_json(ptr %text)"));
        assert!(ir.contains("define private %struct.User @__nova_json_decode_User"));
        assert!(ir.contains("call ptr @__nova_json_parse(ptr %text)"));
        assert!(ir.contains("call void @__nova_json_free(ptr %root)"));
        assert!(ir.contains("call i64 @__nova_json_unsigned"));
        assert!(ir.contains("call ptr @__nova_json_string"));
        assert!(ir.contains("call i1 @__nova_json_boolean"));
        assert!(ir.contains("define i32 @User_write_json"));
    }
    #[test]
    fn deserialize_nested_structs_codegen() {
        let ir=compile(r#"
            #[derive(Deserialize)] struct Address { zip: i32, active: bool }
            #[derive(Deserialize)] struct Customer { address: Address, name: str }
            fn main() -> i32 {
                let c = Customer_from_json("{\"address\":{\"zip\":123,\"active\":true},\"name\":\"Ana\"}");
                return c.address.zip - 123;
            }
        "#).unwrap();
        assert!(ir.contains("call %struct.Address @__nova_json_decode_Address(ptr %field0)"));
        assert!(ir.contains("define private %struct.Address @__nova_json_decode_Address"));
        assert!(ir.contains("call i64 @__nova_json_signed"));
    }
    #[test]
    fn deserialize_generated_helpers_support_generic_struct_specialization() {
        let ir = compile(
            r#"
            #[derive(Deserialize)] struct Wrapper<T> { value: T }
            fn main() -> i32 {
                let item: Wrapper<i32> = Wrapper__i32_from_json("{\"value\":17}");
                return item.value - 17;
            }
        "#,
        )
        .unwrap();
        assert!(ir.contains("@Wrapper__i32_from_json"));
    }
    #[test]
    fn deserialize_rejects_unsupported_pointer_fields() {
        let err = compile(
            "#[derive(Deserialize)] struct P { raw: *mut i32 } fn main() -> i32 { return 0; }",
        )
        .unwrap_err();
        assert!(err.contains("cannot be deserialized"), "{err}");
    }
    #[test]
    fn deserialize_nested_types_must_opt_in() {
        let err=compile("struct Inner { id: i32 } #[derive(Deserialize)] struct Outer { value: Inner } fn main() -> i32 { return 0; }").unwrap_err();
        assert!(err.contains("nested structs also need"), "{err}");
    }
    #[test]
    fn deserialize_rejects_generated_name_collision() {
        let err=compile("#[derive(Deserialize)] struct User { id: i32 } fn User_from_json(s: str) -> User { return new User { id: 0 }; } fn main() -> i32 { return 0; }").unwrap_err();
        assert!(err.contains("conflicts"), "{err}");
    }
    #[test]
    fn generic_functions_specialize_at_call_sites() {
        let ir = compile("fn id<T>(x:T) -> T { return x; } fn main() -> i32 { return id(7) - 7; }")
            .unwrap();
        assert!(ir.contains("@id__i32"));
    }
    #[test]
    fn generic_struct_and_method() {
        let ir=compile(r#"struct Box<T> { value: T }
            impl<T> Box<T> { fn get(self: Box<T>) -> T { return self.value; } }
            fn main() -> i32 { let box: Box<i32> = new Box<i32> {value: 8}; return box.get() - 8; }"#).unwrap();
        assert!(ir.contains("%struct.Box__i32"));
        assert!(ir.contains("@Box__get__i32"));
    }
    #[test]
    fn namespaces_use_mangled_names() {
        let ir=compile("namespace math { fn sum(x:i32, y:i32) -> i32 { return x + y; } } fn main() -> i32 { return math::sum(1,2) - 3; }").unwrap();
        assert!(ir.contains("@math__sum"));
    }
    #[test]
    fn option_enum_and_exhaustive_match() {
        let ir=compile(r#"enum Option<T> { Some(T), None }
        fn main() -> i32 { let v: Option<i32> = Some(9); match v { Some(x) => { return x-9; } None => { return 1; } } }"#).unwrap();
        assert!(ir.contains("%struct.Option__i32"));
    }
    #[test]
    fn non_exhaustive_enum_match_is_rejected() {
        let error = compile(
            r#"enum Option<T> { Some(T), None }
        fn main() -> i32 { let v: Option<i32> = None; match v { Some(x) => { return 0; } } }"#,
        )
        .unwrap_err();
        assert!(error.contains("non-exhaustive"));
    }
    #[test]
    fn generic_function_arity_checked() {
        let error = compile(
            "fn id<T>(x: T) -> T { return x; } fn main() -> i32 { return id::<i32,i64>(7); }",
        )
        .unwrap_err();
        assert!(error.contains("generic") || error.contains("number"));
    }
    #[test]
    fn derives_generate_concrete_helpers() {
        let ir = compile(
            r#"#[derive(Serialize, Debug, Default, Clone)] struct P { x:i32 }
        fn main() -> i32 { let p: P = P_default(); let q: P = P_clone(p); P_debug(q); return 0; }"#,
        )
        .unwrap();
        for name in ["P_default", "P_clone", "P_debug", "P_write_json"] {
            assert!(ir.contains(name));
        }
    }
    #[test]
    fn invalid_derive_is_rejected() {
        let err = compile("#[derive(Nope)] struct S { a: i32 } fn main() -> i32 { return 0; }")
            .unwrap_err();
        assert!(err.contains("unknown derive"));
    }
    #[test]
    fn array_bounds_emits_trap() {
        let ir=compile("fn main() -> i32 { let values: [i32; 3] = [4, 5, 6]; let idx: i32 = 1; return values[idx]; }").unwrap();
        assert!(ir.contains("@llvm.trap"));
    }
    #[test]
    fn pointers_require_unsafe() {
        let err = compile("fn main() -> i32 { let a: i32 = 1; let p: *const i32 = &a; return 0; }")
            .unwrap_err();
        assert!(err.contains("unsafe"));
    }
    #[test]
    fn pointer_access_inside_unsafe() {
        let src =
            "fn main() -> i32 { let a: i32 = 8; unsafe { let p: *const i32 = &a; return *p; } }";
        assert!(compile(src).unwrap().contains("load i32, ptr"));
    }
    #[test]
    fn owner_move_keeps_single_drop_path() {
        let ir = compile(
            r#"
            struct Resource { token: i32 }
            impl Resource { fn drop(self: Resource) -> () { return; } }
            fn main() -> i32 {
                let original = new Resource { token: 10 };
                let transferred = original;
                return transferred.token - 10;
            }
        "#,
        )
        .unwrap();
        assert!(ir.contains("__drop"));
        assert!(ir.contains("br i1"));
    }

    #[test]
    fn owner_use_after_move_is_error() {
        let err = compile(
            r#"
            class Resource { token: i32 }
            impl Resource { fn drop(self: Resource) -> () { return; } }
            fn main() -> i32 {
                let resource = new Resource { token: 3 };
                let transferred = resource;
                return resource.token;
            }
        "#,
        )
        .unwrap_err();
        assert!(err.contains("moved value"), "{err}");
    }

    #[test]
    fn owned_assignment_without_move_is_rejected() {
        let err = compile(
            r#"
            class Resource { token: i32 }
            impl Resource { fn drop(self: Resource) -> () { return; } }
            fn main() -> i32 {
                let mut current = new Resource { token: 1 };
                current = new Resource { token: 2 };
                return 0;
            }
        "#,
        )
        .unwrap_err();
        assert!(err.contains("overwrite"), "{err}");
    }

    #[test]
    fn shared_borrow_can_be_dereferenced_without_unsafe() {
        assert!(compile(
            r#"
            fn main() -> i32 {
                let number: i32 = 41;
                let reference: &i32 = &number;
                return *reference + 1 - 42;
            }
        "#
        )
        .is_ok());
    }

    #[test]
    fn mutable_borrow_supports_safe_deref_assignment() {
        let ir = compile(
            r#"
            fn main() -> i32 {
                let mut number: i32 = 1;
                let exclusive: &mut i32 = &mut number;
                *exclusive = 42;
                return *exclusive - 42;
            }
        "#,
        )
        .unwrap();
        assert!(ir.contains("store i32 42"));
    }

    #[test]
    fn borrow_array_struct_element_is_a_real_place() {
        let ir = compile(
            r#"
            struct Item { id: i32, points: i32 }
            fn main() -> i32 {
                let items: [Item; 2] = [
                    new Item { id: 7, points: 10 },
                    new Item { id: 9, points: 20 }
                ];
                let index: i32 = 1;
                let item: &Item = &items[index];
                return (*item).id - 9;
            }
        "#,
        )
        .unwrap();
        assert!(ir.contains("borrow.index.ok"), "{ir}");
        assert!(
            ir.contains("getelementptr inbounds [2 x %struct.Item], ptr %slot"),
            "{ir}"
        );
        assert!(ir.contains("@llvm.trap"), "{ir}");
    }

    #[test]
    fn borrow_nested_field_from_array() {
        let ir = compile(
            r#"
            struct Item { id: i32, points: i32 }
            fn main() -> i32 {
                let items: [Item; 2] = [
                    new Item { id: 7, points: 10 },
                    new Item { id: 9, points: 20 }
                ];
                let points: &i32 = &items[1].points;
                return *points - 20;
            }
        "#,
        )
        .unwrap();
        assert!(
            ir.contains("getelementptr inbounds [2 x %struct.Item]"),
            "{ir}"
        );
        assert!(ir.contains("getelementptr inbounds %struct.Item"), "{ir}");
    }

    #[test]
    fn borrow_direct_struct_field_without_copying_owner() {
        let ir = compile(
            r#"
            struct Item { id: i32, points: i32 }
            fn main() -> i32 {
                let item = new Item { id: 7, points: 11 };
                let points: &i32 = &item.points;
                return *points - 11;
            }
        "#,
        )
        .unwrap();
        assert!(ir.contains("getelementptr inbounds %struct.Item"), "{ir}");
    }

    #[test]
    fn exclusive_borrow_of_array_element_supports_mutation() {
        let ir = compile(
            r#"
            struct Item { id: i32 }
            fn main() -> i32 {
                let mut items: [Item; 2] = [
                    new Item { id: 7 }, new Item { id: 9 }
                ];
                let id: &mut i32 = &mut items[0].id;
                *id = 21;
                return *id - 21;
            }
        "#,
        )
        .unwrap();
        assert!(
            ir.contains("getelementptr inbounds [2 x %struct.Item]"),
            "{ir}"
        );
    }

    #[test]
    fn array_borrow_prevents_conflicting_mutable_borrow() {
        let err = compile(
            r#"
            fn main() -> i32 {
                let mut values: [i32; 2] = [1, 2];
                let first: &i32 = &values[0];
                let second: &mut i32 = &mut values[1];
                return *first + *second;
            }
        "#,
        )
        .unwrap_err();
        assert!(err.contains("borrow"), "{err}");
    }

    #[test]
    fn borrowed_array_cannot_be_overwritten() {
        let err = compile(
            r#"
            fn main() -> i32 {
                let mut values: [i32; 2] = [1, 2];
                let first: &i32 = &values[0];
                values = [3, 4];
                return *first;
            }
        "#,
        )
        .unwrap_err();
        assert!(err.contains("borrow"), "{err}");
    }

    #[test]
    fn array_borrow_literal_out_of_bounds_is_rejected() {
        let err = compile(
            r#"
            fn main() -> i32 {
                let values: [i32; 2] = [1, 2];
                let element: &i32 = &values[3];
                return *element;
            }
        "#,
        )
        .unwrap_err();
        assert!(err.contains("out of bounds"), "{err}");
    }

    #[test]
    fn mutable_array_element_borrow_requires_mutable_root() {
        let err = compile(
            r#"
            fn main() -> i32 {
                let values: [i32; 2] = [1, 2];
                let element: &mut i32 = &mut values[0];
                return *element;
            }
        "#,
        )
        .unwrap_err();
        assert!(
            err.contains("mutable borrow") || err.contains("let mut"),
            "{err}"
        );
    }

    #[test]
    fn owning_array_drops_each_element_without_dropping_twice() {
        let ir = compile(
            r#"
            struct Resource { token: i32 }
            impl Resource {
                fn drop(self: Resource) -> () { return; }
            }
            fn main() -> i32 {
                let resources: [Resource; 2] = [
                    new Resource { token: 1 }, new Resource { token: 2 }
                ];
                let first: &Resource = &resources[0];
                return (*first).token - 1;
            }
        "#,
        )
        .unwrap();
        assert_eq!(ir.matches("call void @Resource__drop(").count(), 2, "{ir}");
    }

    #[test]
    fn moving_owning_element_out_of_array_is_still_rejected() {
        let err = compile(
            r#"
            struct Resource { token: i32 }
            impl Resource { fn drop(self: Resource) -> () { return; } }
            fn main() -> i32 {
                let items: [Resource; 2] = [
                    new Resource { token: 1 }, new Resource { token: 2 }
                ];
                let removed: Resource = items[0];
                return removed.token;
            }
        "#,
        )
        .unwrap_err();
        assert!(
            err.contains("owning indexed element") || err.contains("move"),
            "{err}"
        );
    }

    #[test]
    fn borrow_nested_method_receiver_does_not_move_struct() {
        let ir = compile(
            r#"
            struct Item { id: i32 }
            impl Item {
                fn get_id(self: &Item) -> i32 { return self.id; }
            }
            fn main() -> i32 {
                let items: [Item; 2] = [new Item { id: 3 }, new Item { id: 5 }];
                return items[1].get_id() - 5;
            }
        "#,
        )
        .unwrap();
        assert!(ir.contains("borrow.index.ok"), "{ir}");
    }

    #[test]
    fn cannot_write_through_shared_borrow() {
        let err = compile(
            r#"
            fn main() -> i32 {
                let mut number: i32 = 1;
                let shared: &i32 = &number;
                *shared = 4;
                return 0;
            }
        "#,
        )
        .unwrap_err();
        assert!(err.contains("&mut") || err.contains("mutable"), "{err}");
    }

    #[test]
    fn conflicting_mutable_borrows_are_rejected() {
        let err = compile(
            r#"
            fn main() -> i32 {
                let mut number: i32 = 8;
                let exclusive: &mut i32 = &mut number;
                let second: &mut i32 = &mut number;
                return *exclusive;
            }
        "#,
        )
        .unwrap_err();
        assert!(err.contains("borrow"), "{err}");
    }

    #[test]
    fn shared_then_mutable_borrow_is_rejected() {
        let err = compile(
            r#"
            fn main() -> i32 {
                let mut number: i32 = 8;
                let shared: &i32 = &number;
                let exclusive: &mut i32 = &mut number;
                return *shared;
            }
        "#,
        )
        .unwrap_err();
        assert!(err.contains("borrow"), "{err}");
    }

    #[test]
    fn returning_reference_to_local_is_rejected() {
        let err = compile(
            r#"
            fn wrong() -> &i32 {
                let number: i32 = 1;
                return &number;
            }
            fn main() -> i32 { return 0; }
        "#,
        )
        .unwrap_err();
        assert!(err.contains("reference") || err.contains("borrow"), "{err}");
    }

    #[test]
    fn conditional_owner_move_requires_definite_availability() {
        let err = compile(
            r#"
            class Resource { token: i32 }
            impl Resource { fn drop(self: Resource) -> () { return; } }
            fn main() -> i32 {
                let resource = new Resource { token: 3 };
                if true { let moved_resource = resource; }
                return resource.token;
            }
        "#,
        )
        .unwrap_err();
        assert!(err.contains("may have been moved"), "{err}");
    }

    #[test]
    fn generated_drop_for_nested_owned_struct() {
        let ir = compile(
            r#"
            struct Child { token: i32 }
            impl Child { fn drop(self: Child) -> () { return; } }
            struct Parent { child: Child }
            fn main() -> i32 {
                let parent = new Parent { child: new Child { token: 1 } };
                return 0;
            }
        "#,
        )
        .unwrap();
        assert!(ir.contains("Parent__drop"), "{ir}");
        assert!(ir.contains("Child__drop"), "{ir}");
    }

    #[test]
    fn consuming_wrapper_uses_generated_nested_drop_without_partial_moves() {
        let ir = compile(
            r#"
            struct RouteCollection { handle: u64 }
            impl RouteCollection {
                fn drop(self: RouteCollection) -> () { return; }
            }
            struct MiddlewareCollection { handle: u64 }
            impl MiddlewareCollection {
                fn drop(self: MiddlewareCollection) -> () { return; }
            }
            struct App { routes: RouteCollection, middlewares: MiddlewareCollection }
            impl App { fn close(self: App) -> () { return; } }
            fn main() -> i32 {
                let app: App = new App {
                    routes: new RouteCollection { handle: 1 },
                    middlewares: new MiddlewareCollection { handle: 2 }
                };
                app.close();
                return 0;
            }
        "#,
        )
        .unwrap();
        assert!(ir.contains("App__drop"), "{ir}");
        assert!(ir.contains("RouteCollection__drop"), "{ir}");
        assert!(ir.contains("MiddlewareCollection__drop"), "{ir}");
    }


    #[test]
    fn c_abi_accepts_pointer_to_repr_c_struct() {
        let source = r#"
            #[repr(C)]
            struct Point { x: i32, y: f64 }
            [[unsafe]] extern fn c_point_sum(point: *const Point) -> i32;
            fn main() -> i32 {
                let point = new Point { x: 4, y: 2.0 };
                unsafe {
                    let raw: *const Point = &point;
                    return c_point_sum(raw);
                }
            }
        "#;
        let ir = compile(source).unwrap();
        assert!(ir.contains("declare i32 @c_point_sum(ptr"), "{ir}");
    }

    #[test]
    fn c_abi_rejects_pointer_to_non_repr_struct() {
        let error = compile(r#"
            struct Point { x: i32 }
            [[unsafe]] extern fn c_point_read(point: *const Point) -> i32;
            fn main() -> i32 { return 0; }
        "#).unwrap_err();
        assert!(error.contains("unsupported value across C ABI"), "{error}");
    }

    #[test]
    fn repr_c_rejects_non_c_field_types() {
        let error = compile(r#"
            #[repr(C)]
            struct Invalid { text: str }
            fn main() -> i32 { return 0; }
        "#).unwrap_err();
        assert!(error.contains("non-C-compatible type"), "{error}");
    }

    #[test]
    fn custom_drop_runs_before_owned_fields_in_reverse_order() {
        let ir = compile(r#"
            struct First { id: i32 }
            impl First { fn drop(self: First) -> () { return; } }
            struct Second { id: i32 }
            impl Second { fn drop(self: Second) -> () { return; } }
            struct LocalGuard { id: i32 }
            impl LocalGuard { fn drop(self: LocalGuard) -> () { return; } }
            struct Parent { first: First, second: Second }
            impl Parent {
                fn drop(self: Parent) -> () {
                    let guard = new LocalGuard { id: 9 };
                    return;
                }
            }
            fn main() -> i32 {
                let parent = new Parent {
                    first: new First { id: 1 },
                    second: new Second { id: 2 }
                };
                return 0;
            }
        "#).unwrap();
        let parent_drop = ir.split("define void @Parent__drop(").nth(1).unwrap();
        let local_call = parent_drop.find("call void @LocalGuard__drop(").unwrap();
        let first_call = parent_drop.find("call void @First__drop(").unwrap();
        let second_call = parent_drop.find("call void @Second__drop(").unwrap();
        assert!(local_call < second_call && second_call < first_call,
            "body locals drop before fields, then fields in reverse declaration order: {parent_drop}");
    }

    #[test]
    fn explicit_destructor_move_suppresses_automatic_drop() {
        let ir = compile(
            r#"
            class Resource { token: i32 }
            impl Resource { fn drop(self: Resource) -> () { return; } }
            fn main() -> i32 {
                let resource = new Resource { token: 5 };
                resource.drop();
                return 0;
            }
        "#,
        )
        .unwrap();
        assert!(ir.contains("Resource__drop"));
    }
    #[test]
    fn unicode_comment_offsets_are_preserved() {
        let source = "// comentário 🌎\nfn main() -> i32 { return 0; }";
        let stripped = strip_comments(source);
        assert_eq!(source.len(), stripped.len());
        assert_eq!(source.find("fn main"), stripped.find("fn main"));
    }
    #[test]
    fn inline_comment_inside_string() {
        assert!(strip_comments("\"url://host\" // comment\n").starts_with("\"url://host\""));
    }
    #[test]
    fn postgres_link_is_optional_until_a_native_call_is_generated() {
        let unused = "declare i64 @__nova_std__postgres____pg_connect(ptr)\n";
        assert!(!needs_postgres_runtime(unused));
        let used = "  %db = call i64 @__nova_std__postgres____pg_connect(ptr %dsn)\n";
        assert!(needs_postgres_runtime(used));
        let unrelated = "  %fd = call i32 @__nova_std__old__net____tcp_listen(i64 8080)\n";
        assert!(!needs_postgres_runtime(unrelated));
    }
    #[test]
    fn optional_native_modules_activate_only_when_referenced() {
        let declarations = concat!(
            "declare i64 @__nova_coro_create(ptr, ptr)\n",
            "declare i32 @__nova_std__old__net____tcp_listen(i64)\n",
            "declare ptr @__nova_std__old__atomic____new_i64(i64)\n",
            "declare ptr @__nova_std__old__sync____mutex_new(i64)\n",
            "declare ptr @__nova_std__old__thread____thread_start(ptr)\n",
        );
        assert!(!needs_coroutine_runtime(declarations));
        assert!(!needs_net_runtime(declarations));
        assert!(!needs_core_runtime(declarations));

        assert!(needs_coroutine_runtime(
            "%task = call i64 @__nova_coro_create(ptr %fn, ptr null)"
        ));
        assert!(needs_coroutine_runtime(
            "call void @__nova_std__old__task____run_until_idle()"
        ));
        assert!(needs_coroutine_runtime(
            "call void @__nova_std__old__generator____drop(i64 %gen)"
        ));
        assert!(needs_net_runtime(
            "%fd = call i32 @__nova_std__old__net____tcp_listen(i64 9999)"
        ));
        assert!(needs_core_runtime(
            "%m = call ptr @__nova_std__old__sync____mutex_new(i64 0)"
        ));
        assert!(needs_core_runtime(
            "%t = call ptr @__nova_std__old__thread____thread_start(ptr %f)"
        ));
        // Taking a function address is a reference too, not merely calling it.
        assert!(needs_net_runtime(
            "store ptr @__nova_std__old__net____tcp_listen, ptr %slot"
        ));
        assert!(!needs_net_runtime(
            "; call i32 @__nova_std__old__net____tcp_listen(i64 9999)"
        ));
    }

    #[test]
    fn runtime_pruning_follows_calls_and_function_addresses() {
        let ir = "define i32 @main() {\nentry:\n  %v = call i32 @std__alive()\n  ret i32 %v\n}\ndefine i32 @std__alive() {\nentry:\n  ret i32 42\n}\ndefine void @std__unused() {\nentry:\n  call void @__nova_std__old__string____string_destroy(i64 1)\n  ret void\n}\n";
        let stripped = prune_unused_std_ir(ir);
        assert!(stripped.contains("define i32 @std__alive"));
        assert!(!stripped.contains("@std__unused"));
        assert!(!needs_core_runtime(&stripped));
        let cb = "@global_callback = global ptr @std__callback\ndefine i32 @main() {\nentry:\n  ret i32 0\n}\ndefine void @std__callback() {\nentry:\n  ret void\n}\n";
        assert!(prune_unused_std_ir(cb).contains("define void @std__callback"));
        let callback_body = "define i32 @main() {\nentry:\n  store ptr @std__callback, ptr null\n  ret i32 0\n}\ndefine void @std__callback() {\nentry:\n  ret void\n}\n";
        assert!(prune_unused_std_ir(callback_body).contains("define void @std__callback"));
    }

    #[test]
    fn atomic_bool_symbols_activate_atomic_runtime() {
        assert!(!needs_atomic_runtime(
            "declare i1 @__nova_atomic_bool_load(ptr, i32)\\n"
        ));
        assert!(needs_atomic_runtime(
            "%value = call i1 @__nova_atomic_bool_load(ptr %address, i32 1)\\n"
        ));
        assert!(needs_atomic_runtime(
            "store ptr @__nova_atomic_bool_compare_exchange, ptr %slot\\n"
        ));
        assert!(needs_atomic_runtime(
            "%value = call i32 @__nova_atomic_i32_load(ptr %address, i32 1)\\n"
        ));
        assert!(needs_atomic_runtime(
            "%value = call ptr @__nova_atomic_ptr_load(ptr %address, i32 1)\\n"
        ));
    }

    #[test]
    fn pure_llvm_program_needs_no_custom_c_runtime() {
        assert!(!needs_core_runtime(
            "define i32 @main() {\nentry:\n  ret i32 42\n}\n"
        ));
        assert!(!needs_core_runtime(
            "declare i64 @__nova_std__old__raw____allocate(i64, i64)\n"
        ));
        assert!(needs_core_runtime(
            "%ptr = call ptr @__nova_std__old__raw____allocate(i64 8, i64 8)"
        ));
        assert!(needs_core_runtime(
            "call void @__nova_json_builder_append(i64 1, ptr %p)"
        ));
        assert!(needs_core_runtime(
            "store ptr @__nova_std__old__string____destroy, ptr %slot"
        ));
    }

    #[test]
    fn embedded_standard_library_compiles() {
        let program = package::load(std::path::Path::new("examples/packages/std_demo")).unwrap();
        let lowered = surface::lower(&program).unwrap();
        let high = hir::lower(&lowered).unwrap();
        let low = mir::lower(&high).unwrap();
        let llvm = codegen::generate(&low);
        // Builder is ordinary Nova code, not an external native-string ABI.
        assert!(llvm.contains("std__old__builder__StringBuilder__to_string"));
        assert!(!llvm.contains("@__nova_std__old__builder____finish"));
        assert!(llvm.contains("std__old__raw__deallocate_bytes"));
        assert!(!llvm.contains("__nova_internal_byte_offset"));
        assert!(llvm.contains("@main"));
    }
    #[test]
    fn raw_pointer_address_casts_are_explicit_and_unsafe() {
        let ok = compile(
            r#"fn main()->i32 {
            unsafe {
                let mut value:i64 = 41;
                let pointer:*mut i64 = &mut value as (*mut i64);
                let raw:u64 = pointer as u64;
                let second:*mut i64 = raw as (*mut i64);
                return (*second as i32) - 41;
            }
        }"#,
        )
        .unwrap();
        assert!(ok.contains("ptrtoint"), "{ok}");
        assert!(ok.contains("inttoptr"), "{ok}");
        let rejected = compile(
            r#"fn main()->i32 {
            let value:i32 = 42;
            let pointer:*const i32 = &value as (*const i32);
            let address:u64 = pointer as u64;
            return 0;
        }"#,
        )
        .unwrap_err();
        assert!(rejected.contains("unsafe"), "{rejected}");
    }

    #[test]
    fn raw_allocator_requires_unsafe() {
        let src="namespace std { namespace old { namespace raw { [[unsafe]] extern fn native_allocate(size: u64, align: u64) -> *mut u8; } } } fn main() -> i32 { let p: *mut u8 = std::old::raw::native_allocate(16, 8); return 0; }";
        let error = compile(src).unwrap_err();
        assert!(
            error.contains("requires an unsafe block"),
            "Erro retornado pelo compilador: {error}"
        );
    }
    #[test]
    fn raw_allocator_is_available_in_unsafe() {
        let src="namespace std { namespace old { namespace raw { [[unsafe]] extern fn native_allocate(size: u64, align: u64) -> *mut u8; [[unsafe]] extern fn native_deallocate(ptr: *mut u8) -> (); } } } fn main() -> i32 { unsafe { let p: *mut u8 = std::old::raw::native_allocate(16, 8); std::old::raw::native_deallocate(p); } return 0; }";
        let ir = compile(src).unwrap();
        assert!(ir.contains("@__nova_std__old__raw____allocate"));
        assert!(ir.contains("@__nova_std__old__raw____deallocate"));
    }
    #[test]
    fn missing_return_is_rejected() {
        let err = compile("fn main() -> i32 { if true { return 1; } }").unwrap_err();
        assert!(err.contains("every path") || err.contains("does not return"));
    }
}

#[cfg(test)]
mod foreign_import_stage14_tests {
    use super::{options, reachable_foreign_symbols, unapproved_foreign_symbols};
    #[test]
    fn deny_all_extern_rejects_even_approved_platform_calls() {
        let program = concat!(
            "declare i64 @write(i32, ptr, i64)\n",
            "declare i64 @llvm.ctpop.i64(i64)\n",
            "define i32 @main() {\nentry:\n",
            "  %n = call i64 @write(i32 1, ptr null, i64 0)\n",
            "  %bits = call i64 @llvm.ctpop.i64(i64 7)\n",
            "  ret i32 0\n}\n",
        );
        assert_eq!(reachable_foreign_symbols(program), vec!["write"]);
        assert!(unapproved_foreign_symbols(program).is_empty());
        let opts = options(&["--deny-all-extern".to_string()]).unwrap();
        assert!(opts.deny_all_extern);
    }
    #[test]
    fn absent_foreign_symbol_is_not_treated_as_live() {
        let program =
            "declare i64 @printf(ptr, ...)\ndefine i32 @main() {\nentry:\n  ret i32 0\n}\n";
        assert!(reachable_foreign_symbols(program).is_empty());
    }
    #[test]
    fn llvm_only_is_accepted_under_strict_profile() {
        let program = "declare i64 @llvm.ctpop.i64(i64)\ndefine i32 @main() {\nentry:\n  %n = call i64 @llvm.ctpop.i64(i64 42)\n  ret i32 0\n}\n";
        assert!(reachable_foreign_symbols(program).is_empty());
    }
}
