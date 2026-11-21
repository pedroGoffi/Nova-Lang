# Option, Result, and Failure Handling

## `Option<T>`

`Option<T>` represents a value that may be absent:

```nova
let value: Option<i32> = Some(42);
let missing: Option<i32> = None;

match value {
    Some(number) => { return number; }
    None => { return 0; }
}
```

Use `Option` when absence is expected and needs no error details.

## `Result<T,E>`

```nova
fn checked(value: i32) -> Result<i32,i32> {
    if value == 42 { return Ok(value); }
    return Err(-1);
}

match checked(42) {
    Ok(value) => { return value - 42; }
    Err(_) => { return 1; }
}
```

`Ok(T)` carries success; `Err(E)` carries failure. Prefer structured error types over magic numbers in public APIs. See [`option-result`](../examples/features/option-result/src/main.nova).

Use `Option` for normal absence and `Result` when callers need to distinguish success from failure. File, network, and system APIs should preserve errors instead of hiding OS status codes.
