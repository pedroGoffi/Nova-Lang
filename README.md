# Nova

Nova is an experimental, statically typed, native programming language with ownership, traits, classes, generics, coroutines, async tasks, and a standard library written primarily in Nova.

The production compiler frontend is written in Rust with LALRPOP. Programs pass through AST, HIR, MIR, and LLVM IR before Clang produces a native executable.

> Nova is alpha software. The current Cargo package version is `0.20.0-alpha.1`; syntax, APIs, and ABI may change.

## Example

```nova
import std;
use std::io;

fn main() -> i32 {
    let language = "Nova";
    io::stdout().write_line(format!("Hello from {language}").as_str());
    return 0;
}
```

## Highlights

- Native compilation through LLVM and Clang.
- Static types, local inference, generics, concepts, and traits.
- Classes, inheritance, virtual methods, and trait objects.
- Ownership, `&T`/`&mut T` borrows, RAII, and `defer`.
- Declarative macros and `format!` interpolation.
- Generators, `yield`, typed async tasks, `spawn`, and `wait`.
- Modular standard library for text, collections, memory, I/O, files, networking, time, serialization, concurrency, and platform APIs.
- CLI tools for checking, building, running, formatting, linting, testing, documentation, and compiler representation dumps.

## Build and run

Primary environment: 64-bit Linux with Rust/Cargo and Clang 15 or newer.

```bash
make build
cargo test

cargo run -- --help
cargo run -- check examples/features/basics
cargo run -- run examples/features/basics
```

Install the optimized compiler into `/usr/local/bin`:

```bash
sudo make install
```

For a custom prefix:

```bash
make install PREFIX=/path/to/prefix
```

Run `make help` to list the `build`, `debug`, `test`, `check-examples`, `install`, and `clean` targets. Direct Cargo commands remain supported.

Create a package:

```bash
cargo run -- new hello_nova
cargo run -- run hello_nova
```

## Executable examples

All current examples live under [`examples/features`](examples/features). Each feature is an independent Nova package.

| Example | Demonstrates |
| --- | --- |
| [`basics`](examples/features/basics) | Bindings, types, and conditions |
| [`classes`](examples/features/classes) | Constructors, inheritance, and virtual methods |
| [`trait-bounds`](examples/features/trait-bounds) | Traits, generics, and concepts |
| [`requires-contracts`](examples/features/requires-contracts) | Structural contracts with `requires(...)` |
| [`metaprogramming`](examples/features/metaprogramming) | Compile-time functions and type reflection |
| [`effects`](examples/features/effects) | Source-defined effect contracts and propagation |
| [`impl-trait`](examples/features/impl-trait) / [`dyn-trait`](examples/features/dyn-trait) | Static and dynamic trait abstraction |
| [`async`](examples/features/async) / [`thread`](examples/features/thread) | Typed tasks, spawn, and wait |
| [`defer`](examples/features/defer) | Scope-exit cleanup |
| [`net`](examples/features/net) / [`backend`](examples/features/backend) | Networking and a listening server |
| [`gui`](examples/features/gui) | SDL2 widgets and an Emacs-inspired editor |

Run any example with:

```bash
cargo run -- run examples/features/FEATURE_NAME
```

## Documentation

Start with the **[complete Nova language guide](docs/README.md)**.

- [Getting started](docs/01-getting-started.md)
- [Fundamentals](docs/02-fundamentals.md)
- [Types and classes](docs/03-types-and-classes.md)
- [Traits and generics](docs/04-traits-and-generics.md)
- [Option and Result](docs/05-option-and-result.md)
- [Modules, macros, and scopes](docs/06-modules-macros-scopes.md)
- [Async and coroutines](docs/07-async-and-coroutines.md)
- [I/O, networking, and backend](docs/08-io-network-backend.md)
- [GUI](docs/09-gui.md)
- [Unsafe and memory](docs/10-unsafe-and-memory.md)
- [CLI and packages](docs/11-cli-and-packages.md)
- [Ownership and the standard library](docs/12-ownership-and-stdlib.md)
- [Architecture and limitations](docs/13-architecture-and-limitations.md)
- [Self-hosted frontend](selfhost/README.md)

## Compiler development tools

```bash
cargo run -- dump-ast examples/features/basics/src/main.nova
cargo run -- dump-hir examples/features/basics/src/main.nova
cargo run -- dump-mir examples/features/basics/src/main.nova
cargo run -- doctor
```

The `nova-vscode` directory contains the Nova Language Tools extension. The `selfhost` directory contains the experimental Nova-written lexer and parser.

## Contributing and licensing

See [CONTRIBUTING.md](CONTRIBUTING.md) and run `cargo test` before submitting changes.

The repository root currently has no source-code license file. Add a root `LICENSE` before publishing a licensing claim for the compiler as a whole.
