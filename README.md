# Nova

**Uma linguagem de programação compilada, experimental, com sintaxe moderna, tipagem estática, ownership e geração de código nativo.**

Nova é um projeto de linguagem e compilador em desenvolvimento. O compilador principal é escrito em **Rust**, utiliza **LALRPOP** para o parser e passa por **AST → HIR → MIR → LLVM IR** antes de gerar um executável nativo com **Clang**. Sua biblioteca padrão (`std/`) é escrita principalmente em Nova e utiliza um runtime C para serviços de baixo nível.

> **Estado atual:** *alpha / projeto experimental*. O manifesto Rust identifica o compilador como `0.20.0-alpha.1`. Os números nos nomes de alguns exemplos (como `format_macros_038`) indicam incrementos de desenvolvimento, **não** uma versão publicada equivalente. A linguagem ainda possui limitações de portabilidade, segurança e compatibilidade; veja [Limitações](docs/limitacoes.md).

## Uma amostra da Nova

```nova
fn main() -> i32 {
    let linguagem = "Nova";
    let versao: i64 = 20;

    println!("Olá, {}!", linguagem);
    println!("Compilador alpha: {}", versao);

    let mensagem = format!("Resultado: {}", 21 + 21);
    println!("{}", mensagem);

    return 0;
}
```

As macros de formatação fazem parte do snapshot atual. Para projetos com `Nova.toml`, declare a STD como mostrado em [Primeiros passos](docs/primeiros-passos.md).

## Destaques

- **Compilação nativa:** frontend em Rust/LALRPOP, análise semântica, HIR/MIR e backend LLVM/Clang.
- **Tipos estáticos e genéricos:** inferência local, templates, traits, conceitos (`concept`, `where`, `requires`) e monomorfização.
- **Gerenciamento de recursos:** moves, empréstimos `&T`/`&mut T`, `drop`, RAII e `defer` com restrições ainda experimentais.
- **Macros de AST:** macros declarativas `macro_rules!`, fragmentos tipados, repetição, restrições por concept e macros de formatação.
- **Concorrência e corrotinas:** primitivas experimentais de `async`/`await`, geradores, tarefas e `sync`.
- **Biblioteca padrão modular:** strings, coleções, JSON, I/O, arquivos, rede, tempo, tarefas e outras APIs.
- **Ferramentas:** CLI (`check`, `build`, `run`, `fmt`, `lint`, `test`, `doc`), parser self-host experimental e extensão para VS Code/VSCodium.

## Instalação rápida

**Ambiente principal:** Linux 64 bits, Rust/Cargo e Clang 15 ou superior. A implementação das corrotinas é particularmente orientada a Linux; não há suporte de cross-compilation amplo validado.

```bash
# Na raiz deste repositório:
cargo build --release
cargo test

# Conferir os comandos disponíveis:
./target/release/nova --help

# Criar e executar um projeto:
./target/release/nova new hello_nova
./target/release/nova run hello_nova
```

`nova new` cria um `Nova.toml` com a dependência da biblioteca padrão embutida. A compilação e a execução exigem que `clang` esteja disponível no `PATH`.

Veja também [Instalação](docs/instalacao.md) e [Primeiros passos](docs/primeiros-passos.md).

## Exemplo: conceitos

```nova
concept Whole<T> = integer<T>;

fn twice<T: Whole>(value: T) -> T {
    return value + value;
}

fn main() -> i32 {
    return twice(21) - 42;
}
```

O verificador confere a restrição ao especializar a função. Os concepts são metadados de compilação e não exigem objetos de runtime.

## Exemplos do repositório

| Exemplo | Demonstra |
| --- | --- |
| [`examples/packages/print_macros_037`](examples/packages/print_macros_037) | `print!`, `println!`, `eprint!` e interpolação |
| [`examples/packages/format_macros_038`](examples/packages/format_macros_038) | `format!` com `String` gerenciada |
| [`examples/packages/ast_macro_rules_033`](examples/packages/ast_macro_rules_033) | `macro_rules!`, repetição e higiene de variáveis locais |
| [`examples/packages/ast_macro_concepts_034`](examples/packages/ast_macro_concepts_034) | Concepts aplicados a argumentos de macros |
| [`examples/packages/concepts_018`](examples/packages/concepts_018) | Genéricos com restrições |
| [`examples/packages/std_concurrency_028`](examples/packages/std_concurrency_028) | Mutexes, `sync` e operações sobre vetores |
| [`examples/packages/modules_demo`](examples/packages/modules_demo) | Organização por módulos e imports |

Para executar qualquer pacote: `./target/release/nova run examples/packages/NOME_DO_PACOTE`.

## Documentação

**[Índice completo da documentação](docs/README.md)**

- [Instalação e requisitos](docs/instalacao.md) · [Primeiros passos](docs/primeiros-passos.md) · [CLI](docs/cli-ferramentas.md)
- [Sintaxe e fluxo de controle](docs/sintaxe.md) · [Tipos, genéricos e concepts](docs/tipos-concepts.md)
- [Ownership, empréstimos e RAII](docs/ownership-raii.md) · [Macros e formatação](docs/macros-formatacao.md)
- [Módulos e pacotes](docs/pacotes-modulos.md) · [STD](docs/stdlib.md)
- [Async e concorrência](docs/async-concorrencia.md) · [Arquitetura](docs/arquitetura.md)
- [Limitações conhecidas](docs/limitacoes.md) · [Como contribuir](CONTRIBUTING.md)

Os documentos com nomes históricos/versionados em `docs/` permanecem como registros de implementação; para aprender a usar o snapshot atual, **comece pelos guias acima**.

## Ferramentas de desenvolvimento

```bash
./target/release/nova check examples/packages/format_macros_038
./target/release/nova lint examples/packages/format_macros_038
./target/release/nova fmt examples/packages/format_macros_038 --check
./target/release/nova dump-ast examples/hello.nova
./target/release/nova lsp-probe
```

A extensão [Nova Language Tools](nova-vscode/) utiliza o compilador para diagnósticos semânticos. A navegação de símbolos no editor ainda não equivale a um servidor de linguagem completo.

## Desenvolvimento e contribuições

O projeto é experimental e aceita melhorias no compilador, na STD, nos testes, no runtime e na documentação. Antes de propor uma mudança, consulte [CONTRIBUTING.md](CONTRIBUTING.md) e valide `cargo test` no seu ambiente.

**Licenciamento:** o snapshot fornecido não contém uma licença de código-fonte na raiz. **Defina e adicione um `LICENSE` para o repositório antes de declarar publicamente uma licença.** A licença da extensão em `nova-vscode/LICENSE` não estabelece, por si só, a licença do compilador inteiro.
