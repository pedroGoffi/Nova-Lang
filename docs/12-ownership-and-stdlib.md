# Ownership, RAII, Defer, and the Standard Library

## Values and borrows

```nova
fn inspect(value: &Item) -> i32 { return value.size; }
fn update(value: &mut Item) -> () { value.size += 1; return; }
```

Passing `Item` by value may transfer or consume ownership. `&Item` is a temporary read-only borrow; `&mut Item` is a temporary exclusive mutable borrow. A mutable borrow requires a `mut` binding. Never return a reference to a local value that will be destroyed when the function exits.

## RAII and cleanup

Resource-owning types should acquire and release resources inside their class. A type with `drop(self)` may be destroyed at lexical scope exit. Keep one clear owner and exactly one release.

## `defer`

`defer` schedules cleanup for scope exit. The executable example is [`examples/features/defer`](../examples/features/defer).

```nova
import std;
use std::io;

fn cleanup() -> () {
    io::stdout().write_line("3. resource released");
    return;
}

fn execute() -> () {
    io::stdout().write_line("1. resource acquired");
    defer cleanup();
    io::stdout().write_line("2. using resource");
    return;
}

fn main() -> i32 {
    execute();
    return 0;
}
```

Output:

```text
1. resource acquired
2. using resource
3. resource released
```

The cleanup is registered, not called immediately. It runs even when the scope exits through `return`. Multiple defers run in last-in, first-out order:

```nova
defer release_database();
defer release_file();
// Exit order: release_file(), then release_database().
```

## Standard-library map

| Directory | Responsibility |
| --- | --- |
| `core` | Fundamental types, control types, core traits, and prelude |
| `text` | String, builder, UTF-8, formatting, and encoding |
| `collections` | Vectors, lists, sets, maps, trees, heaps, and iterators |
| `memory` | Allocators, storage, owning pointers, and raw memory |
| `error` | Error and Result |
| `io`, `fs` | Readers, writers, files, buffers, directories, and metadata |
| `system` | Processes, commands, environment, and system information |
| `concurrency` | Tasks, threads, channels, mutexes, and atomics |
| `time` | Duration, instant, and clock |
| `net` | Addresses, TCP, UDP, HTTP, and DNS |
| `serialization` | JSON, TOML, and binary formats |
| `platform`, `abi` | Approved platform wrappers and internal native boundaries |

Applications should use `File` instead of `open_at`, `TcpListener` instead of socket syscalls, `String` instead of raw byte pointers, and `Vec<T>` instead of manual reallocation. Use imports to keep paths readable.
