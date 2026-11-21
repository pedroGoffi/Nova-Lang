# Modules, Imports, Macros, and Scope Functions

## `import`, `mod`, and `use`

- `import package;` loads a dependency.
- `mod file;` declares a module in the current package.
- `use path::Item;` brings a symbol into scope and keeps call sites short.

The [`modules`](../examples/features/modules) example contains `main.nova` and `math.nova`:

```nova
mod math;
fn main() -> i32 { return math::answer() - 42; }
```

```nova
pub fn answer() -> i32 { return 42; }
```

Symbols are private by default. `pub` exports them. Internal modules are discovered from the package tree; they do not need to be listed in `Nova.toml`.

Additional package roots can be supplied with `--module-path` or `NOVA_PATH`:

```bash
nova check app --module-path /workspace/packages
NOVA_PATH=/workspace/libs:/opt/nova nova check app
```

## Path dependencies

```toml
[dependencies]
std = { builtin = "std" }
nova_gui = { path = "../../../packages/nova_gui" }
```

The source then uses `import nova_gui;`.

## Macros

```nova
macro twice(value) => value + value;
let answer = twice!(21);
```

`!` makes macro expansion explicit. Expansion happens before type checking. Prefer functions unless syntax transformation is required. See [`macros`](../examples/features/macros/src/main.nova).

## Scope functions

`.let` transforms a value and returns the block result:

```nova
let doubled: i32 = value.let { return it + it; };
```

`.also` configures a value and returns that original value:

```nova
let box = new Boxed { value: 42 }.also { it.value += 1; };
```

`with` exposes its receiver as `it` and returns the block result:

```nova
let total: i32 = with(pair) { return it.left + it.right; };
```

Run [`scope-let`](../examples/features/scope-let), [`scope-also`](../examples/features/scope-also), and [`scope-with`](../examples/features/scope-with).
