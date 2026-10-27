# Instalação

## Ambiente suportado

O backend e runtime deste snapshot priorizam **Linux de 64 bits**. A compilação nativa usa Clang/LLVM; a implementação de corrotinas usa recursos POSIX e possui limites de arquitetura. macOS e Windows não devem ser considerados plataformas suportadas sem adaptação e testes adicionais.

Pré-requisitos:

- Toolchain Rust com `cargo` e `rustc` (edição Rust 2021).
- **Clang 15+** disponível como comando `clang`.
- Ferramentas C/Linux e bibliotecas padrão para compilar e linkar código nativo (`-pthread`, `-lm`).
- Acesso às dependências Cargo para o primeiro build (`lalrpop` e `lalrpop-util` 0.22).

### Debian/Ubuntu

```bash
sudo apt update
sudo apt install -y build-essential clang llvm pkg-config curl

# Instale Rust/Cargo com o método oficial do projeto Rust, se ainda não estiverem instalados.
rustc --version
cargo --version
clang --version
```

Se Clang ou Cargo não forem encontrados, configure o PATH antes de prosseguir.

## Compilar o compilador

Na raiz do repositório:

```bash
cargo build --release
./target/release/nova --help
```

A gramática em `src/grammar.lalrpop` é processada no build por `build.rs`. O compilador gerado fica em `target/release/nova`.

Execute os testes Rust:

```bash
cargo test
```

A passagem dos testes Cargo verifica partes do compilador, mas não certifica uso em produção nem todas as combinações de runtime e plataformas.

## Primeiro programa

```bash
./target/release/nova new hello_nova
./target/release/nova check hello_nova
./target/release/nova run hello_nova
```

Para gerar apenas o executável:

```bash
./target/release/nova build hello_nova
./hello_nova/target/nova-app
```

O comando `run` compila e executa; `build` compila sem executar. Em um pacote, o executável padrão fica em `<pacote>/target/nova-app`.

## Opcional: instalar o binário Rust

```bash
cargo install --path .
nova --help
```

O binário incorpora os fontes do runtime usados pelo driver nativo, mas **Clang continua necessário** para gerar executáveis Nova.

## Diagnósticos e erros comuns

- `unable to launch clang`: Clang não está instalado ou não está no PATH.
- `requires a 64-bit Linux host`: o driver/runtime deste snapshot não suporta seu alvo.
- Erros em `std::...` em projetos com manifesto: verifique `std = { builtin = "std" }` em `[dependencies]` no `Nova.toml`.
- Erro de parsing: confira os delimitadores e execute `nova check` ou `nova parse-rust` para separar erros sintáticos da análise semântica.

[Próximo: Primeiros passos](primeiros-passos.md)
