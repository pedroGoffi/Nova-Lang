#!/usr/bin/env python3
"""Validate the portable NOVA-CST-V1 emitted by the Nova-written parser.

Format stores numeric token spans/ordered tree edges without embedding source.
A CST is not yet a semantic AST and this reader intentionally does not pretend it is.
"""
from __future__ import annotations
from dataclasses import dataclass
from pathlib import Path
import argparse


@dataclass(frozen=True)
class Token:
    kind: int
    start: int
    end: int
    line: int
    column: int


@dataclass(frozen=True)
class Node:
    kind: int
    start_token: int
    end_token: int
    children: tuple[int, ...]


@dataclass(frozen=True)
class CST:
    tokens: tuple[Token, ...]
    nodes: tuple[Node, ...]
    root: int


class CSTError(ValueError):
    pass


def expect(condition: bool, message: str) -> None:
    if not condition:
        raise CSTError(message)


def load(source: Path, serialized: Path) -> CST:
    raw = source.read_bytes()
    lines = serialized.read_text(encoding='utf-8').splitlines()
    expect(bool(lines) and lines[0] == 'NOVA-CST-V1', 'unsupported CST format/version')
    index = 1
    def take(tag: str) -> list[int]:
        nonlocal index
        expect(index < len(lines), f'missing {tag}')
        parts = lines[index].split()
        index += 1
        expect(parts[0] == tag, f'expected {tag}, got {parts[0]}')
        try:
            return [int(x) for x in parts[1:]]
        except ValueError as err:
            raise CSTError(f'non-integer field in {tag}') from err

    (t_count,) = take('TOKENS')
    expect(0 < t_count <= 10_000_000, 'unreasonable token count')
    tokens: list[Token] = []
    last_end = 0
    for i in range(t_count):
        record = take('T')
        expect(len(record) == 6 and record[0] == i, f'token {i}: wrong field count/id')
        kind, start, end, line, col = record[1:]
        expect(1 <= kind <= 7, f'token {i}: unknown lexical kind {kind}')
        expect(last_end <= start <= end <= len(raw), f'token {i}: bad byte span')
        expect(line >= 1 and col >= 1, f'token {i}: bad source location')
        tokens.append(Token(kind, start, end, line, col))
        last_end = end
    expect(tokens[-1].kind == 6 and tokens[-1].start == tokens[-1].end == len(raw), 'missing or malformed EOF token')
    (n_count,) = take('NODES')
    expect(0 < n_count <= 10_000_000, 'unreasonable node count')
    nodes: list[Node] = []
    for i in range(1, n_count + 1):
        fields = take('N')
        expect(len(fields) >= 5 and fields[0] == i, f'node {i}: wrong id/count')
        kind, start, end, child_count = fields[1:5]
        children = tuple(fields[5:])
        expect(1 <= kind <= 1000, f'node {i}: unknown CST kind')
        expect(0 <= start <= end < t_count, f'node {i}: invalid token span')
        expect(child_count == len(children), f'node {i}: wrong child count')
        expect(all(1 <= c <= n_count and c != i for c in children), f'node {i}: invalid child id')
        nodes.append(Node(kind, start, end, children))
    (root,) = take('ROOT')
    expect(index == len(lines), 'unexpected trailing records')
    expect(root == 1 and nodes[0].kind == 1, 'root must be program node 1')
    expect(nodes[0].start_token == 0, 'program must start at first token')
    expect(nodes[0].end_token == t_count - 1, 'program must finish before EOF')
    # Rooted, acyclic ordered tree (Pratt nodes may be allocated after left child).
    colors = [0] * (n_count + 1)
    stack = [(root, False)]
    while stack:
        node, visited = stack.pop()
        if visited:
            colors[node] = 2
            continue
        expect(colors[node] != 1, f'cycle at node {node}')
        expect(colors[node] != 2, f'node {node} attached twice')
        colors[node] = 1
        stack.append((node, True))
        stack.extend((child, False) for child in reversed(nodes[node - 1].children))
    expect(all(color == 2 for color in colors[1:]), 'unreachable nodes in CST')
    return CST(tuple(tokens), tuple(nodes), root)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument('source', type=Path)
    parser.add_argument('cst', type=Path)
    args = parser.parse_args()
    try:
        result = load(args.source, args.cst)
    except (CSTError, ValueError, OSError) as err:
        parser.exit(1, f'error[NOVA-CST]: {err}\n')
    print(f'OK CST-V1 {len(result.tokens)} tokens / {len(result.nodes)} nodes / root {result.root}')

if __name__ == '__main__':
    main()
