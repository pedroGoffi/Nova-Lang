# Changelog

## 0.5.1 — Diagnósticos e localização

- Erros sem intervalo exato não aparecem mais falsamente na linha 0.
- Nunca confundir `related function declaration` com o local do erro.
- Fallback limitado à função ativa para `self.campo_inexistente`, com fonte não salva.
- O compilador deve ser recompilado com `src/diagnostics.rs` para produzir um intervalo para o token inválido.
- Nenhuma mudança no tema de cores ou nas verificações semânticas da Nova 0.20.

# Histórico de versões

## 0.5.0 — Nova Studio

- Realce de sintaxe redesenhado com hierarquia cromática (tipos, funções, métodos, variáveis, propriedades, namespaces, atributos, macros e palavras-chave).
- Temas novos **Nova Studio Dark** e **Nova Studio Light**, com contraste equilibrado e menor poluição visual.
- Tema **Nova Aurora Dark** preservado, mas revisado para manter compatibilidade com quem já usava.
- Comentários de documentação `///`, caracteres de escape, parâmetros, declarações e operadores contextualizados.
- Mantém integralmente o servidor LSP e o pipeline semântico da versão 0.4.0.

## 0.4.0 — NovaLS semântico

- Buffers não salvos com ownership/RAII e análise de módulos via compilador atualizado.
