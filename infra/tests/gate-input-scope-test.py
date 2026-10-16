"""Exercise the gate's actual input scanner in hermetic source trees."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
GATE = (ROOT / 'infra/gate.sh').read_text()
MARKER = next(line for line in GATE.splitlines() if line.startswith('TREE_WIDE_FILE_PATTERN='))
SCANNER = MARKER + '\nfile_input_index() {' + GATE.split('file_input_index() {', 1)[1].split('\n}\n', 1)[0] + '\n}\n'


class InputScope(unittest.TestCase):
    def scan(self, readers):
        with tempfile.TemporaryDirectory(prefix='boss-gate-input-scope-') as temporary:
            root = Path(temporary)
            document = root / 'docs/record.md'
            document.parent.mkdir()
            document.write_text('record\n')
            for name, body in readers.items():
                path = root / 'crates/core/sample' / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(body)
            result = subprocess.run(['bash'], input=SCANNER + '\nfile_input_index\n',
                                    text=True, cwd=root, capture_output=True,
                                    env={**os.environ, 'LC_ALL': 'C'}, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            return result.stdout.splitlines()

    def test_declared_tree_wide_reader_does_not_select_its_entire_crate(self):
        self.assertEqual(self.scan({'tests/global.rs':
            '//! tree-wide pin — every scoped gate runs this whole target\n'
            'fn check() { read("docs/record.md"); }\n'}), [])

    def test_ordinary_document_reader_still_selects_its_crate(self):
        self.assertEqual(self.scan({'tests/local.rs':
            'fn check() { read("docs/record.md"); }\n'}), ['docs/record.md sample'])

    def test_function_marker_in_production_is_not_a_whole_target_declaration(self):
        self.assertEqual(self.scan({'src/lib.rs':
            '// tree-wide pin — only one function is filtered, not the entire target\n'
            'const SOURCE: &str = "docs/record.md";\n'}), ['docs/record.md sample'])

    def test_other_reader_preserves_crate_scope_when_a_global_reader_also_exists(self):
        self.assertEqual(self.scan({
            'tests/global.rs': '//! tree-wide pin — entire target\nfn check() { read("docs/record.md"); }\n',
            'tests/local.rs': 'fn check() { read("docs/record.md"); }\n',
        }), ['docs/record.md sample'])


if __name__ == '__main__':
    unittest.main()
