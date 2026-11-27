# Nova package build graph (stage 4)

`nova build` now resolves the local `Nova.toml` dependency graph before AST
loading and writes deterministic per-package metadata under
`target/nova/packages/<name>-<root-hash>/package.nvp`.

The graph is dependency-first and deduplicated by canonical package directory.
Each package fingerprint includes its name/version, source files under `src/`
(and its configured entry if outside `src/`), and canonical dependency roots.
Changing a dependency source therefore changes that package fingerprint; the
consumer graph also records the dependency identity.

This stage establishes package identity, ordering, and cache inputs. It does
**not** yet emit `.o`/`.a` artifacts or remove dependency AST expansion. Those
require a separate MIR partitioning and cross-package symbol/ABI implementation;
`package.nvi` remains an interface snapshot rather than a linkable ABI contract.


## Stage 5: validated NVI v2 format

`package.nvi` now uses a strict, line-oriented `NOVA-NVI 2` format. Package
names and export records are UTF-8 encoded as hexadecimal so punctuation,
whitespace, and future type syntax cannot corrupt record boundaries. The
reader validates the format version, record count, UTF-8, sorted/unique exports,
absence of trailing records, and the interface fingerprint. The writer reads
back every emitted interface and fails the build if serialization does not
round-trip exactly.

The fingerprint is an incremental-build checksum (FNV-1a), not a cryptographic
signature. NVI v2 is still a compiler metadata snapshot: the current consumer
compiler continues to load dependency source and expand dependency ASTs. It is
not yet a stable ABI, and no `.o`/`.a` package artifacts are emitted in this
stage.
