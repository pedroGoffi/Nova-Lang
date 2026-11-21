# Variables, Functions, and Control Flow

## Bindings and mutability

```nova
let answer: i32 = 40 + 2;
let inferred = 42;
let mut total: i32 = 0;
total += 10;
```

The type follows `:`. Local types may be inferred. Bindings are immutable by default; `mut` makes later assignment explicit. See [`basics`](../examples/features/basics/src/main.nova).

## Functions

```nova
fn square(value: i32) -> i32 {
    return value * value;
}

fn announce() -> () { return; }
```

Parameters and return values are typed. `()` means no useful return value. See [`functions`](../examples/features/functions/src/main.nova).

## Conditions and loops

```nova
if answer == 42 { return 0; }

let mut total: i32 = 0;
for value in 0..5 { total += value; }
while total < 15 { total += 1; }
```

`0..5` visits zero through four; the upper bound is excluded. `while` repeats while its condition is true. See [`control-flow`](../examples/features/control-flow/src/main.nova).

## Explicit casts

```nova
let wide: i64 = 42;
let narrow: i32 = wide as i32;
```

Casts to smaller types may lose information, so use them only when the range is known.
