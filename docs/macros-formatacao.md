# Macros de AST e formatação de strings

A Nova possui macros declarativas com expansão de AST, além de macros de formatação reconhecidas pelo compilador. As regras são verificadas antes da geração de LLVM.

## Macros declarativas

```nova
macro_rules! double {
    ($value:expr) => $value + $value;
}

fn main() -> i32 {
    return double!(21) - 42;
}
```

`macro_rules!` escolhe a primeira alternativa que corresponde à chamada. O snapshot suporta fragmentos `expr`, `ident`, `path` e `literal`, algumas repetições `$(...),*`, `$(...)+` e opcionais, e higiene de variáveis locais em expansões suportadas. **Não** é uma implementação completa de token trees/procedural macros do Rust.

### Repetições

```nova
macro_rules! collect {
    ($($value:expr),*) => [$($value),*];
}

fn main() -> i32 {
    let values = collect!(10, 20, 12);
    return values[0] + values[1] + values[2] - 42;
}
```

Veja [`examples/packages/ast_macro_rules_033`](../examples/packages/ast_macro_rules_033).

### Restrições por concept

```nova
concept Whole<T> = integer<T>;

macro_rules! checked {
    ($value:expr where Whole) => $value;
}

fn main() -> i32 {
    return checked!(42) - 42;
}
```

A expressão capturada é verificada pelo sistema de tipos e concepts real. Para callbacks, é possível impor uma assinatura com `same_as<fn(i64)->i64>`; veja `src/concepts_integration_tests.rs` e os exemplos de macros com concepts.

## `print!` e `println!`

```nova
import std;

fn main() -> i32 {
    let name: str = "Nova";
    let price: f64 = 29.90;
    println!("Olá, {}!", name);
    print!("Preço: R$ {:.2}", price);
    println!();
    eprintln!("Mensagem em stderr: {}", 404);
    return 0;
}
```

Macros de saída disponíveis:

| Macro | Resultado |
| --- | --- |
| `print!(...)` | Escreve em stdout, sem nova linha automática. |
| `println!(...)` | Escreve em stdout e acrescenta nova linha. |
| `eprint!(...)` | Escreve em stderr. |
| `eprintln!(...)` | Escreve em stderr e acrescenta nova linha. |

As macros usam um **literal de formato em tempo de compilação** como primeiro argumento. São aceitas interpolação posicional `{}`, variável local nomeada `{nome}`, escape `{{`/`}}`, debug `{:?}` e precisão fixa `{:N}` com limite `N ≤ 18`, desde que o tipo seja compatível. O parser atual **não** implementa todas as flags de formatação do Rust (por exemplo, `{:x}` e `{:#?}` não são suportados neste snapshot).

## `format!`: criar uma `String`

Diferente das macros de saída, `format!` pode ser usado em posições de expressão e retorna uma `std::string::String` proprietária:

```nova
import std;

fn build() -> std::string::String {
    return format!("Resultado: {}", 21 + 21);
}

fn main() -> i32 {
    let message = build();
    println!("{}", message);
    return 0;
}
```

Também aceita aninhamento:

```nova
let message = format!("Mensagem: {}", format!("valor={}", 42));
println!("{}", message);
```

Os resultados temporários de `format!` seguem as regras de ownership e são tratados por auxiliares da STD. Strings nomeadas podem ser emprestadas; temporários podem ser consumidos pelos adaptadores apropriados.

## Objetos definidos pelo usuário

Para usar `{}` com um tipo personalizado, implemente um método compatível `to_string(self: &T) -> str`; para `{:?}`, utilize `to_debug_string(self: &T) -> str` no subconjunto suportado.

```nova
struct Message { text: str }

impl Message {
    fn to_string(self: &Message) -> str { return self.text; }
    fn to_debug_string(self: &Message) -> str { return self.text; }
}
```

## Diagnósticos

Erros comuns incluem literal ausente, contagem incorreta de placeholders, chaves inválidas, especificador não suportado e tipo não formatável. O compilador usa códigos `NOVA-FMT...` e `NOVA-MAC...` para esses problemas.

## Exemplos para executar

```bash
./target/release/nova run examples/packages/print_macros_037
./target/release/nova run examples/packages/format_macros_038
./target/release/nova check examples/packages/ast_macro_rules_033
```

A implementação é concentrada em `src/format_macros.rs`, `src/macros.rs`, `src/surface.rs` e na STD. Não se deve supor que um `println!` simples tenha todas as características da API equivalente do Rust.
