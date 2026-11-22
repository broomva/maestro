#!/usr/bin/env python3
"""Exercise the reader ratchet in an isolated git fixture, including a dead-check control.

Each test builds a small repository in a temporary directory, copies the checker into it and runs
it there, so nothing here reads or changes this checkout. The folder names are spelled in two
halves so that this file is not itself a reader.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SOURCE = Path(__file__).resolve().parent
CHECKER = "check_control_readers.py"
OLD = "." + "control"
NEW = "." + "broomva"
# Run from a git hook, these would point the fixture repository at the hook's repository.
ENV = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}


class RatchetTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        scripts = self.root / "scripts"
        scripts.mkdir()
        shutil.copy(SOURCE / CHECKER, scripts / CHECKER)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True, env=ENV)
        self.write("crates/store/src/old.rs", f'let asks = root.join("{OLD}/asks");\n')
        (scripts / "control_readers_baseline.json").write_text(
            json.dumps({"count": 1, "readers": ["crates/store/src/old.rs"]})
        )
        self.stage()

    def write(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def stage(self):
        subprocess.run(["git", "add", "-A"], cwd=self.root, check=True, env=ENV)

    def check(self, *args):
        return subprocess.run([sys.executable, f"scripts/{CHECKER}", *args],
                              cwd=self.root, capture_output=True, text=True, env=ENV)

    def test_fixture_starts_at_its_baseline(self):
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Unpaired readers: 1; baseline: 1", result.stdout)

    def test_added_rust_reader_fails_and_names_file(self):
        self.write("crates/mcp/src/new.rs", f'let policy = root.join("{OLD}/policy.yaml");\n')
        self.stage()
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("  crates/mcp/src/new.rs\n", result.stdout)
        self.assertNotIn("  crates/store/src/old.rs\n", result.stdout)
        self.assertIn("Unpaired readers: 2; baseline: 1", result.stdout)

    def test_added_script_reader_fails_and_names_file(self):
        self.write("scripts/new.sh", f'cat "$ROOT/{OLD}/policy.yaml"\n')
        self.stage()
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("  scripts/new.sh\n", result.stdout)

    def test_removed_reader_passes_and_reports_lower_count(self):
        self.write("crates/store/src/old.rs", "fn main() {}\n")
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("fell to 0", result.stdout)
        self.assertIn("  crates/store/src/old.rs\n", result.stdout)

    def test_paired_reader_passes(self):
        # Both names with the same suffix: the file handles the rename, so it is not listed.
        self.write("crates/mcp/src/new.rs",
                   f'let new = root.join("{NEW}/policy.yaml");\nlet old = root.join("{OLD}/policy.yaml");\n')
        self.stage()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Unpaired readers: 1; baseline: 1", result.stdout)

    def test_both_names_with_different_suffixes_fails(self):
        # Naming the new folder is not enough: the legacy path itself must have its twin.
        self.write("crates/mcp/src/new.rs",
                   f'let new = root.join("{NEW}/flows");\nlet old = root.join("{OLD}/policy.yaml");\n')
        self.stage()
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("  crates/mcp/src/new.rs\n", result.stdout)

    def test_swapping_one_reader_for_another_fails(self):
        # The count stays at the baseline, but new.rs was never in it.
        self.write("crates/store/src/old.rs", "fn main() {}\n")
        self.write("crates/mcp/src/new.rs", f'let policy = root.join("{OLD}/policy.yaml");\n')
        self.stage()
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("  crates/mcp/src/new.rs\n", result.stdout)
        self.assertIn("Unpaired readers: 1; baseline: 1", result.stdout)

    def test_prose_and_fixtures_are_not_readers(self):
        self.write("README.md", f"Asks live in `{OLD}/asks`.\n")
        self.write("fixtures/machine/asks.yaml", f"path: {OLD}/asks\n")
        self.write("docs/adrs/layout.rs", f'let asks = root.join("{OLD}/asks");\n')
        self.stage()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Unpaired readers: 1; baseline: 1", result.stdout)

    def test_baseline_count_must_match_its_list(self):
        (self.root / "scripts/control_readers_baseline.json").write_text(
            json.dumps({"count": 5, "readers": ["crates/store/src/old.rs"]})
        )
        result = self.check()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("baseline count does not match", result.stderr)

    def test_print_baseline_lists_the_checkout(self):
        self.write("crates/mcp/src/new.rs", f'let policy = root.join("{OLD}/policy.yaml");\n')
        self.stage()
        result = self.check("--print-baseline")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(json.loads(result.stdout),
                         {"count": 2, "readers": ["crates/mcp/src/new.rs", "crates/store/src/old.rs"]})

    def test_comparison_is_live(self):
        # The dead-check control: with the comparison disabled, the added reader passes. So the
        # failures above come from the comparison, not from something else in the fixture.
        self.write("crates/mcp/src/new.rs", f'let policy = root.join("{OLD}/policy.yaml");\n')
        self.stage()
        checker = self.root / "scripts" / CHECKER
        original = checker.read_text()
        self.assertEqual(original.count("    if new:\n"), 1)
        checker.write_text(original.replace("    if new:\n", "    if False:\n", 1))
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_classifier_controls(self):
        result = self.check("--selftest")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertRegex(result.stdout, r"selftest: (\d+)/\1\n")


if __name__ == "__main__":
    unittest.main()
