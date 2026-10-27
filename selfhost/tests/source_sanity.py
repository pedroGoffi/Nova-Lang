#!/usr/bin/env python3
"""Non-compiling structural check; actual integration needs cargo and clang."""
from pathlib import Path
import re

root = Path(__file__).resolve().parents[1]
files = sorted((root / 'src').glob('*.nova'))
assert {p.name for p in files} == {'main.nova', 'lexer.nova', 'parser.nova'}

for file in files:
    data = file.read_text(encoding='utf-8')
    stack = []
    i = 0
    line = 1
    while i < len(data):
        char = data[i]
        if data.startswith('//', i):
            next_line = data.find('\n', i)
            if next_line < 0:
                break
            i = next_line
            continue
        if char == '"':
            start = line
            i += 1
            while i < len(data):
                if data[i] == '\\':
                    i += 2
                    continue
                if data[i] == '"':
                    break
                if data[i] == '\n':
                    line += 1
                i += 1
            else:
                raise AssertionError(f'{file}:{start}: unterminated string')
            i += 1
            continue
        if char in '{([':
            stack.append((char, line))
        elif char in '})]':
            match = {'}': '{', ')': '(', ']': '['}[char]
            assert stack and stack[-1][0] == match, (file, line, char, stack[-3:])
            stack.pop()
        if char == '\n':
            line += 1
        i += 1
    assert not stack, (file, stack)
    # `else if` is supported in stage-0 and stage-1 from 0.9 onward.
    functions = re.findall(r'\b(?:pub\s+)?fn\s+([A-Za-z_]\w*)\s*\(', data)
    assert len(functions) == len(set(functions)), (file, 'duplicate functions')
    print(f'{file.relative_to(root)}: {len(functions)} functions, delimiters balanced')

parser = (root / 'src' / 'parser.nova').read_text()
for name in ('parse_program','expression','unary','postfix','atom','parse_type','pattern',
             'block','statement','struct_item','enum_item','function_item','extern_item',
             'impl_item','attribute','node_child'):
    assert re.search(r'\bfn\s+' + name + r'\s*\(', parser), name
print('Static sanity checks passed (NOT a compilation test).')

# Module visibility regression: parser::create accepts lexer::Scan, so
# lexer must be exported from the root for a sibling module to reference it.
main = (root / 'src' / 'main.nova').read_text()
lexer = (root / 'src' / 'lexer.nova').read_text()
assert re.search(r'^pub\s+mod\s+lexer\s*;', main, re.M), 'lexer module must be public'
assert re.search(r'^pub\s+struct\s+Scan\b', lexer, re.M), 'Scan must be public'
assert re.search(r'\bpub\s+fn\s+create\s*\(scan\s*:\s*lexer::Scan\)', parser), 'parser must accept Scan'
print('Cross-module visibility regression: OK')

# Lexer/parser parity: attributes can precede pub on structs/enums.
assert 'if !public_item && has_attributes && take(p,"pub")' in parser
assert 'if has_attributes && !look(p,"struct") && !look(p,"class") && !look(p,"enum")' in parser
grammar = (root.parent / 'src' / 'grammar.lalrpop').read_text()
assert '<attributes:Attribute+> "pub" <s:StructDef>' in grammar
assert '<attributes:Attribute+> "pub" <e:EnumDef>' in grammar
fixture = (root / 'tests' / 'valid_attribute_visibility.nova').read_text()
assert '#[derive(Serialize, Clone)]\npub struct Visible' in fixture
assert '#[derive(Debug)] pub enum Status' in fixture
assert 'pub #[derive(Clone)] struct Legacy' in fixture
print('Attribute / visibility parser parity structural regression: OK')
