# Nova language examples

Each directory is an independent package focused on one language feature. The
examples are intentionally small: use `cargo run -- run examples/features/<name>`
to execute one package.

Executable feature examples currently included:

- `basics`, `control-flow`, `functions`, `structs`, `classes`
- `generics`, `traits`, `option-result`, `arrays`, `collections`
- `io-format`, `modules`, `unsafe-pointers`, `macros`
- `impl-trait`, `dyn-trait`, `trait-bounds`
- `net`, `backend`, `gui`, `async-types`
- `coroutines`, `yield`, `async`, `thread`, `scope-let`, `scope-also`, `scope-with`

Invalid and diagnostic cases belong in compiler tests, not in executable demos.
