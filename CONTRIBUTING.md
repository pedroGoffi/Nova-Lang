# Contribuindo com a Nova

Obrigado pelo interesse em melhorar a Nova. O projeto está em estágio **alpha**, com compilador Rust/LALRPOP/LLVM, STD em Nova e runtime nativo C.

## Antes de começar

1. Leia o [README](README.md), a [arquitetura](docs/arquitetura.md) e as [limitações](docs/limitacoes.md).
2. Verifique se já existe um teste semelhante à sua alteração em `src/`, `examples/` ou `tests/`.
3. Prefira uma mudança pequena, com comportamento e regressões documentados.
4. Não anuncie como completo um recurso que funciona apenas em casos específicos.

## Preparação

```bash
cargo build --release
cargo test
./target/release/nova --help
```

A compilação de programas Nova exige Linux 64 bits, Clang e bibliotecas de desenvolvimento C.

## Onde contribuir

- `src/grammar.lalrpop` e `src/ast.rs`: sintaxe e representação da linguagem.
- `src/surface.rs`, `src/hir.rs`, `src/ownership.rs`, `src/mir.rs`: regras semânticas e IR.
- `src/macros.rs` e `src/format_macros.rs`: macros declarativas e formatação.
- `src/codegen.rs`: geração de LLVM IR.
- `std/src/`: biblioteca padrão; favorecer implementação na linguagem Nova.
- `runtime/`: integração com operações nativas, com ABI e ownership explícitos.
- `nova-vscode/`: integração com o editor e apresentação de diagnósticos.
- `docs/`: exemplos reproduzíveis, documentação e limitações.

## Testes antes de propor uma mudança

```bash
cargo fmt --check
cargo test
cargo build --release
./target/release/nova check examples/packages/concepts_018
./target/release/nova run examples/packages/format_macros_038
```

Um teste nativo C isolado não substitui a execução das etapas Rust/LALRPOP/LLVM. Mudanças em ownership, FFI, formatação, traits ou macros devem incluir casos de sucesso e de erro de compilação.

## Estilo e compatibilidade

- Priorize tipos explícitos nos limites de APIs, diagnósticos claros e nomes descritivos.
- Evite criar um segundo mecanismo para verificação de conceitos, ownership ou tipos dentro do linter/editor.
- Não adicione dependências de rede ou automatize downloads de pacotes sem contrato de segurança e resolução.
- Mantenha a STD encapsulada: APIs públicas legíveis, runtime privado e `unsafe` restrito ao necessário.
- Ao mudar uma sintaxe, atualize os exemplos e os guias em `docs/`.

## Relatos de erros

Informe a versão de `Cargo.toml`, sistema operacional/arquitetura, versões de Rust/Cargo e Clang, o comando executado, a mensagem completa e um caso mínimo `.nova`. Para erros de editor, inclua a saída de `nova lsp-probe`, a versão da extensão e se o arquivo estava salvo.

## Licença

A raiz do snapshot original não possui um arquivo `LICENSE`. Antes de aceitar contribuições externas ou anunciar termos de redistribuição, o mantenedor deve publicar uma licença do repositório e, se necessário, esclarecer como ela se relaciona com a licença de `nova-vscode/`.
