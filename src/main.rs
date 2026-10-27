mod ast;
mod call_arguments;
mod codegen;
mod concepts;
mod concurrency;
mod consteval;
mod coroutine_codegen;
mod coroutines;
mod devtools;
mod diagnostics;
mod editor_overlay;
mod effects;
mod hir;
mod iterators;
mod lambdas;
mod macros;
mod mir;
mod modules;
mod ownership;
mod package;
mod surface;
mod type_features;
mod types;
use lalrpop_util::lalrpop_mod;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
lalrpop_mod!(pub grammar);

#[cfg(test)]
mod concepts_integration_tests;
mod format_macros;
#[cfg(test)]
mod function_attributes_tests;
#[cfg(test)]
mod libc_tests;
#[cfg(test)]
mod module_tests;
#[cfg(test)]
mod parser_regressions;
#[cfg(test)]
mod variadic_tests;

fn usage() -> &'static str {
    r#"Nova 0.20 Alpha: Nexo package + typed STDLIB + private runtime ABI — native compiler (Rust + LALRPOP + HIR/MIR + LLVM)
Usage:
  nova new <name> | init | add std | add <name> --path <directory> | tree [directory]
  nova check [file.nova | package-directory]
  nova build [file.nova | package-directory] [-o executable] [--emit-llvm]
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
  nova doc [package-directory] [-o path]     Public symbol index
Options:
  -o, --output <path>    Output artifact
  --opt-level <0..3>     Optimization level for Clang (default: 0)
  --target <triple>      Target triple (cross-linker/sysroot are host dependent)
  --debug                Ask Clang to generate debug information
  --verbose              Print tool invocations
  -h, --help             Show this help
Source modules: mod foo; use foo::bar; pub fn bar() -> i32 { ... }
Loops: for i in 0..10 { ... }, loop { ... }, break, continue, +=
Types: typedef Integer = i32; trait<T in i8,i16,i32,i64> Integer extends Numeric {...}
Traits: impl Integer for i32 {...}; compile-time contracts and inheritance
Closures: capturing fn(...) -> T {...} become callable structs (immutable by-value captures)
Coroutines: gen fn, coro fn, async fn; yield, await (Linux x86-64 bootstrap)
Variadic packs: fn sum(...xs: i64); sum(1,2,3); sum(...xs); xs.len()\nC variadics: unsafe { std::libc::stdio::printf("%d", 5); } (Linux x86-64)
Requires clang 15+ and a 64-bit host for native build. Owning struct/class locals with drop(self) are destroyed on lexical scope exit.
Defer: defer cleanup(); or auto x = make() defer x.close();"#
}

/// Preserve string literals and byte offsets while deleting comments.
pub(crate) fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut quoted = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if quoted {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
        } else if c == '"' {
            quoted = true;
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
#[derive(Default)]
struct Options {
    output: Option<PathBuf>,
    emit_llvm: bool,
    opt: u8,
    target: Option<String>,
    debug: bool,
    verbose: bool,
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
            other => return Err(format!("unknown option '{other}'")),
        }
        i += 1;
    }
    Ok(o)
}
fn require_main(hir: &hir::Program) -> Result<(), String> {
    let f = hir
        .functions
        .iter()
        .find(|f| f.name == "main" && matches!(f.kind, hir::FunctionKind::Defined))
        .ok_or("build/run requires fn main() -> i32")?;
    if f.result != hir.types.i32() || !f.params.is_empty() {
        return Err("entry point must be fn main() -> i32".into());
    }
    Ok(())
}
fn llvm_path(output: &Path) -> PathBuf {
    output.with_extension("ll")
}
fn build(ir: &str, output: &Path, options: &Options) -> Result<(), String> {
    if !cfg!(all(target_os = "linux", target_pointer_width = "64")) {
        return Err("Nova 0.8 coroutine runtime currently requires a 64-bit Linux host".into());
    }
    if let Some(target) = &options.target {
        if !target.contains("linux") || !(target.contains("x86_64") || target.contains("aarch64")) {
            return Err("Nova 0.8 runtime target must be Linux 64-bit; cross-toolchains are not yet supported".into());
        }
    }
    let intermediate = llvm_path(output);
    if output == intermediate {
        return Err("executable output path cannot end in .ll".into());
    }
    fs::write(&intermediate, ir).map_err(|e| format!("{}: {e}", intermediate.display()))?;
    // Embed the C11 runtime in the Nova binary: cargo install --path . works
    // even when the original compiler source directory is no longer present.
    let runtime_source = output.with_extension("nova_rt.c");
    fs::write(&runtime_source, include_str!("../runtime/nova_rt.c"))
        .map_err(|e| format!("{}: {e}", runtime_source.display()))?;
    let coro_source = output.with_extension("nova_coroutines.c");
    fs::write(&coro_source, include_str!("../runtime/nova_coroutines.c"))
        .map_err(|e| format!("{}: {e}", coro_source.display()))?;
    let net_source = output.with_extension("nova_net.c");
    fs::write(&net_source, include_str!("../runtime/nova_net.c"))
        .map_err(|e| format!("{}: {e}", net_source.display()))?;
    let atomic_source = output.with_extension("nova_atomic.c");
    fs::write(&atomic_source, include_str!("../runtime/nova_atomic.c"))
        .map_err(|error| format!("{}: {error}", atomic_source.display()))?;
    let sync_source = output.with_extension("nova_sync.c");
    fs::write(&sync_source, include_str!("../runtime/nova_sync.c"))
        .map_err(|error| format!("{}: {error}", sync_source.display()))?;
    let mut command = Command::new("clang");
    command
        .arg("-x")
        .arg("ir")
        .arg(&intermediate)
        .arg("-x")
        .arg("c")
        .arg(&runtime_source)
        .arg(&coro_source)
        .arg(&net_source)
        .arg(&atomic_source)
        .arg(&sync_source)
        .arg("-std=c11")
        .arg("-pthread")
        .arg(format!("-O{}", options.opt))
        .arg("-o")
        .arg(output)
        .arg("-lm");
    if let Some(t) = &options.target {
        command.arg(format!("--target={t}"));
    }
    if options.debug {
        command.arg("-g").arg("-rdynamic");
    }
    if options.verbose {
        eprintln!("{command:?}");
    }
    let result = command.status();
    let _ = fs::remove_file(&runtime_source);
    let _ = fs::remove_file(&coro_source);
    let _ = fs::remove_file(&net_source);
    let _ = fs::remove_file(&atomic_source);
    let _ = fs::remove_file(&sync_source);
    let status = result.map_err(|e| format!("unable to launch clang: {e}"))?;
    if !status.success() {
        return Err(format!(
            "clang failed ({status}); LLVM IR preserved at {}",
            intermediate.display()
        ));
    }
    if !options.emit_llvm {
        let _ = fs::remove_file(&intermediate);
    }
    Ok(())
}
fn compile_executable(input: &Path, output: &Path, opts: &Options) -> Result<(), String> {
    let (ast, sources) = package::load_detailed(input)
        .map_err(|e| diagnostics::attach_package(&e, input, "module loading"))?;
    let ast = surface::lower(&ast)
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
    build(&codegen::generate(&mir), output, opts)
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
    if args[0] == "lint" {
        let path = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("."));
        let (ast, sources) = package::load_detailed(&path)
            .map_err(|e| diagnostics::attach_package(&e, &path, "module loading"))?;
        let ast = surface::lower(&ast)
            .map_err(|e| diagnostics::semantic_with_context(&e, &sources, &path, "surface"))?;
        let hir = hir::lower(&ast)
            .map_err(|e| diagnostics::semantic_with_context(&e, &sources, &path, "hir"))?;
        let _ = mir::lower(&hir)
            .map_err(|e| diagnostics::pipeline_error("NOVA-IR001", &e, &sources, &path, "mir"))?;
        println!("Nova lint: {} warning(s)", devtools::lint(&hir));
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
    let opts = options(&args[option_start..])?;
    let (ast, sources) = package::load_detailed(&path)
        .map_err(|e| diagnostics::attach_package(&e, &path, "module loading"))?;
    if command == "dump-ast" {
        println!("{ast:#?}");
        return Ok(());
    }
    let ast = surface::lower(&ast)
        .map_err(|e| diagnostics::semantic_with_context(&e, &sources, &path, "surface"))?;
    let hir = hir::lower(&ast)
        .map_err(|e| diagnostics::semantic_with_context(&e, &sources, &path, "hir"))?;
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
    let ir = codegen::generate(&mir);
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
        let status = Command::new(&bin)
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
                eprintln!("{error}");
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
    fn embedded_standard_library_compiles() {
        let program = package::load(std::path::Path::new("examples/packages/std_demo")).unwrap();
        let lowered = surface::lower(&program).unwrap();
        let high = hir::lower(&lowered).unwrap();
        let low = mir::lower(&high).unwrap();
        let llvm = codegen::generate(&low);
        assert!(llvm.contains("@__nova_std__builder____finish"));
        assert!(llvm.contains("@__nova_std__arena____reset"));
        assert!(llvm.contains("@__nova_std__vec____int64_vector_get"));
        assert!(llvm.contains("@main"));
    }
    #[test]
    fn raw_allocator_requires_unsafe() {
        let src="namespace std { namespace raw { [[unsafe]] extern fn native_allocate(size: u64, align: u64) -> *mut u8; } } fn main() -> i32 { let p: *mut u8 = std::raw::native_allocate(16, 8); return 0; }";
        let error = compile(src).unwrap_err();
        assert!(
            error.contains("requires an unsafe block"),
            "Erro retornado pelo compilador: {error}"
        );
    }
    #[test]
    fn raw_allocator_is_available_in_unsafe() {
        let src="namespace std { namespace raw { [[unsafe]] extern fn native_allocate(size: u64, align: u64) -> *mut u8; [[unsafe]] extern fn native_deallocate(ptr: *mut u8) -> (); } } fn main() -> i32 { unsafe { let p: *mut u8 = std::raw::native_allocate(16, 8); std::raw::native_deallocate(p); } return 0; }";
        let ir = compile(src).unwrap();
        assert!(ir.contains("@__nova_std__raw____allocate"));
        assert!(ir.contains("@__nova_std__raw____deallocate"));
    }
    #[test]
    fn missing_return_is_rejected() {
        let err = compile("fn main() -> i32 { if true { return 1; } }").unwrap_err();
        assert!(err.contains("every path") || err.contains("does not return"));
    }
}
