# Sistema de tipos, generics, traits e concepts

A Nova faz a especialização de funções genéricas no compilador, antes do HIR/MIR final e da emissão LLVM. Concepts são verificados na análise semântica e não existem como valores em runtime.

## Tipos

Tipos numéricos inteiros e de ponto flutuante, `bool`, `str`, arrays, estruturas, enums, referências e ponteiros têm regras próprias. Há inferência de tipos locais e casts explícitos com `as`; nem toda conversão implícita é permitida.

## Genéricos

```nova
fn identity<T>(value: T) -> T {
    return value;
}

fn main() -> i32 {
    return identity(42) - 42;
}
```

Genéricos são especializados para tipos usados. O sistema de inferência e unificação ainda não é um resolvedor genérico completo.

## Concepts

```nova
concept Number<T> = integer<T> || floating<T>;
concept Small<T> = Number<T> && !same_as<T, f64>;

fn twice<T: Small>(value: T) -> T {
    return value + value;
}

static_assert(integer<i32> && !floating<i32>);

fn main() -> i32 {
    return twice(21) - 42;
}
```

Há predicados como `integer`, `signed`, `unsigned`, `floating`, `numeric`, `boolean`, `pointer`, `reference`, `same_as` e `implements`. Os predicados podem ser combinados com `!`, `&&` e `||`.

### Restrições `where`

```nova
concept Equal<A, B> = same_as<A, B>;

fn matches<T, U>(a: T, b: U) -> i32 where Equal<T, U>, integer<T> {
    return 42;
}
```

Uma lista de restrições `where` usa conjunção lógica (AND).

### `requires` estrutural

```nova
struct Point { x: i64, y: i64 }

concept HasX<T> = requires(T value) {
    { value.x } -> i64;
};

static_assert(HasX<Point>);
```

O suporte ao `requires` é deliberadamente parcial: verifica formas específicas de campos, métodos e operadores, não todas as expressões possíveis de C++20.

## Traits

```nova
trait Printable { fn print(self: Self) -> i32; }

impl Printable for i32 {
    fn print(self: i32) -> i32 { return self; }
}

concept CanPrint<T> = implements<T, Printable>;
```

Traits podem declarar tipos associados e alguns métodos padrão. O compilador confere contratos de assinatura, mas não implementa o modelo completo de trait objects/dynamic dispatch do Rust.

## Validação

```bash
./target/release/nova check examples/packages/concepts_018
cargo test concepts
```

Para conceitos em macros, leia [Macros e formatação](macros-formatacao.md).
