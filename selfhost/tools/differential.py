#!/usr/bin/env python3
"""Differential language-syntax test: LALRPOP vs Nova parser, with seeded programs.

Requires an already compiled Rust nova executable and selfhost parser executable.
Use: python3 selfhost/tools/differential.py target/debug/nova selfhost/target/nova-selfhost
"""
from __future__ import annotations
import argparse
import os
from pathlib import Path
import random
import subprocess
import tempfile
from cst_v1 import load, CSTError

ROOT = Path(__file__).resolve().parents[2]

def corpus(seed: int, count: int) -> list[tuple[str, str, bool]]:
    sources: list[tuple[str, str, bool]] = []
    for source in sorted((ROOT / 'examples').glob('*.nova')):
        if source.name == 'invalid.nova':
            continue
        sources.append((source.name, source.read_text(encoding='utf8'), True))
    for source in sorted((ROOT / 'selfhost/tests').glob('*.nova')):
        sources.append((source.name, source.read_text(encoding='utf8'), source.name.startswith('valid_')))
    rng = random.Random(seed)
    operators = ['+', '-', '*', '==', '>=', '<<', '&', '|']
    for i in range(count):
        stmts = []
        for j in range(rng.randint(1, 8)):
            op = rng.choice(operators)
            value = rng.randrange(1, 40)
            stmts.append(f'let v{j}: i64 = ({value} {op} {rng.randrange(1, 7)});')
        stmts.append('if true { return 0; } else if false { return 1; } else { return 2; }')
        content = 'fn main() -> i32 {\n' + '\n'.join(stmts) + '\n}'
        sources.append((f'generated_{i:04}.nova', content, True))
    return sources


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('rust', type=Path)
    parser.add_argument('selfhost', type=Path)
    parser.add_argument('--seed', type=int, default=94831)
    parser.add_argument('--count', type=int, default=100)
    args = parser.parse_args()
    rust = args.rust.resolve()
    nova = args.selfhost.resolve()
    if not rust.is_file() or not nova.is_file():
        parser.error('compile the two executables first (cargo build && cargo run -- bootstrap-selfhost)')
    tested = 0
    with tempfile.TemporaryDirectory(prefix='nova-differential-') as temp:
        tmp = Path(temp)
        for name, code, expected in corpus(args.seed, args.count):
            source = tmp / name
            source.write_text(code, encoding='utf8')
            cst = tmp / 'result.cst'
            if cst.exists():
                cst.unlink()
            old = subprocess.run([str(rust),'parse-rust',str(source)], capture_output=True,text=True)
            new = subprocess.run([str(nova)],env={**os.environ,'NOVA_SOURCE':str(source),'NOVA_CST_PATH':str(cst)},capture_output=True,text=True)
            accepted_old = old.returncode == 0
            accepted_new = new.returncode == 0
            if accepted_old != accepted_new or (expected and not accepted_old) or (not expected and accepted_old):
                print(f'FAIL {name}: expected={expected} rust={accepted_old} nova={accepted_new}')
                print('Rust:',old.stdout,old.stderr)
                print('Nova:',new.stdout,new.stderr)
                print('Source:\n',code)
                print('seed:',args.seed)
                return 1
            if accepted_new:
                if not cst.is_file():
                    print(f'FAIL {name}: Nova parser accepted but did not write a CST')
                    return 1
                try:
                    load(source,cst)
                except (CSTError,ValueError,OSError) as err:
                    print(f'FAIL {name}: invalid CST-V1: {err}')
                    return 1
            tested += 1
    print(f'OK differential syntax: {tested} cases (seed={args.seed}, generated={args.count})')
    return 0

if __name__ == '__main__':
    raise SystemExit(main())
