# Generators, Async Functions, Tasks, and Spawn

Generators, async functions, and spawned tasks solve different problems:

- a generator produces values incrementally with `yield`;
- an async function creates a task that eventually returns a typed value;
- `spawn` schedules a task, while `wait` joins it and retrieves its result.

These are language features; user code does not depend on `std::coro` or LLVM names.

## Generators

```nova
gen fn counter() -> i64 {
    yield 10;
    yield 32;
    return 0;
}

let coroutine = counter();
let first: i64 = next coroutine;
```

`next` runs until the next `yield`; local state survives suspension. A generator is cooperative and does not automatically create an OS thread. See [`yield`](../examples/features/yield/src/main.nova) and [`coroutines`](../examples/features/coroutines/src/main.nova).

## Async values

```nova
async fn compute(value: i64) -> i64 { return value + 1; }

let task = compute(41);
let result: i64 = task.wait();
```

The task preserves the declared return type. For example, waiting on `async fn ready() -> bool` returns `bool`. See [`async`](../examples/features/async/src/main.nova) and [`async-types`](../examples/features/async-types/src/main.nova).

## Spawn and join

```nova
let left_task = read_left();
let right_task = read_right();
spawn left_task;
spawn right_task;

let left: i64 = left_task.wait();
let right: i64 = right_task.wait();
```

Start independent tasks before waiting for either one. Nova differs from Go goroutines because the typed task handle directly provides `wait()` and its result; generators are also distinct from goroutines. See [`thread`](../examples/features/thread/src/main.nova).
