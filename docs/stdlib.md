# Biblioteca padrão (STD)

A biblioteca padrão está em `std/src/`, com um ponto de entrada em `std/src/lib.nova`. É distribuída como módulo embutido no compilador e delega certas operações a um runtime C privado.

## Módulos públicos

O snapshot declara os seguintes módulos na STD:

| Área | Namespaces |
| --- | --- |
| Texto | `std::string`, `std::builder`, `std::convert` |
| Coleções | `std::vec`, `std::map`, `std::set`, `std::option`, `std::result` |
| Sistema | `std::io`, `std::fs`, `std::env`, `std::time`, `std::net` |
| Concorrência | `std::task`, `std::thread`, `std::sync`, `std::atomic` |
| Recursos | `std::memory`, `std::arena`, `std::ownership`, `std::raw` |
| Dados | `std::json`, `std::symbol`, `std::math` |
| Metaprogramação | `std::concepts`, `std::ops` |
| Corrotinas | `std::generator`, `std::coro` |
| Outros | `std::error`, `std::panic`, `std::libc` |

`std::internal` não é uma API pública: o compilador restringe o acesso a esses símbolos.

## Importar a STD

Em projetos com manifesto, acrescente:

```toml
[dependencies]
std = { builtin = "std" }
```

No código:

```nova
import std;

fn main() -> i32 {
    std::io::println("Olá, Nova");
    return 0;
}
```

Para imprimir valores formatados, prefira [`println!`](macros-formatacao.md) ao invés de supor que `std::io::println` aceite qualquer tipo: a função base de I/O recebe `str`, enquanto a macro converte os argumentos por tipo.

## Strings gerenciadas

`str` representa uma visão textual, enquanto `std::string::String` é um objeto proprietário com operações como `len`, `as_str`, `clone` e `drop`. Para construir texto:

```nova
import std;

fn main() -> i32 {
    let message = format!("Valor: {}", 42);
    println!("{}", message);
    return 0;
}
```

A `String` produzida por `format!` é administrada pelo sistema de ownership e liberada ao sair do escopo, nos caminhos suportados pelo compilador.

## Limites atuais da STD

Há APIs legadas baseadas em handles, interfaces POSIX e FFI de baixo nível. Nem todas as funções têm contratos de erro recuperáveis e alguns módulos ainda refletem o bootstrap original da linguagem. Leia [Limitações](limitacoes.md) antes de usar rede/arquivos/async em situações críticas.

Referências de código: [`std/src/lib.nova`](../std/src/lib.nova), [`std/src/io.nova`](../std/src/io.nova), [`std/src/string.nova`](../std/src/string.nova).
