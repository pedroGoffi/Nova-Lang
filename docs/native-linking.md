# Native linking

Nova package manifests may declare native link requirements:

```toml
[link]
libraries = ["ssl", "crypto"]
search = ["native/lib"]
static = ["native/lib/libhelper.a"]
rpath = ["$ORIGIN/../lib"]
```

- `libraries` emits `-l<name>` (normally a shared object is selected by the system linker).
- `search` emits `-L<directory>`; relative directories are resolved against the manifest that declares them.
- `static` adds an explicit archive path. Relative paths are resolved against that manifest.
- `rpath` emits a runtime search path. Relative paths are manifest-relative; `$ORIGIN` expressions are preserved for the ELF loader.

Requirements are collected from the root package and all path dependencies. Their declaration order is preserved because static linkers resolve archives from left to right. Duplicate entries are removed without sorting.

A native C function can be declared in Nova with an external function declaration:

```nova
extern fn calculate(a: i32, b: i32) -> i32;

fn main() -> i32 {
    return calculate(20, 10) - 210;
}
```

The declaration supplies the symbol and type signature; `[link]` supplies the library. For example, `libraries = ["math_extra"]` together with `search = ["native/lib"]` links `native/lib/libmath_extra.so` when that is the library selected by the platform linker.

CLI equivalents are `--library NAME` / `-l NAME`, `-L DIRECTORY`, `--static-library PATH`, and `--rpath DIRECTORY`.

## Incremental executable linking

The final executable link has a sidecar `*.nova-cache`. Its key includes generated LLVM IR, Nova compiler version, Clang version, optimization/target/debug options, declared external symbols, link arguments, explicit static archive contents, and matching libraries found in configured search directories. A cache hit skips Clang; changing an input invalidates the entry. Delete the sidecar or run `nova clean` to force a fresh build.

## Current boundary

This feature links native libraries into the final executable. Nova path dependencies are still loaded into the compiler's unified AST/HIR/MIR pipeline; they are **not yet independently compiled Nova archives**. The `.nvi` package interface and per-package `.o`/`.a` artifact pipeline remain separate work and are not implied by this native-linking support.

## Package interface snapshots

A successful `nova build` now writes deterministic public-interface snapshots
for dependency namespaces under `target/nova/packages/<alias>/package.nvi`.
The format starts with `NOVA-NVI 1`, the dependency alias, an interface hash,
and a sorted export list. The file is currently an inspection/cache artifact;
the existing semantic pipeline still loads dependency source and the linker
still links the resulting program as one compilation unit. `.nvi` is not yet a
replacement for source loading, nor does it imply that a package has already
been emitted as an independent `.o`/`.a` artifact.
