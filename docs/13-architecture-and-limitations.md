# Compiler Architecture and Current Limitations

## Compilation pipeline

```text
Nova source
   ↓
LALRPOP parser / AST
   ↓
package and module resolution
   ↓
surface and type analysis
   ↓
HIR
   ↓
MIR
   ↓
LLVM IR
   ↓
Clang / native executable
```

Diagnostic phases identify where a failure occurred:

- `module loading`: packages, imports, visibility, and paths;
- `surface`: names, signatures, and high-level types;
- `hir`: ownership, calls, and typed representation;
- backend/codegen: LLVM generation, linking, and ABI.

Fix errors in phase order because one parser error may cause many later type errors.

## Low-level boundary

LLVM and native symbols stay behind internal standard-library modules. Normal Nova programs should use language intrinsics and high-level APIs. `--deny-unapproved-extern` and `--deny-all-extern` audit this boundary.

## Alpha limitations

- The primary target is Linux x86-64.
- Linking depends on host Clang and installed libraries.
- Cross-compilation requires an external linker and sysroot.
- Coroutines and task scheduling are still evolving.
- GUI examples require SDL2, SDL2_ttf, and an installed font.
- Some collections and network APIs are not yet as mature as Rust or Go equivalents.
- The GUI editor follows Emacs interaction patterns but does not implement Emacs Lisp or all of GNU Emacs.
- Some diagnostics can only report the nearest reliable context instead of an exact span.

## Investigation tools

```bash
nova dump-ast file.nova
nova dump-hir file.nova
nova dump-mir file.nova
nova emit-llvm file.nova -o file.ll
nova doctor
```

Use AST for syntax, HIR for resolution/types, MIR for flow and ownership, and LLVM IR for backend issues.

When language behavior changes, update the compiler, the corresponding `examples/features` package, this guide, and the tests. Interactive backend and GUI examples also require manual runtime checks.
