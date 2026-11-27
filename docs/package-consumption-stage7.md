# Stage 7 — source-free package consumption

The package loader now recognizes a compiled dependency directory containing
`package.nvi` and `lib<package>.a` but no `Nova.toml` or Nova source files.

For the initial ABI, `.nvi` public function records are source-like typed
extern declarations. The importer parses those declarations with Nova's own
grammar, places them in the package namespace, and lets the ordinary module,
HIR, MIR and typechecking pipeline validate call sites. The final linker adds
the package archive as a static input. The archive is checked for existence
before semantic analysis/linking.

## Supported surface

- Public, non-generic functions.
- Function parameters and results expressed using type syntax accepted by the
  current Nova parser and FFI-safe scalar/pointer types accepted by HIR.
- Static archive `lib<package>.a`.
- Package alias equal to the package identity embedded in `.nvi`.

## Not yet supported

- Struct/enum layout and nominal type identity across package boundaries.
- Generic functions, monomorphization across package boundaries.
- Constants, traits, concepts, macros and proc macros.
- Stable ABI guarantees across compiler versions or targets.
- Dynamic Nova package artifacts (`.so`) and package registry distribution.

The `.nvi` version remains `NOVA-NVI 2`; this stage uses a restricted
source-like export record for public functions. It is still an experimental
format and should not be treated as a stable external ABI.
