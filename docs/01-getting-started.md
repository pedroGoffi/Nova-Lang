# Installation and Your First Project

## Requirements

The validated environment is 64-bit Linux. Install Rust/Cargo, Clang 15 or newer, a compatible LLVM toolchain, and SDL2/SDL2_ttf for the GUI example.

```bash
make build
cargo run -- --help
```

During development, `cargo run --` forwards the remaining arguments to Nova.

## Install the compiler

The repository includes a `Makefile` that builds the optimized binary and installs it under `PREFIX/bin`:

```bash
sudo make install
```

The default destination is `/usr/local/bin/nova`. To install elsewhere:

```bash
make install PREFIX=/path/to/prefix
```

Packaging systems can stage the installation with `DESTDIR`:

```bash
make install DESTDIR=/package/root PREFIX=/usr
```

Use `make help` to list all supported targets.

## Create a package

```bash
cargo run -- new hello
```

```text
hello/
├── Nova.toml
└── src/
    └── main.nova
```

Minimal manifest:

```toml
[package]
name = "hello"
version = "0.1.0"
entry = "src/main.nova"

[dependencies]
std = { builtin = "std" }
```

Minimal program:

```nova
import std;

fn main() -> i32 {
    return 0;
}
```

`import std;` makes the standard library available. `main` is the entry point, and its `i32` becomes the process exit code. On Unix, zero means success.

## Development loop

```bash
cargo run -- check hello   # Analyze without running.
cargo run -- build hello   # Produce a native executable.
cargo run -- run hello     # Build and execute.
```

Fix parser and module-loading errors before later type or HIR errors. Start with [`examples/features/basics`](../examples/features/basics), change `40 + 2`, and inspect `echo $?` after running it.

`cargo test` validates every package under `examples/features`.
