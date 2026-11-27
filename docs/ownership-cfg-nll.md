# Ownership CFG e NLL

A Nova constrói um CFG de HIR antes da passagem de ownership. O grafo é usado
para calcular liveness de locais e decidir em quais pontos os empréstimos de
referências locais podem ser liberados.

## Nós

Cada instrução possui um nó com:

- `uses`: locais lidos pela instrução;
- `defs`: locais cujo valor anterior é substituído pela instrução;
- `successors`: nós alcançáveis depois da instrução;
- `live_in` e `live_out`: conjuntos calculados pelo dataflow backward.

## Equações

Para cada nó `n`:

```text
live_out[n] = union(live_in[s]) para s em successors[n]
live_in[n]  = uses[n] union (live_out[n] - defs[n])
```

O solver itera até ponto fixo, portanto backedges de loops propagam liveness
entre iterações.

## Controle estruturado

- `if`: dois sucessores e um ponto de junção;
- `while` e `for`: caminho de saída e backedge do corpo para o cabeçalho;
- `loop`: backedge e saída conservadora;
- `break`: aresta para a saída do loop mais próximo;
- `continue`: aresta para o cabeçalho do loop mais próximo;
- `return`: passa pelo cleanup de `defer` antes de terminar;
- `break` e `continue`: passam pelos cleanups do bloco antes de chegar ao destino do loop;
- `defer`: os locais capturados são usados no nó de cleanup do escopo, não no ponto de declaração;
- `unsafe`: fluxo do bloco interno é incorporado ao bloco externo.

Cada bloco HIR também registra o ponto de continuação após cada instrução
raiz. O visitor de ownership usa `live_in` desse ponto para liberar empréstimos
que não serão usados depois da instrução completa. Isso evita confundir usos
que acontecem dentro de um branch com usos que sobrevivem ao `if`.

## Escopo desta etapa

Este CFG resolve liveness de locais no HIR estruturado. O borrow checker ainda
valida conflitos e proveniência com o visitor de ownership existente. Controle
de fluxo dentro de expressões e análise de regiões/lifetimes ainda não estão
representados integralmente neste grafo.
