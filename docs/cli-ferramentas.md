# CLI, testes e ferramentas de editor

Todos os comandos abaixo correspondem ao driver `src/main.rs` do snapshot atual. Na raiz do repositório, use `./target/release/nova` após `cargo build --release`.

## Comandos da Nova

| Comando | Finalidade |
| --- | --- |
| `nova new <nome>` | Cria um pacote com `Nova.toml` e `src/main.nova`. |
| `nova init` | Inicializa um pacote no diretório atual. |
| `nova add std` | Registra a STD builtin no manifesto. |
| `nova add <nome> --path <diretorio>` | Registra dependência local por caminho. |
| `nova tree [pasta]` | Inspeciona a árvore do pacote. |
| `nova check [arquivo|pacote]` | Executa análise semântica sem gerar executável. |
| `nova build [arquivo|pacote] [-o nome]` | Compila e linka um executável. |
| `nova run [arquivo|pacote]` | Compila e executa. |
| `nova emit-llvm <arquivo> [-o saida.ll]` | Emite LLVM IR. |
| `nova dump-ast <arquivo>` | Mostra a AST. |
| `nova dump-hir <arquivo>` | Mostra a HIR após lowering. |
| `nova dump-mir <arquivo>` | Mostra a MIR. |
| `nova fmt [arquivo|pasta] [--check]` | Formata ou verifica a formatação. |
| `nova lint [arquivo|pacote]` | Faz checagem semântica e avisa sobre certos usos/declarações. |
| `nova test [pacote]` | Executa testes Nova encontrados no pacote. |
| `nova bench [arquivo|pacote] [repetições]` | Compila e mede repetições da execução. |
| `nova doc [pacote] [-o arquivo]` | Gera índice Markdown de símbolos públicos. |
| `nova clean [pacote]` | Limpa artefatos gerados. |
| `nova lsp-probe` | Informa o protocolo de overlays disponível ao editor. |
| `nova bootstrap-selfhost` | Compila o parser escrito em Nova. |
| `nova parse-selfhost <arquivo>` | Valida sintaxe com parser self-host. |
| `nova parse-rust <arquivo>` | Parse sintático de referência em Rust. |

As flags do driver de build incluem `--emit-llvm`, `--opt-level 0..3`, `--target`, `--debug`, `--verbose` e `-o`. A flag de target não equivale a cross-compilation totalmente suportada.

## Exemplos

```bash
cargo build --release
cargo test
./target/release/nova check examples/packages/format_macros_038
./target/release/nova lint examples/packages/format_macros_038
./target/release/nova fmt examples/packages/format_macros_038 --check
./target/release/nova run examples/packages/format_macros_038
```

`cargo test` testa o compilador **Rust**. `nova test` executa testes da linguagem **Nova**. São operações diferentes.

## Editor: Nova Language Tools

O diretório [`nova-vscode/`](../nova-vscode/) contém a extensão. O `package.json` da extensão identifica a versão **0.5.1**; os artefatos VSIX incluídos no repositório podem ter números anteriores. Confira a versão que instalar em vez de inferir pelo nome do arquivo.

Configuração típica de VS Code/VSCodium:

```json
{
  "nova.compiler.path": "/caminho/absoluto/para/nova/target/release/nova",
  "nova.diagnostics.onType": true,
  "nova.diagnostics.onSave": true,
  "nova.diagnostics.debounceMs": 500
}
```

Depois reinicie o servidor de linguagem pelo comando `Nova: Restart Language Server`. `nova lsp-probe` imprime `nova-lsp-overlays-v2` na versão atual.

### Limites do LSP e linter

O editor consulta o compilador para diagnósticos semânticos e suporta overlays de buffers não salvos. Contudo, a localização de alguns avisos ainda é aproximada ou ausente, e a indexação semântica de símbolos entre módulos permanece parcial. Não interprete um aviso apontado para `1:1` automaticamente como problema na primeira linha do seu arquivo. Detalhes em [`nova-vscode/README.md`](../nova-vscode/README.md).
