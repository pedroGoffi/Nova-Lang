# Arquitetura do compilador Nova

O compilador principal da Nova está implementado em Rust, com gramática LALRPOP e geração de LLVM IR. O diretório `selfhost/` contém um experimento de lexer/parser escrito na própria linguagem, mas ainda não substitui todo o compilador principal.

## Pipeline

```text
Arquivos .nova / Nova.toml
         │
         ▼
Loader de pacotes e módulos ───────── src/package.rs, src/modules.rs
         │
         ▼
Lexer e parser LALRPOP ─────────────── src/grammar.lalrpop
         │
         ▼
AST ────────────────────────────────── src/ast.rs
         │
         ▼
Expansão de macros, concepts, ─────── src/macros.rs, src/format_macros.rs,
checagens semânticas e lowering         src/concepts.rs, src/surface.rs
         │
         ▼
HIR e análise de ownership ─────────── src/hir.rs, src/ownership.rs
         │
         ▼
MIR e verificação ──────────────────── src/mir.rs
         │
         ▼
LLVM IR ────────────────────────────── src/codegen.rs
         │
         ▼
Clang + runtime C ──────────────────── runtime/nova_*.c
         │
         ▼
Executável Linux nativo
```

O pipeline é uma descrição das responsabilidades principais; transformações de conceitos, macros, chamadas nomeadas, corrotinas e efeitos podem participar de etapas diferentes, dependendo do recurso.

## Estrutura do repositório

| Caminho | Responsabilidade |
| --- | --- |
| `src/` | Driver CLI, parser, AST, análise estática e compilação para LLVM. |
| `src/grammar.lalrpop` | Gramática principal. |
| `std/src/` | Biblioteca padrão modular escrita em Nova. |
| `runtime/` | Runtime C (strings, sincronização, rede, corrotinas e operações nativas). |
| `examples/` | Arquivos avulsos e pacotes de demonstração/regressão. |
| `tests/` | Testes e auxiliares de runtime. |
| `selfhost/` | Parser experimental escrito em Nova. |
| `nova-vscode/` | Extensão para VS Code/VSCodium e servidor NovaLS. |
| `docs/` | Guias de uso e registros históricos de implementação. |
| `packages/` | Pacotes experimentais externos à STD, incluindo componentes Nexo. |

## Biblioteca padrão e ABI

A STD é incluída no executável do compilador por fontes embutidas. Ela apresenta namespaces de alto nível em Nova, enquanto símbolos internos específicos são implementados pelo runtime C. O driver inclui os fontes C do runtime no processo de compilação do programa e invoca Clang para realizar a geração/linkedição nativa.

O módulo `std::internal` é privado à biblioteca padrão. O namespace `std::libc` concentra interfaces C/FFI de menor nível, que exigem cuidados específicos com `unsafe`.

## Diagnósticos e ferramentas

Erros de parse, de tipos, de ownership, de concepts e de fases intermediárias passam pelo módulo `src/diagnostics.rs`. A extensão do editor utiliza `nova lint`/`nova check` e overlays do loader, em vez de manter outro verificador de tipos separado.

## Auto-hospedagem

`nova bootstrap-selfhost` compila o parser Nova que fica em `selfhost/`. `nova parse-selfhost` valida a sintaxe com esse parser. **Ele não substitui o frontend semântico, HIR/MIR, geração LLVM ou o compilador Rust completo.**

## Princípios de desenvolvimento

1. Preferir funcionalidades da linguagem implementadas em Nova sempre que viável; reservar C para ABI/runtime básico.
2. Manter a segurança semântica no compilador e não duplicar o checker em macros ou na extensão.
3. Evitar caminhos alternativos para o mesmo recurso de ownership ou formatador.
4. Preservar regressões: `cargo test`, testes de exemplos e testes de runtime.
5. Diferenciar explicitamente recursos experimentais de garantias de produção.

[Limitações conhecidas](limitacoes.md) · [Como contribuir](../CONTRIBUTING.md)
