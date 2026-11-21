# Traits, Generics, Concepts, and Polymorphism

## Traits

```nova
trait Measure { fn measure(self: &Self) -> i32; }
struct Item { size: i32 }
impl Measure for Item {
    fn measure(self: &Item) -> i32 { return self.size; }
}
```

A trait defines behavior independently of storage. See [`traits`](../examples/features/traits/src/main.nova).

## Generics and concepts

```nova
fn identity<T>(value: T) -> T { return value; }

concept ScorableType<T> = implements<T,Scorable>;
fn read_score<T: ScorableType>(value: &T) -> i32 {
    return value.score();
}
```

Concept constraints are checked during specialization. See [`generics`](../examples/features/generics/src/main.nova) and [`trait-bounds`](../examples/features/trait-bounds/src/main.nova).

## Structural contracts with `requires(...)`

A requires expression describes operations that a type must support:

```nova
concept Addable<T> = requires(T left, T right) {
    { left + right } -> T;
};

fn add<T: Addable>(left: T, right: T) -> T {
    return left + right;
}
```

The names declared in `requires(...)` are compile-time placeholders. The rule above requires addition to compile and return `T`. The compiler validates the contract when `add` is specialized, before LLVM generation. `static_assert(Addable<i32>);` checks it directly. This is a compile-time structural contract, not a runtime boolean guard. See [`requires-contracts`](../examples/features/requires-contracts/src/main.nova).

## Compile-time metaprogramming

`comptime fn` executes in the compiler evaluator:

```nova
comptime fn sum_to(limit: i64) -> i64 {
    let mut total: i64 = 0;
    for value in 0..limit { total += value; }
    return total;
}
```

`#[comptime_assert]` can validate reflected type metadata before runtime code is generated:

```nova
#[comptime_assert]
fn user_metadata_is_correct() -> bool {
    return std::introspection::meta::field_count::<User>() == 2
        && std::introspection::meta::has_field::<User>("id");
}
```

If this function returns `false`, compilation fails. Reflection values remain compile-time data and do not introduce runtime type scanning. See [`metaprogramming`](../examples/features/metaprogramming/src/main.nova).

## Effect contracts

Effects describe capabilities required by a function call. They are declared in Nova source:

```nova
#[meta_effect]
trait telemetry {}
```

A function publishes its requirements after the return type:

```nova
fn record(value: i32) -> i32 requires(telemetry) {
    return value;
}
```

Requirements propagate through calls. Because `compute` calls `record`, it must also declare `requires(telemetry)`; its callers must do the same at their contract boundary. If a caller omits the capability, compilation fails with an effect diagnostic before code generation.

Effects do not perform the operation themselves and add no runtime branch. They make side-effect capabilities explicit and statically checked. See [`effects`](../examples/features/effects/src/main.nova).

## Static and dynamic trait returns

```nova
fn make_greeting() -> impl Render {
    return new Greeting { value: 42 };
}

fn inspect(value: &dyn Measure) -> i32 {
    return value.measure();
}
```

`impl Trait` hides one concrete type while retaining static specialization. `dyn Trait` provides runtime polymorphism and dynamic dispatch. See [`impl-trait`](../examples/features/impl-trait/src/main.nova) and [`dyn-trait`](../examples/features/dyn-trait/src/main.nova).
