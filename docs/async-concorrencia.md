# Async, corrotinas e concorrência

A Nova possui um conjunto experimental de recursos para programação assíncrona e concorrente, com integração ao runtime nativo. **Isso ainda não representa um runtime async completo, multiplataforma e pronto para produção.**

## Async e corrotinas

A sintaxe inclui `async fn`, `gen fn`, `coro fn`, `await`, `yield`, `join`, `next` e `resume` em casos suportados.

```nova
async fn double(value: i64) -> i64 {
    return value * 2;
}

fn main() -> i32 {
    let task: std::task::Task<i64> = std::task::manage(double(21));
    let result: i64 = join task;
    return (result - 42) as i32;
}
```

O exemplo anterior é baseado em [`examples/packages/typed_async_016`](../examples/packages/typed_async_016). Os wrappers genéricos `Task<T>` e correlatos preservam tipos na superfície, mas a ABI atual do runtime impõe restrições a resultados representáveis em uma palavra (`i64`/`u64`).

## Sincronização

A STD fornece operações de sincronização e mutexes. A forma `sync(...) as guard { ... }` delimita o uso de uma guarda:

```nova
import std;

fn main() -> i32 {
    let counter = std::sync::new_i64(0);
    sync(counter) as guard {
        guard.set(42);
    }
    return 0;
}
```

O exemplo completo em [`examples/packages/std_concurrency_028`](../examples/packages/std_concurrency_028) utiliza duas guardas (`sync(state, counter) as guard`) e vetores. O compilador inclui verificações estáticas para aquisição incorreta e algumas situações de deadlock/await enquanto uma guarda está ativa.

## Threads, atomics e tarefas

`std::thread`, `std::atomic`, `std::sync` e `std::task` estão presentes na STD. Existem primitivas nativas de pthread/C11 e verificações de assinatura e contextos de segurança para callbacks de threads.

## Limitações e cuidados

- Não há prova geral de `Send`/`Sync` para todos os tipos.
- A ABI async não transporta genericamente estruturas arbitrárias.
- Cancelamento, unwinding e execução de `drop` em frames suspensos ainda são incompletos.
- O runtime de corrotinas é prioritariamente Linux e usa primitivas POSIX.
- APIs históricas baseadas em handles ainda exigem disciplina de ownership.
- Rede async/HTTP implementada aqui é experimental e não deve ser anunciada como servidor HTTPS/produção.

Leia também [Ownership e RAII](ownership-raii.md), [Arquitetura](arquitetura.md) e [Limitações](limitacoes.md).
