# Package artifacts

`nova package build [directory]` compiles a dependency-free Nova package into
an LLVM IR file, relocatable object, static archive, versioned interface and
metadata record.

Artifacts are written to:

```text
target/debug/packages/<package>-<source-fingerprint>/
  package.nvi
  package.ll
  package.o
  lib<package>.a
  package.nvp
```

The package's manifest name is used as its symbol namespace. The current
cross-package ABI is experimental. Source-free consumption of root-level
public functions is documented in
[`package-consumption-stage7.md`](package-consumption-stage7.md).
