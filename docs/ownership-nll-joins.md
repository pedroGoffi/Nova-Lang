# Ownership: joins e NLL (0.20.8)

No join de um `if` em nível de instrução, a análise usa o conjunto de locais
referenciados depois da instrução. Um empréstimo associado a um borrower que
não será usado na continuação pode ser descartado no join. Isso evita manter
um empréstimo morto e evita rejeitar uma atribuição condicional a uma
referência que nunca será lida.

Se o borrower continua vivo, a proveniência precisa estar definida nos dois
caminhos. Quando os dois caminhos fornecem empréstimos, as origens são unidas;
quando apenas um caminho fornece empréstimo, o programa é rejeitado.

A coleta de liveness inclui explicitamente:
- o local de referência usado como destino de `DerefSet`;
- a raiz do objeto usada por `FieldSet`.

A implementação é limitada a joins de `Stmt::If`. Expressões condicionais,
loops e escopos ainda não são analisados por um CFG de pontos de programa.
