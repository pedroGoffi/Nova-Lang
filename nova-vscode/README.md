# Nova Language Tools 0.5.1 — correção de diagnósticos

Esta versão mantém o realce e o LSP anteriores e corrige erros mostrados na linha 0.
Para diagnósticos de campos inexistentes no `surface`, aplique **apenas** o `src/diagnostics.rs` do patch complementar na mesma versão da Nova 0.20 e execute `cargo build --release`.
Quando a posição não pode ser determinada sem ambiguidade, o servidor registra o erro em **Nova: Show Language Server Output**, sem inventar linha/coluna.

# Nova Language Tools 0.5.0

Esta versão deve ser instalada **junto com** o código-fonte de compilador da mesma entrega.
Suporta snapshots de arquivos ainda não salvos, inclusive novos arquivos `.nova`
cuja pasta já existe, validação de ownership/borrow/RAII e demais verificações
estáticas implementadas no compilador 0.20. No VS Code, o status **Semântico**
indica que o executável implementa `lsp-probe` e buffers.

# Nova Language Tools 0.5.0 — LSP semântico

Extensão do VS Code para a linguagem **Nova 0.20 Alpha**, mantendo o realce de sintaxe e tema opcional da versão 0.2.0.

## Novo realce de sintaxe — Nova Studio (0.5.0)

A gramática de cores é TextMate e funciona em qualquer tema do VSCodium, sem precisar de LSP. Ela separa funções declaradas, chamadas, métodos, tipos, parâmetros, campos, módulos, atributos `#[derive(...)]`/`[[unsafe]]`, macros `nome!()`, strings, comentários `///`, recursos `defer`/`unsafe`, corrotinas e operadores.

Temas disponíveis: **Nova Studio Dark** (recomendado, cores equilibradas), **Nova Studio Light** e **Nova Aurora Dark** (compatibilidade). O tema é *opcional* e não é imposto ao usuário.

No VSCodium: `Ctrl+Shift+P` → `Preferences: Color Theme` → `Nova Studio Dark` (ou `Nova Studio Light`). Depois `Developer: Reload Window`.

**O comportamento de lint / ownership / RAII é o mesmo da v0.4.0**; não é necessário reaplicar patch Rust para atualizar apenas as cores.

## Principal mudança

A versão 0.2.0 utilizava `nova parse-rust` para texto ainda não salvo, por isso não detectava **use-after-move**, conflitos de `&mut`, referências inválidas nem violações de RAII até o `nova check` após salvar.

Na 0.4.0, o servidor chama **`nova lint` do pacote real em cada edição** (ele executa o mesmo pipeline de `nova check` e também gera avisos), com as versões não salvas dos arquivos `.nova` abertos fornecidas ao loader de fontes por meio de `NOVA_LSP_OVERLAY_MAP`. Com isso, executa o pipeline existente de Nova: carregamento de pacotes / módulos / STD, parser, `surface`, HIR (incluindo `ownership::check`), MIR e verificação de tipos, com as mesmas regras do compilador. A extensão **não implementa um segundo borrow checker**.

**Para funcionar durante a digitação, é obrigatório aplicar o patch complementar de quatro arquivos Rust e recompilar Nova.** Um compilador anterior não entende overlays. O servidor verifica isso com `nova lsp-probe` antes de ativar a análise semântica em buffers não salvos.

### Instalação no Linux

```bash
# 1. Na raiz do repositório ORIGINAL do compilador Nova 0.20:
cd ~/Desenvolvimento/nova-core-0.7-foundations

# Recomenda-se fazer backup dos 3 arquivos existentes ANTES de substituir.
# Extraia o ZIP complementar nessa raiz, preservando src/.
unzip -o ~/Downloads/nova-lsp-compiler-0.4.0-patch.zip

cargo build --release
./target/release/nova lsp-probe
# Saída esperada: nova-lsp-overlays-v1

# 2. Na pasta de downloads:
codium --install-extension ~/Downloads/nova-language-tools-0.5.0.vsix --force
```

No VS Code: abra um diretório do seu pacote Nova e, em `Preferences: Open User Settings (JSON)`, adicione (ajuste o caminho do compilador à sua máquina):

```json
{
  "nova.compiler.path": "/home/pedro/Desenvolvimento/nova-core-0.7-foundations/target/release/nova",
  "nova.diagnostics.onType": true,
  "nova.diagnostics.onSave": true,
  "nova.diagnostics.debounceMs": 500
}
```

Execute `Developer: Reload Window` ou `Nova: Restart Language Server` no menu de comandos. A barra de status deve indicar **NovaLS · Semântico**. Se indicar **NovaLS · Sintaxe**, abra `Nova: Show Language Server Output`, confira o caminho do compilador, e rode `nova lsp-probe` no terminal.

### Exemplo para testar ownership/RAII

Crie ou abra um arquivo `.nova` compilado pelo pacote e digite **sem salvar**:

```nova
class Resource { token: i32 }
impl Resource {
    fn drop(self: Resource) -> () { return; }
}
fn main() -> i32 {
    let resource = new Resource { token: 3 };
    let transferred = resource;
    return resource.token; // use of moved value
}
```

A seção **Problems** deverá receber um erro semântico do compilador (o exemplo é baseado no teste `owner_use_after_move_is_error` no compilador anexado). O tempo de análise depende do tamanho do pacote.

### Recursos

- Diagnósticos semânticos de arquivos alterados **sem salvar**: ownership, empréstimos `&T`/`&mut T`, RAII/drop, tipos, genéricos, conceitos, templates, módulos, atributos, macros e demais regras que `nova check` executa.
- Fontes em múltiplos módulos abertos: overlays simultâneos sem sobrescrever `.nova` ou `Nova.lock`.
- Diagnósticos de parse com linhas/colunas originais quando fornecidas pelo compilador.
- Avisos do `nova lint` para variáveis não usadas e `mut` desnecessário (com localização aproximada quando o compilador não disponibiliza spans).
- Limpeza de erros quando corrigidos, debounce, verificação no salvamento, comando `Nova: Check Current File / Package`.
- Realce TextMate (incluindo corrotinas, generators, strings, macros, atributos); snippets, formatação, símbolos, autocomplete léxico, hover e definição simples no arquivo atual.
- Fallback explícito para compilador anterior: parser durante a digitação, `nova check` no arquivo salvo; **não** anuncia análise semântica de texto não salvo sem suporte do compilador.

### Limitações reais

- **Inteligência semântica de navegação**: completion, hover e F12 ainda são em grande parte léxicos, não indexação completa de símbolos entre módulos; rename/references/semantic tokens ainda não são implementados. Todos os recursos semânticos do compilador **são verificados como diagnósticos**, não como um IDE completo.
- O compilador Nova 0.20 retorna normalmente **o primeiro erro por execução**. O LSP não consegue listar erros adicionais antes da correção do primeiro, sem suporte novo no compilador.
- Algumas mensagens de ownership e MIR no compilador ainda não guardam spans exatos; nesses casos o diagnóstico pode aparecer no início do arquivo ou na declaração contextual, não necessariamente na expressão problemática. Isso depende da evolução dos spans no AST/HIR.
- Arquivos ainda não salvos pela primeira vez (sem caminho de disco) não têm módulos próprios resolvíveis.
- Os overlays dependem de arquivos `.nova` fisicamente existentes e abertos no VS Code. Dependências de caminho (`Nova.toml`) permanecem referenciadas nos locais originais.
- Não compila/executa projetos a cada tecla e não verifica propriedades dinâmicas de RAII (p. ex. estado de runtime); apenas as regras estáticas do compilador.

## Desenvolvimento

```bash
node --check server/novals.js
node --check client/extension.js
npm test
```

O teste automatizado é executado com um **mock do compilador** e verifica fluxo JSON-RPC, envio de vários buffers não salvos, RAII/moves/borrows como erros simulados, fallback e salvaguardas. O compilador Rust original **não foi compilado neste ambiente**: valide com `cargo build --release` e `cargo test` no seu repositório.
