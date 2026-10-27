# Primeiros passos

## Criar um projeto

```bash
./target/release/nova new hello_nova
```

Estrutura gerada:

```text
hello_nova/
├── Nova.toml
└── src/
    └── main.nova
```

O manifesto contém os metadados do pacote e a STD embutida:

```toml
[package]
name = "hello_nova"
version = "0.1.0"
entry = "src/main.nova"

[dependencies]
std = { builtin = "std" }
```

## Escrever um programa

Substitua o conteúdo de `hello_nova/src/main.nova` por:

```nova
import std;

fn main() -> i32 {
    let nome: str = "Nova";
    let idade: i64 = 20;
    println!("Bem-vindo à {}!", nome);
    println!("Versão de exemplo: {}", idade);
    return 0;
}
```

A função de entrada de programas executáveis deve ser exatamente `fn main() -> i32`.

## Verificar, compilar e executar

```bash
./target/release/nova check hello_nova
./target/release/nova build hello_nova
./target/release/nova run hello_nova
```

A compilação passa por AST, análise semântica, HIR, MIR, LLVM IR e linkedição nativa com Clang. `check` **não gera o executável**.

## Introdução às variáveis

```nova
fn main() -> i32 {
    let valor: i64 = 21;
    let mut dobro: i64 = valor * 2;
    dobro += 1;

    if dobro == 43 {
        return 0;
    }
    return 1;
}
```

- `let` declara uma variável imutável por padrão.
- `let mut` permite atribuição posterior.
- Tipos básicos incluem `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`, `f32`, `f64`, `bool` e `str`.
- `auto`/inferência local também estão presentes, conforme o contexto e o tipo.

## Explorar exemplos existentes

```bash
./target/release/nova run examples/packages/print_macros_037
./target/release/nova run examples/packages/format_macros_038
./target/release/nova run examples/packages/concepts_018
```

Exemplos com `_invalid` no nome foram criados para produzir **erros esperados** e são úteis para testar diagnósticos.

## Próximas leituras

[Sintaxe](sintaxe.md) · [Macros e formatação](macros-formatacao.md) · [Pacotes e módulos](pacotes-modulos.md)
