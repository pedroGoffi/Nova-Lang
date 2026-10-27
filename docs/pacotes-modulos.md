# Pacotes, módulos e imports

A Nova trabalha com pacotes locais, declarados em `Nova.toml`, e também aceita arquivos `.nova` avulsos. A versão atual é **local-first**: não existe ainda um registry remoto integrado ou resolução automática de pacotes pela rede.

## Manifesto

```toml
[package]
name = "my_app"
version = "0.1.0"
entry = "src/main.nova"

[dependencies]
std = { builtin = "std" }
```

Os campos reconhecidos em `[package]` são `name`, `version` e `entry` (opcional, padrão `src/main.nova`). Dependências utilizam `path` ou a declaração especial `std = { builtin = "std" }`.

> **Importante:** `import std;` não substitui a dependência declarada em `Nova.toml` na implementação atual. A resolução automática da STD é uma proposta para uma versão futura.

## Criar projetos e adicionar dependências

```bash
./target/release/nova new meu_projeto

# Dentro de um projeto existente:
./target/release/nova add std
./target/release/nova add math --path ../math
./target/release/nova tree .
```

Para dependências por caminho:

```toml
[dependencies]
std = { builtin = "std" }
math = { path = "../math" }
```

Não utilize `math = "1.0.0"` como se houvesse um registry remoto: a versão atual do loader não aceita esse formato.

## Organização de arquivos

```text
my_app/
├── Nova.toml
└── src/
    ├── main.nova
    ├── helpers.nova
    └── utils/
        ├── mod.nova
        └── format.nova
```

Exemplo de uso de módulos:

```nova
mod helpers;
mod utils;

use helpers::twice;

fn main() -> i32 {
    return twice(21) - 42;
}
```

A resolução dos módulos usa as regras locais do loader; símbolos não públicos não devem ser acessados de outros módulos. O exemplo concreto [`examples/packages/modules_demo`](../examples/packages/modules_demo) mostra `mod`, `use` e visibilidade com `pub`.

## `import`, `mod`, `use` e `pub`

- `mod`: declara um módulo/arquivo integrante do projeto.
- `use`: disponibiliza um símbolo de um módulo no escopo.
- `pub`: expõe declarações para outros módulos.
- `import`: referencia módulos ou dependências, desde que o loader consiga resolvê-los.

## `Nova.lock`

O loader cria um `Nova.lock` com um **snapshot informativo de caminhos e hashes não criptográficos**. O formato atual pode registrar caminhos absolutos e não é equivalente a um lockfile de registry reproduzível em diferentes máquinas. Ele é gerado durante o carregamento normal de pacotes; o LSP evita modificá-lo ao usar overlays de arquivos não salvos.

Por isso, antes de adotá-lo em CI ou fazer commits em equipe, considere os caminhos locais e a portabilidade do arquivo gerado.

## Segurança de dependências

A Nova não instala pacotes desconhecidos ao encontrar um `import`. Na arquitetura atual, o usuário precisa fornecer explicitamente um caminho de pacote ou declarar a STD builtin. Essa limitação também impede instalação inesperada de código externo.
