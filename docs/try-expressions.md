# Try expressions

Nova currently provides typed error contracts and three related try forms:

```nova
use std::error::result::Result;

// A declared error type makes the function result `Result<T, E>`.
fn find_user(id: i32) throws UserError -> User {
    if id < 0 { throw UserError::InvalidId; }
    return Ok(User { id });
}

// Propagate an Option<T> or Result<T, E> failure from this expression.
let user = try find_user(id);

// A block may contain several operations and propagates its final Result/Option.
let profile = try {
    let user = find_user(id);
    load_profile(user.id)
};

// Handle a Result error locally. The catch block must produce the success type.
let profile = try {
    load_profile(id)
} catch error {
    default_profile()
};

// Match individual variants of the error enum.
let profile = try {
    load_profile(id)
} catch {
    ProfileError::NotFound => { default_profile() }
    ProfileError::Forbidden => { public_profile() }
    _ => { cached_profile() }
};
```

`throws E` is declaration sugar for a `Result<T, E>` return contract.
`throw e;` is declaration-body sugar for `return Err(e);`. The type checker
therefore checks the error type through the normal `Result` type.

`try expr` is an expression-level equivalent of the existing postfix `?`.
`try { ... }` applies the same propagation to the block's final value.
Both forms require the enclosing function to return a compatible `Option<T>` or
`Result<T, E>`, as enforced by the existing `NOVA-TRY001` validation.

`try { ... } catch error { ... }` evaluates the block as `Result<T, E>` and
matches `Ok(value)` / `Err(error)`. The `catch` arm must produce a value compatible
with `T`. The pattern form `catch { ... }` matches the `Err` payload, not the
outer `Result`: its patterns are variants of `E`, and its arms must cover every
error case (normally with a final `_` arm).

## Exception model status

These forms currently use typed `Option` / `Result` control flow. `throw` and
`throws E` are typed syntax sugar, not native stack-unwinding exceptions. This
release does not provide LLVM `invoke`, landing pads, personality functions,
cross-FFI exception propagation, or a guarantee that `defer` and destructors run
during native exception unwinding. Those require a separate runtime and ABI design.
