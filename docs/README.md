# Documentação da linguagem Nova

Esta é a documentação de uso do **snapshot atual** do compilador Nova (`0.20.0-alpha.1` no `Cargo.toml`, com incrementos posteriores incorporados ao código). Ela foi preparada a partir do código Rust, da gramática LALRPOP, da biblioteca padrão e dos exemplos disponíveis no repositório.

## Comece aqui

1. [Instalação](instalacao.md): dependências e compilação do compilador.
2. [Primeiros passos](primeiros-passos.md): projeto, código inicial e execução.
3. [Sintaxe](sintaxe.md): declarações, estruturas, funções e controle de fluxo.
4. [Tipos, traits e concepts](tipos-concepts.md): tipagem, genéricos e restrições.
5. [Ownership e RAII](ownership-raii.md): movimentos, empréstimos e liberação de recursos.
6. [Macros e formatação](macros-formatacao.md): `macro_rules!`, `print!`, `println!` e `format!`.
7. [Pacotes e módulos](pacotes-modulos.md): `Nova.toml`, `Nova.lock`, `mod`, `use` e `import`.
8. [Biblioteca padrão](stdlib.md): principais namespaces e limites.
9. [Async e concorrência](async-concorrencia.md): corrotinas, tarefas e sincronização.
10. [CLI e ferramentas](cli-ferramentas.md): commands do compilador, linter, formatador, extensão.
11. [Arquitetura](arquitetura.md): etapas do compilador e componentes do projeto.
12. [Limitações conhecidas](limitacoes.md): diferenças entre suporte real e recursos futuros.

## Convenções

- Os blocos marcados `nova` são exemplos da linguagem; não são Rust.
- Os comandos usam `./target/release/nova` para não depender da instalação global no PATH.
- Exemplos de pacotes completos se encontram em `examples/packages/`.
- Alguns recursos são protótipos: veja sempre [Limitações conhecidas](limitacoes.md).
- O parser self-host em `selfhost/` é um experimento separado do compilador Rust principal.

## Documentação histórica

O diretório `docs/` também contém anotações de desenvolvimento como `CONCEPTS_018.md`, `OWNERSHIP_SUBSYSTEM_015.md`, `LANGUAGE_EXTENSIONS_019.md` e `STDLIB_020_NEXO_ABI.md`. Elas registram **o estado de determinados incrementos** e podem estar desatualizadas ou ser contraditas por mudanças posteriores. Use os guias sem sufixo de versão acima como referência inicial e o código-fonte como autoridade final.

[Voltar ao README principal](../README.md)
