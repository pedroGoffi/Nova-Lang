#!/usr/bin/env python3
"""Pure-Python schema validation tests; no Rust/Nova toolchain required."""
import tempfile
import unittest
from pathlib import Path
from cst_v1 import load, CSTError

VALID = '''NOVA-CST-V1
TOKENS 2
T 0 1 0 2 1 1
T 1 6 2 2 1 3
NODES 1
N 1 1 0 1 0
ROOT 1
'''

class CSTV1Tests(unittest.TestCase):
    def fixture(self, content: str) -> tuple[Path, Path]:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        source = Path(tmp.name, 'source.nova')
        tree = Path(tmp.name, 'tree.cst')
        source.write_text('fn',encoding='utf-8')
        tree.write_text(content,encoding='utf-8')
        return source, tree

    def test_accepts_structurally_valid_tree(self):
        tree = load(*self.fixture(VALID))
        self.assertEqual(tree.root, 1)
        self.assertEqual(len(tree.tokens), 2)

    def test_rejects_version_mismatch(self):
        with self.assertRaises(CSTError):
            load(*self.fixture(VALID.replace('NOVA-CST-V1','NOVA-CST-V0')))

    def test_rejects_bad_token_span(self):
        with self.assertRaises(CSTError):
            load(*self.fixture(VALID.replace('T 0 1 0 2','T 0 1 0 3')))

    def test_rejects_missing_eof(self):
        with self.assertRaises(CSTError):
            load(*self.fixture(VALID.replace('T 1 6','T 1 1')))

    def test_rejects_child_cycle(self):
        with self.assertRaises(CSTError):
            load(*self.fixture(VALID.replace('N 1 1 0 1 0','N 1 1 0 1 1 1')))

    def test_rejects_unreachable_node(self):
        with self.assertRaises(CSTError):
            load(*self.fixture(VALID.replace('NODES 1','NODES 2').replace('ROOT 1','N 2 38 0 1 0\nROOT 1')))

if __name__ == '__main__':
    unittest.main()
