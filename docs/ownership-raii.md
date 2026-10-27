# Ownership, empréstimos, RAII e `defer`

A Nova possui um sistema experimental de propriedade de valores para evitar usos depois de transferências e chamadas duplicadas de destrutores. Ele é **inspirado em Rust**, mas não é um borrow checker com todas as garantias de Rust.

## Moves e `drop`

```nova
class Resource { token: i32 }

impl Resource {
    fn drop(self: Resource) -> () {
        return;
    }
}

fn main() -> i32 {
    let first = new Resource { token: 3 };
    let second = first; // transfere a propriedade de first
    // usar first aqui seria inválido
    return 0;           // second é descartado ao sair do escopo
}
```

Locais que possuem recursos podem ter `drop(self)` executado ao deixar o escopo. Destrutores gerados para campos proprietários são parte do subsistema RAII.

## Referências

```nova
fn main() -> i32 {
    let mut value: i32 = 10;
    let borrowed: &mut i32 = &mut value;
    *borrowed = 42;
    return *borrowed - 42;
}
```

- `&T`: empréstimo compartilhado.
- `&mut T`: empréstimo mutável exclusivo nos caminhos verificados.
- `*const T` e `*mut T`: ponteiros brutos, sujeitos a `unsafe` para operações perigosas.

O checker rejeita diversos casos de uso após move, empréstimos mutáveis conflitantes e retorno de referências locais expiradas. As regiões de empréstimo são conservadoras (lexicais): código aceito pelo borrow checker mais sofisticado do Rust pode ser rejeitado aqui.

## `defer`

`defer` executa uma ação no encerramento do escopo. O exemplo de referência está em [`examples/advanced/raii_defer.nova`](../examples/advanced/raii_defer.nova), com `defer cleanup();` e `auto resource = make() defer resource.close();`.

## Cuidados

- Não assuma suporte pleno a lifetimes, NLL, moves parciais ou todos os padrões de reborrowing.
- Valores temporários proprietários e recursos capturados por closures/corrotinas exigem cuidado adicional.
- `unsafe` **não** garante automaticamente a segurança de aliasing ou recursos externos.
- Destruição de frames assíncronos suspensos ainda possui limitações; evite recursos críticos nessas condições.

Leitura técnica: [`OWNERSHIP_SUBSYSTEM_015.md`](OWNERSHIP_SUBSYSTEM_015.md) (histórico) e [Limitações atuais](limitacoes.md).
