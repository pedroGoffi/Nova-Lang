# Independent Nova package artifact

This fixture builds a source package into an object, static archive and public
interface:

```sh
cargo run -- package build examples/package-artifact --verbose
```

The output directory under `target/debug/packages/` contains `package.nvi`,
`package.ll`, `package.o`, `libmath_core.a` and `package.nvp`.

The current source-free consumer supports root-level public, non-generic
functions with ABI-compatible parameter/result types. Structs, enums, generic
functions and cross-package nominal types are not part of the experimental ABI.

To consume the result without the provider's `.nova` source, follow
[`../package-consumer/README.md`](../package-consumer/README.md). That workflow
copies only `package.nvi` and `libmath_core.a` into the consumer's vendor
folder. The consumer compiler parses the interface into typed extern
signatures and links the archive automatically.
