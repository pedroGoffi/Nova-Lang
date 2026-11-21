# Unsafe Code, Pointers, and Memory

Nova permits low-level programming, but dangerous operations must be visible inside `unsafe` blocks.

- `*const T` points to data that should not be mutated through that access.
- `*mut T` points to mutable data.
- `&T` and `&mut T` are checked references and should be preferred in public APIs.

## Manual allocation

```nova
use std::memory::raw;

let pointer: *mut i32 = unsafe {
    raw::alloc_record::<i32>() as (*mut i32)
};
unsafe { *pointer = 42; }
let result: i32 = unsafe { *pointer };
unsafe { raw::free_record::<i32>(pointer); }
```

The programmer must guarantee that the pointer is non-null, aligned, alive, and not violating aliasing rules. After `free_record`, the pointer must never be read, written, or freed again. See [`unsafe-pointers`](../examples/features/unsafe-pointers/src/main.nova).

## Layering rule

Application code should use safe abstractions. Keep native symbols in `std::abi`, primitive pointer work in `std::memory::raw`, ownership in allocators/storage/collections, and expose safe types such as `String`, `Vec`, `File`, and `TcpListener`.

Before accepting an unsafe block, identify the allocation owner, its release point, every error path, the valid length/capacity, and whether a safe standard-library abstraction can replace it.
