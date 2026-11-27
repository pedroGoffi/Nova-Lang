# Source-free package consumer (Stage 7)

This example consumes a compiled Nova package without its `.nova` sources.
The current binary interface supports public, non-generic functions whose
parameter and return types are understood by the parser/typechecker. Structs,
enums, generic functions, constants, traits and cross-package nominal types
are not part of this experimental ABI yet.

From the repository root:

```sh
# 1. Compile the provider package.
cargo run -- package build examples/package-artifact --verbose

# 2. Copy only the public interface and static archive into the consumer.
mkdir -p examples/package-consumer/vendor/math_core
ARTIFACT=$(find examples/package-artifact/target/debug/packages -maxdepth 2 -type f -name package.nvi -print -quit | xargs dirname)
cp "$ARTIFACT/package.nvi" "$ARTIFACT/libmath_core.a" examples/package-consumer/vendor/math_core/

# 3. Build and run the consumer. The provider's src/lib.nova is not loaded.
cargo run -- run examples/package-consumer --verbose
```

Expected result: the executable exits with status `0`.

The dependency alias must match the package identity embedded in `package.nvi`
for this experimental ABI. The distribution directory consists of exactly:

```text
vendor/math_core/
├── package.nvi
└── libmath_core.a
```
