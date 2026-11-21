# CLI and Package Reference

## Main commands

| Command | Purpose |
| --- | --- |
| `nova new <name>` | Create a package |
| `nova init` | Initialize the current directory |
| `nova check <target>` | Analyze without producing the final executable |
| `nova build <target>` | Build a native executable |
| `nova run <target>` | Build and run |
| `nova fmt <target>` | Format source files |
| `nova lint <target>` | Report style and usage issues |
| `nova test <package>` | Run Nova package tests |
| `nova clean <package>` | Remove package artifacts |
| `nova doc <package>` | Generate a public symbol index |
| `nova dump-ast/hir/mir <file>` | Inspect compiler representations |
| `nova doctor` | Audit compiler/stdlib boundaries |

Use `cargo run --` while developing this repository:

```bash
cargo run -- check examples/features/classes
```

## Manifest

```toml
[package]
name = "my_package"
version = "0.1.0"
entry = "src/main.nova"

[dependencies]
std = { builtin = "std" }
another_library = { path = "../another_library" }
```

Internal files are declared with `mod` and discovered from the package tree. Additional roots can be supplied with `--module-path` or the colon-separated `NOVA_PATH` environment variable.

## Diagnostics and backend inspection

```bash
nova check app --all-errors
nova build app --emit-llvm
nova emit-llvm src/main.nova -o output.ll
nova build app --debug --opt-level 0
```

Fix parsing and module errors before semantic errors. LLVM options are compiler-development tools; ordinary Nova code should use language and standard-library abstractions rather than direct LLVM names.

`cargo test` discovers every directory under `examples/features`, validates its manifest and entry file, and runs `nova check`.
