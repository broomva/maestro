#!/usr/bin/env python3
"""Exercise the reader ratchet in an isolated git fixture, including a dead-check control.

Each test builds a small repository in a temporary directory, copies the checker into it and runs
it there, so nothing here reads or changes this checkout. The folder names are spelled in two
halves so that this file is not itself a reader.
"""
import importlib.util
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


def load_checker():
    spec = importlib.util.spec_from_file_location("check_control_readers", SOURCE / CHECKER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


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

    def commit(self, message):
        self.stage()
        subprocess.run(["git", "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                        "-c", "commit.gpgsign=false", "commit", "-q", "--no-verify", "-m", message],
                       cwd=self.root, check=True, env=ENV)

    def base(self):
        """Commit the fixture as the base branch and tag it, as CI's PR base."""
        self.commit("base")
        subprocess.run(["git", "tag", "base"], cwd=self.root, check=True, env=ENV)

    def regenerate_baseline(self):
        listed = self.check("--print-baseline")
        self.assertEqual(listed.returncode, 0, listed.stdout + listed.stderr)
        (self.root / "scripts/control_readers_baseline.json").write_text(listed.stdout)

    def check(self, *args):
        return subprocess.run([sys.executable, f"scripts/{CHECKER}", *args],
                              cwd=self.root, capture_output=True, text=True, env=ENV)

    def test_fixture_starts_at_its_baseline(self):
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f"Files mentioning {OLD}: 1; baseline: 1", result.stdout)

    def test_added_rust_reader_fails_and_names_file(self):
        self.write("crates/mcp/src/new.rs", f'let policy = root.join("{OLD}/policy.yaml");\n')
        self.stage()
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("  crates/mcp/src/new.rs\n", result.stdout)
        self.assertNotIn("  crates/store/src/old.rs\n", result.stdout)
        self.assertIn(f"Files mentioning {OLD}: 2; baseline: 1", result.stdout)
        # The hint states when the base comparison is skipped.
        self.assertIn("base has no\nbaseline file yet", result.stdout)

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

    def test_swapping_one_reader_for_another_fails(self):
        # The count stays at the baseline, but new.rs was never in it.
        self.write("crates/store/src/old.rs", "fn main() {}\n")
        self.write("crates/mcp/src/new.rs", f'let policy = root.join("{OLD}/policy.yaml");\n')
        self.stage()
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("  crates/mcp/src/new.rs\n", result.stdout)
        self.assertIn(f"Files mentioning {OLD}: 1; baseline: 1", result.stdout)

    def test_prose_and_fixtures_are_not_readers(self):
        self.write("README.md", f"Asks live in `{OLD}/asks`.\n")
        self.write("fixtures/machine/asks.yaml", f"path: {OLD}/asks\n")
        self.write("docs/adrs/layout.rs", f'let asks = root.join("{OLD}/asks");\n')
        self.stage()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f"Files mentioning {OLD}: 1; baseline: 1", result.stdout)

    def test_directory_ending_in_a_skipped_name_is_code(self):
        # The round-4 reproduction: the skipped directory names are whole path components, not
        # substrings, so `mydocs/` is an ordinary code directory.
        for path in ("scripts/mydocs/probe.py", "crates/mydocs/src/probe.rs"):
            with self.subTest(path=path):
                self.write(path, f'p = "{OLD}/asks"\n')
                self.stage()
                result = self.check()
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn(f"  {path}\n", result.stdout)
                (self.root / path).unlink()
                self.stage()

    def test_skipped_directories_are_whole_components(self):
        skipped = ("docs/x.rs", "crates/a/docs/x.rs", "a/research/x.py", "node_modules/x.js",
                   f"{OLD}/x.py", f"{NEW}/x.py", "a/archive/x.rs")
        counted = ("scripts/research_notes/x.py", "archived/x.rs", "my.control/x.py",
                   "crates/mydocs/src/x.rs", "docsx/x.py")
        for path in skipped + counted:
            self.write(path, f'p = "{OLD}/asks"\n')
        self.stage()
        listed = json.loads(self.check("--print-baseline").stdout)["readers"]
        for path in skipped:
            self.assertNotIn(path, listed)
        for path in counted:
            self.assertIn(path, listed)

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

    def test_regenerated_baseline_with_new_reader_fails_against_base(self):
        # The round-1 reproduction: a new reader plus a baseline regenerated in the same change.
        self.base()
        self.write("actual.py", f'p = "{OLD}/asks"\n')
        self.stage()
        self.regenerate_baseline()
        self.commit("add a reader and grow the baseline")
        self.assertEqual(self.check().returncode, 0)  # without the base, the PR's own baseline agrees
        result = self.check("--base-ref", "base")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("Paths added to the baseline since base:\n  actual.py\n", result.stdout)
        self.assertIn("skipped only when the base has no baseline file yet", result.stdout)

    def test_removed_baseline_entry_passes_against_base(self):
        self.base()
        self.write("crates/store/src/old.rs", "fn main() {}\n")
        self.stage()
        self.regenerate_baseline()
        self.commit("fix the reader")
        result = self.check("--base-ref", "base")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Baseline: no path added since base.", result.stdout)

    def test_renamed_reader_passes_against_base(self):
        self.base()
        subprocess.run(["git", "mv", "crates/store/src/old.rs", "crates/store/src/moved.rs"],
                       cwd=self.root, check=True, env=ENV)
        self.regenerate_baseline()
        self.commit("rename")
        result = self.check("--base-ref", "base")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("crates/store/src/moved.rs replaces crates/store/src/old.rs", result.stdout)

    def test_swap_that_is_not_a_rename_fails_against_base(self):
        # A removed entry pays for an added one only when git calls the added file its rename.
        self.base()
        self.write("crates/store/src/old.rs", "fn main() {}\n")
        self.write("crates/mcp/src/other.rs", f'let p = root.join("{OLD}/policy.yaml");\nfn other() {{}}\n')
        self.stage()
        self.regenerate_baseline()
        self.commit("swap")
        result = self.check("--base-ref", "base")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("  crates/mcp/src/other.rs\n", result.stdout)

    def test_rename_pays_only_for_its_own_new_path(self):
        # The baseline drops old.rs, which was renamed to moved.rs and fixed; that rename does not
        # pay for a different file, other.rs, joining the baseline.
        body = "".join(f"fn f{i}() {{}}\n" for i in range(40))
        self.write("crates/store/src/old.rs", f'let asks = root.join("{OLD}/asks");\n' + body)
        self.base()
        subprocess.run(["git", "mv", "crates/store/src/old.rs", "crates/store/src/moved.rs"],
                       cwd=self.root, check=True, env=ENV)
        self.write("crates/store/src/moved.rs", body)
        self.write("crates/mcp/src/other.rs", f'let p = root.join("{OLD}/policy.yaml");\n')
        self.stage()
        self.regenerate_baseline()
        self.commit("rename one file, add another reader")
        result = self.check("--base-ref", "base")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("  crates/mcp/src/other.rs\n", result.stdout)

    def test_first_introduction_skips_comparison(self):
        # The base has no baseline file yet (the change that introduces it): skipped with a note.
        (self.root / "scripts/control_readers_baseline.json").unlink()
        self.base()
        self.write("scripts/control_readers_baseline.json",
                   json.dumps({"count": 1, "readers": ["crates/store/src/old.rs"]}))
        self.commit("introduce the baseline")
        result = self.check("--base-ref", "base")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("base has no scripts/control_readers_baseline.json; the baseline comparison is skipped",
                      result.stdout)

    def test_no_base_ref_says_comparison_skipped(self):
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("no --base-ref; the baseline comparison is skipped", result.stdout)

    def test_unknown_base_ref_cannot_run(self):
        self.base()
        result = self.check("--base-ref", "no-such-ref")
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)

    def test_constructed_path_is_not_counted(self):
        # A documented limit, pinned so that changing it is deliberate: the scan is literal, and a
        # path assembled from pieces is not found.
        self.write("bypass.rs", 'fn f() { let p = std::path::PathBuf::from(format!("{}{}", ".", "control/asks")); }\n')
        self.stage()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f"Files mentioning {OLD}: 1; baseline: 1", result.stdout)

    def test_comment_counts_as_reader(self):
        # A documented limit: a comment naming the legacy folder counts.
        self.write("crates/mcp/src/note.rs", f"// migration note: {OLD}/asks\nfn f() {{}}\n")
        self.stage()
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("a comment\ncounts", result.stdout)

    def test_file_naming_both_folders_is_listed(self):
        # Nothing is paired: naming the new folder as well does not take a file off the list.
        self.write("crates/mcp/src/both.rs",
                   f'let new = root.join("{NEW}/asks");\nlet old = root.join("{OLD}/asks");\n')
        self.stage()
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("  crates/mcp/src/both.rs\n", result.stdout)
        self.assertIn("generated path reader", result.stdout)

    def test_reviewer_strings(self):
        # The strings review rounds used against the old pairing rule, here and in broomva/bstack,
        # and what each is now: listed exactly when it mentions the legacy folder.
        cases = {
            "r4_mydocs.py": (f'p = "{OLD}/asks"\n', True),
            "r5_double_dot.py": (f'a = "{OLD}/asks"; b = "my.{NEW}/asks"\n', True),
            "r5_double_dot_legacy.py": (f'a = "my.{OLD}/asks"\n', True),
            "r5_repo_dash_home.py": (f'a = "{OLD}/asks"; b = "REPO-HOME/{NEW}/asks"\n', True),
            "r5_tmp_tilde.py": (f'a = "{OLD}/asks"; b = "/tmp/~/{NEW}/asks"\n', True),
            "bstack_r3_area.py": (f'a = "{OLD}/asks/private"; b = "{NEW}/asks/unrelated"\n', True),
            "bstack_r5_plus_suffix.py": (f'p = "{OLD}/asks+old"; q = "{NEW}/asks+new"\n', True),
            "bstack_r5_plus_left.py": (f'p = "my+{OLD}/asks"\n', True),
            "bstack_r5_somehome.py": (f'p = "{OLD}/asks"; q = "SOMEHOME/{NEW}/asks"\n', True),
            "member_access.py": (f"x = self{OLD}\ny = f(){OLD}/2\n", False),
            "longer_name.py": (f'import x{OLD}ler\np = "{OLD}ler/x"\n', False),
            "new_folder_only.py": (f'p = "{NEW}/asks"\n', False),
        }
        for name, (text, _) in cases.items():
            self.write(f"probe/{name}", text)
        self.write(f"{OLD}led/x.py", f'p = "{OLD}/asks"\n')
        self.stage()
        listed = set(json.loads(self.check("--print-baseline").stdout)["readers"])
        for name, (_, want) in cases.items():
            with self.subTest(name=name):
                self.assertEqual(f"probe/{name}" in listed, want)
        self.assertIn(f"{OLD}led/x.py", listed)

    def test_mention_pattern_generated(self):
        # Every left context crossed with every right context, built from the rule, not from the
        # pattern: a mention is a dot at the text start or after anything but a letter, digit,
        # `_`, `)` or `]`, followed by "/", a quote, a backtick or the end of the line.
        left_no = {"letter": "a", "upper": "Z", "digit": "7", "underscore": "_", "paren": ")",
                   "bracket": "]"}
        left_yes = {"start": "", "slash": "/", "single": "'", "double": '"', "backtick": "`",
                    "space": " ", "tab": "\t", "newline": "x\n", "equals": "=", "open": "(",
                    "comma": ",", "dot": "."}
        right_yes = {"slash": "/asks", "single": "'", "double": '"', "backtick": "`",
                     "eol": "\nnext", "eot": ""}
        right_no = {"letter": "ler", "digit": "2", "underscore": "_x", "open": "(", "dot": ".x"}
        module = load_checker()
        cases = 0
        for lname, lchar in {**left_no, **left_yes}.items():
            for rname, rtext in {**right_yes, **right_no}.items():
                text = f"{lchar}{OLD}{rtext}"
                want = lname in left_yes and rname in right_yes
                with self.subTest(left=lname, right=rname, text=text):
                    self.assertEqual(module.mentions(text), want)
                cases += 1
        self.assertEqual(cases, (len(left_no) + len(left_yes)) * (len(right_yes) + len(right_no)))

    def test_classifier_controls(self):
        result = self.check("--selftest")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertRegex(result.stdout, r"selftest: (\d+)/\1\n")


if __name__ == "__main__":
    unittest.main()
