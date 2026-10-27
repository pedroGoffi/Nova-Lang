# Sintaxe da Nova

Este é um resumo de uso, não a especificação formal completa. A gramática atual está em `src/grammar.lalrpop`.

## Comentários e funções

```nova
// Comentário de linha
fn square(value: i64) -> i64 {
    return value * value;
}

fn main() -> i32 {
    return (square(6) - 36) as i32;
}
```

A entrada de um executável é `fn main() -> i32` sem parâmetros.

## Declarações e tipos

```nova
fn main() -> i32 {
    let immutable: i64 = 10;
    let mut count: i64 = 0;
    count += immutable;
    let inferred = count + 1;
    return (inferred - 11) as i32;
}
```

Há inferência local, casts explícitos com `as` e coerções numéricas seguras em determinados contextos. A Nova não permite conversões arbitrárias e implícitas entre tipos incompatíveis.

## Condições e repetições

```nova
fn main() -> i32 {
    let mut total: i32 = 0;
    for index in 0..5 {
        if index == 3 { continue; }
        total += index;
    }
    while total > 0 {
        total -= 1;
    }
    return total;
}
```

Existem `if`/`else`, `while`, `for ... in ...`, `loop`, `break` e `continue`. A iteração de tipos de coleção pode utilizar o protocolo `begin`/`end`/`next`, descrito no histórico [`NATIVE_ITERATORS_MACROS_COROUTINES_0_12.md`](NATIVE_ITERATORS_MACROS_COROUTINES_0_12.md).

## Estruturas, classes e métodos

```nova
struct Point { x: i64, y: i64 }

impl Point {
    fn sum(self: &Point) -> i64 {
        return self.x + self.y;
    }
}

fn main() -> i32 {
    let p = new Point { x: 20, y: 22 };
    return (p.sum() - 42) as i32;
}
```

A gramática também oferece `class`, herança e `override` em cenários restritos. Não presuma equivalência completa a C++ ou Rust quanto a vtables, RTTI e despacho dinâmico.

## Enum e pattern matching

```nova
enum Option<T> { Some(T), None }

fn main() -> i32 {
    let value: Option<i32> = Some(42);
    match value {
        Some(number) => { return number - 42; }
        None => { return 1; }
    }
}
```

O compilador verifica a exaustividade dos padrões suportados.

## Generics, traits e attributes

A Nova reconhece `fn id<T>(...)`, `trait`, `impl`, `typedef`, `concept`, `where`, `static_assert`, anotações `[[pure]]`, `[[unsafe]]`, `[[async]]` e `#[derive(...)]`, cada um com limites próprios. Leia [Tipos e concepts](tipos-concepts.md) e [Limitações](limitacoes.md).

## Módulos

`mod`, `use`, `pub`, `namespace` e `import` organizam arquivos e símbolos. Veja [Pacotes e módulos](pacotes-modulos.md).
