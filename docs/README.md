# The Nova Language Guide

This guide teaches Nova through the executable packages in [`examples/features`](../examples/features). Those packages are checked by `cargo test`, so the snippets track the current compiler.

Nova is alpha software. Syntax and APIs may change.

## Learning path

1. [Installation and first project](01-getting-started.md)
2. [Variables, functions, and control flow](02-fundamentals.md)
3. [Arrays, structs, classes, and inheritance](03-types-and-classes.md)
4. [Traits, generics, concepts, and polymorphism](04-traits-and-generics.md)
5. [Option, Result, and failure handling](05-option-and-result.md)
6. [Modules, imports, macros, and scope functions](06-modules-macros-scopes.md)
7. [Generators, async, tasks, and spawn](07-async-and-coroutines.md)
8. [I/O, formatting, networking, and backend](08-io-network-backend.md)
9. [GUI and the Emacs-inspired editor](09-gui.md)
10. [Unsafe code, pointers, and memory](10-unsafe-and-memory.md)
11. [CLI and package reference](11-cli-and-packages.md)
12. [Ownership, RAII, defer, and the standard library](12-ownership-and-stdlib.md)
13. [Compiler architecture and current limitations](13-architecture-and-limitations.md)

## How to use this guide

Read a chapter, run its linked example, change one value, and run it again. Use `check` before `run` to separate compiler errors from runtime behavior:

```bash
cargo run -- check examples/features/basics
cargo run -- run examples/features/basics
echo $?
```

Small examples return `0` when the feature behaved as expected. Many successful examples intentionally print nothing.

## Feature map

| Feature | Executable example | Chapter |
| --- | --- | --- |
| Variables, functions, and control flow | [`basics`](../examples/features/basics), [`functions`](../examples/features/functions), [`control-flow`](../examples/features/control-flow) | [Fundamentals](02-fundamentals.md) |
| Arrays, structs, and classes | [`arrays`](../examples/features/arrays), [`structs`](../examples/features/structs), [`classes`](../examples/features/classes) | [Types](03-types-and-classes.md) |
| Traits and generics | [`traits`](../examples/features/traits), [`generics`](../examples/features/generics), [`trait-bounds`](../examples/features/trait-bounds) | [Traits](04-traits-and-generics.md) |
| `impl Trait` and `dyn Trait` | [`impl-trait`](../examples/features/impl-trait), [`dyn-trait`](../examples/features/dyn-trait) | [Traits](04-traits-and-generics.md) |
| Structural contracts | [`requires-contracts`](../examples/features/requires-contracts) | [Traits](04-traits-and-generics.md) |
| Compile-time metaprogramming | [`metaprogramming`](../examples/features/metaprogramming) | [Traits](04-traits-and-generics.md) |
| Compile-time effect contracts | [`effects`](../examples/features/effects) | [Traits](04-traits-and-generics.md) |
| Option and Result | [`option-result`](../examples/features/option-result) | [Error handling](05-option-and-result.md) |
| Modules, macros, and scope functions | [`modules`](../examples/features/modules), [`macros`](../examples/features/macros), [`scope-let`](../examples/features/scope-let), [`scope-also`](../examples/features/scope-also), [`scope-with`](../examples/features/scope-with) | [Modules](06-modules-macros-scopes.md) |
| Generators, async, and tasks | [`yield`](../examples/features/yield), [`coroutines`](../examples/features/coroutines), [`async`](../examples/features/async), [`thread`](../examples/features/thread) | [Async](07-async-and-coroutines.md) |
| I/O, networking, and backend | [`io-format`](../examples/features/io-format), [`net`](../examples/features/net), [`backend`](../examples/features/backend) | [I/O](08-io-network-backend.md) |
| SDL2 GUI and editor | [`gui`](../examples/features/gui) | [GUI](09-gui.md) |
| Raw pointers | [`unsafe-pointers`](../examples/features/unsafe-pointers) | [Unsafe](10-unsafe-and-memory.md) |
| Scope cleanup | [`defer`](../examples/features/defer) | [Ownership](12-ownership-and-stdlib.md) |
