# Limitações conhecidas e escopo atual

A Nova é uma linguagem **alpha**. Este documento evita que recursos experimentais ou previstos sejam interpretados como garantias completas. O compilador e a STD podem mudar entre snapshots sem compromisso de compatibilidade binária ou estabilidade de APIs.

## Plataforma e toolchain

- A compilação e o runtime nativo priorizam **Linux 64 bits** e **Clang 15+**.
- Cross-compilation, ABI e portabilidade para outros sistemas operacionais não têm cobertura ampla.
- Parte das corrotinas usa mecanismos nativos/POSIX que exigem evolução para funcionar em outras arquiteturas.

## Memória e segurança

- Ownership, moves, borrows e RAII têm checagem estática real, mas não equivalem à disciplina integral do Rust.
- Empréstimos podem ter escopo lexical conservador; NLL completo e lifetimes explícitos não fazem parte da superfície atual.
- Moves parciais, reborrowing complexo, lifetimes em agregados e limpeza de recursos em todo caminho assíncrono não estão completos.
- A existência de operações `unsafe`/FFI não significa que todos os estados inválidos sejam prevenidos pelo compilador.

## Sistema de tipos

- O suporte a traits, associated types, generics, `requires` e concepts é um subconjunto implementado para cenários definidos, não todo o Rust/C++.
- Não há implementação generalizada de `dyn Trait`/trait objects, GATs, const generics arbitrários ou um solver de traits plenamente geral.
- A resolução de overloads e a inferência avançada são limitadas.

## Macros

- `macro_rules!` opera sobre fragmentos da AST e repetições suportadas; não há token trees arbitrárias, procedural macros, derives procedurais nem higiene sintática completa do Rust.
- `print!`, `println!`, `eprint!`, `eprintln!` e `format!` cobrem um subconjunto de formatação; por exemplo, formatos hexadecimais e alinhamento avançado ainda não foram implementados.
- A formatação de objetos customizados depende de métodos compatíveis como `to_string()` e `to_debug_string()`.

## Corrotinas, threads e rede

- Task/Future/Generator/Coroutine têm tipos na superfície, mas resultados transportados pelo runtime atual possuem limites de layout (especialmente `i64`/`u64`).
- Cancelamento, destruição de frames suspensos, segurança geral de Send/Sync e execução assíncrona com todos os tipos de dados ainda não são recursos completos.
- O servidor de rede/HTTP existente é experimental, com limitações de parser, TLS e operação de produção.

## Pacotes e dependências

- O sistema de pacotes suporta caminhos locais e uma STD embutida; **não** há registry remoto, gerenciador de versões de terceiros ou download automático por import.
- O `Nova.lock` atual contém hashes não criptográficos e pode registrar caminhos absolutos; não é um mecanismo completo de supply-chain ou builds portáveis reproduzíveis.
- `import std;` não adiciona sozinho uma dependência ao manifesto. Use `std = { builtin = "std" }`.

## Editor e diagnóstico

- O LSP usa o compilador para validação semântica, mas navegação entre módulos, renomeação e referências ainda são incompletas.
- Alguns avisos e erros não têm spans de AST suficientemente precisos e podem aparecer em posições aproximadas.
- A análise normalmente é interrompida pelo primeiro erro de compilação em vez de agregar todos os erros de uma só vez.

## Qualidade e testes

- Uma suíte `cargo test` aprovada confirma apenas a cobertura implementada.
- Nem todos os exemplos estão cobertos por testes end-to-end em todos os alvos.
- **Não recomendamos o projeto para cargas de trabalho críticas de produção** sem auditoria, instrumentação e validação específicas.

## Documentos históricos

Há notas antigas em `docs/` que dizem que determinados recursos não existiam em incrementos anteriores (por exemplo, concepts estruturais ou RAII). Essas notas são histórico; para ver o que está no snapshot fornecido, consulte o código atual e os exemplos. Para expectativas futuras, use uma issue/roadmap do repositório, sem atribuir ETA a funcionalidades ainda não entregues.
