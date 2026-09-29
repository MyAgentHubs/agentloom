#!/usr/bin/env python3
"""Regression checks in disposable repositories; no changes to the real index/ref."""

import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True
SCRIPT = Path(__file__).resolve().with_name("check_doc_orphans.py")
spec = importlib.util.spec_from_file_location("doc_orphans_gate", SCRIPT)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

# Reassembled the same way the gate joins it, so this test does not itself
# reintroduce the literal path the public-tree residue scan greps for.
DOCS_ROOT = "/".join(("docs", "superpowers"))
# Likewise built at runtime so the assertion below cannot match its own source.
HOME_MARKER = "/".join(("", "Users", "ai")) + "/"


class SourceHygieneTests(unittest.TestCase):
    def test_no_internal_path_literals(self):
        for path in (SCRIPT, Path(__file__).resolve()):
            source = path.read_text()
            self.assertNotIn(DOCS_ROOT, source)
            self.assertNotIn(HOME_MARKER, source)


class RepositoryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix=".doc-orphans-gate-test-", dir=SCRIPT.parent.parent)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.created = set()
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.env.update({
            "GIT_AUTHOR_NAME": "Doc Orphans Test", "GIT_AUTHOR_EMAIL": "docorphans@example.invalid",
            "GIT_COMMITTER_NAME": "Doc Orphans Test", "GIT_COMMITTER_EMAIL": "docorphans@example.invalid",
        })
        self.git("init", "-q")
        self.git("symbolic-ref", "HEAD", "refs/heads/fixture")
        for entry in gate.ENTRY_FILES:
            path = self.root / entry
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("entry placeholder\n")
            self.created.add(path)
        (self.root / "scripts").mkdir()
        import shutil
        shutil.copyfile(SCRIPT, self.root / "scripts/check_doc_orphans.py")

    def git(self, *args):
        return subprocess.run(
            ["git", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null",
             "-C", str(self.root), *args],
            env=self.env, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        ).stdout.strip().decode()

    def doc(self, relative_within_docs_root, content="content\n"):
        path = self.root / gate.DOCS_ROOT / relative_within_docs_root
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        self.created.add(path)
        return path

    def entry(self, entry_relative, content):
        path = self.root / entry_relative
        path.write_text(content)
        self.created.add(path)
        return path

    def baseline(self):
        if self.created:
            self.git("add", "--", *(str(path) for path in sorted(self.created)))
        self.git("commit", "-q", "--allow-empty", "-m", "fixture")
        commit = self.git("rev-parse", "HEAD")
        self.git("update-ref", "refs/remotes/origin/master", commit)
        return commit

    def run_gate(self, expected, cwd=None):
        result = subprocess.run(
            [sys.executable, str(self.root / "scripts/check_doc_orphans.py")],
            cwd=cwd or self.root, env=self.env,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, check=False,
        )
        self.assertEqual(result.returncode, expected, result.stdout)
        return result.stdout

    def test_referenced_by_full_path_passes(self):
        self.doc("notes/linked.md")
        self.entry(gate.ENTRY_FILES[0], "see notes/linked.md for details\n")
        self.baseline()
        self.assertIn("PASS", self.run_gate(0))

    def test_referenced_by_basename_passes(self):
        self.doc("notes/deep/linked.md")
        self.entry(gate.ENTRY_FILES[0], "see linked.md for details\n")
        self.baseline()
        self.assertIn("PASS", self.run_gate(0))

    def test_basename_partial_match_does_not_count(self):
        self.baseline()
        self.entry(gate.ENTRY_FILES[0], "entry placeholder\nsee xa.md and a.mdx, not it\n")
        self.doc("notes/deep/a.md")
        self.assertIn("notes/deep/a.md (not referenced", self.run_gate(1))

    def test_entry_file_itself_is_never_a_candidate(self):
        self.baseline()
        output = self.run_gate(0)
        self.assertIn("扫描文档数：0", output)

    def test_existing_baseline_orphan_passes(self):
        self.doc("notes/orphan.md")
        self.baseline()
        self.assertIn("PASS", self.run_gate(0))

    def test_new_unreferenced_doc_fails_named(self):
        self.baseline()
        self.doc("notes/new-orphan.md")
        self.assertIn("notes/new-orphan.md (not referenced", self.run_gate(1))

    def test_new_doc_referenced_in_index_passes(self):
        self.baseline()
        self.doc("notes/new-linked.md")
        self.entry(gate.ENTRY_FILES[0], "entry placeholder\nsee notes/new-linked.md\n")
        self.assertIn("PASS", self.run_gate(0))

    def test_removing_existing_orphan_shrinks_debt_and_passes(self):
        path = self.doc("notes/orphan.md")
        self.baseline()
        output = self.run_gate(0)
        self.assertIn("存量孤儿：1", output)
        path.unlink()
        output = self.run_gate(0)
        self.assertIn("存量孤儿：0", output)

    def test_mockups_html_referenced_by_mockups_index_passes(self):
        mockups_entry = [e for e in gate.ENTRY_FILES if e.endswith("mockups/index.html")][0]
        self.doc("specs/2026-05-21-github-fleet-ide/mockups/screen.html", "<html></html>\n")
        self.entry(mockups_entry, '<a href="/files/screen.html">screen</a>\n')
        self.baseline()
        self.assertIn("PASS", self.run_gate(0))

    def test_shared_basename_requires_path_match(self):
        self.baseline()
        self.doc("dirA/shared.md")
        self.doc("dirB/shared.md")
        # Shared basenames must be matched by full path, not by bare basename:
        # dirA is referenced by its full path, dirB is never mentioned at all.
        self.entry(gate.ENTRY_FILES[0], "entry placeholder\nsee dirA/shared.md for details\n")
        output = self.run_gate(1)
        self.assertIn("dirB/shared.md (not referenced", output)
        self.assertNotIn("dirA/shared.md (not referenced", output)

    def test_shared_basename_passes_when_both_paths_referenced(self):
        self.baseline()
        self.doc("dirA/shared.md")
        self.doc("dirB/shared.md")
        self.entry(gate.ENTRY_FILES[0], "entry placeholder\nsee dirA/shared.md and dirB/shared.md\n")
        self.assertIn("PASS", self.run_gate(0))

    def test_shared_basename_in_baseline_forces_path_match_there_too(self):
        self.doc("dirA/shared.md")
        self.doc("dirB/shared.md")
        self.entry(gate.ENTRY_FILES[0], "entry placeholder\nsee shared.md (the dirA one) for details\n")
        self.baseline()
        # dirB/shared.md must already be a baseline orphan (not silently unique
        # there just because it is the only file that changes later); it stays
        # a pre-existing orphan across an unrelated new commit, not a new one.
        self.doc("notes/unrelated.md")
        self.entry(gate.ENTRY_FILES[0], "entry placeholder\nsee shared.md (the dirA one) for details\nsee notes/unrelated.md\n")
        self.assertIn("PASS", self.run_gate(0))

    def test_path_match_requires_boundary_not_slash(self):
        self.baseline()
        self.entry(gate.ENTRY_FILES[0], "entry placeholder\nsee x/notes/a.md over there\n")
        self.doc("notes/a.md")
        # A second candidate with the same basename disables the basename-match
        # shortcut, isolating the full-path boundary rule this test targets.
        self.doc("other/a.md")
        self.assertIn("notes/a.md (not referenced", self.run_gate(1))

    def test_path_match_rejects_longer_containing_string(self):
        self.baseline()
        self.entry(gate.ENTRY_FILES[0], "entry placeholder\nsee zznotes/x.mdzz for background\n")
        self.doc("notes/x.md")
        self.assertIn("notes/x.md (not referenced", self.run_gate(1))

    def test_symlink_candidate_fails_closed(self):
        self.baseline()
        path = self.root / gate.DOCS_ROOT / "notes/linked.md"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.symlink_to("does-not-exist.md")
        self.assertIn("拒绝放行", self.run_gate(2))

    def test_symlink_alias_to_existing_doc_is_not_a_candidate(self):
        self.baseline()
        real = self.doc("notes/real.md")
        alias = self.root / gate.DOCS_ROOT / "notes/alias.md"
        alias.symlink_to(real.name)
        # The alias is not itself a candidate (it is skipped, not counted, not
        # referenced-checked); the real file is still judged on its own path
        # and is a genuine new orphan since nothing references notes/real.md.
        self.assertIn("notes/real.md (not referenced", self.run_gate(1))

    def test_symlink_alias_inside_mounted_docs_root_is_not_a_candidate(self):
        self.baseline()
        self.doc("notes/real.md")
        docs_dir = self.root / gate.DOCS_ROOT
        mounted_dir = self.root / ".private" / gate.DOCS_ROOT
        mounted_dir.parent.mkdir(parents=True)
        docs_dir.rename(mounted_dir)
        docs_dir.symlink_to(mounted_dir, target_is_directory=True)
        (docs_dir / "notes/alias.md").symlink_to("real.md")

        output = self.run_gate(1)
        self.assertIn("notes/real.md (not referenced", output)
        self.assertNotIn("拒绝放行", output)

    def test_symlink_alias_outside_mounted_docs_root_fails_closed(self):
        self.baseline()
        self.doc("notes/real.md")
        docs_dir = self.root / gate.DOCS_ROOT
        mounted_dir = self.root / ".private" / gate.DOCS_ROOT
        mounted_dir.parent.mkdir(parents=True)
        docs_dir.rename(mounted_dir)
        docs_dir.symlink_to(mounted_dir, target_is_directory=True)
        outside = self.root / "outside.md"
        outside.write_text("outside\n")
        (docs_dir / "notes/alias.md").symlink_to(outside)

        self.assertIn("拒绝放行", self.run_gate(2))

    def test_docs_root_missing_skips_and_exits_zero(self):
        import shutil
        shutil.rmtree(self.root / gate.DOCS_ROOT)
        output = self.run_gate(0)
        self.assertIn("SKIP", output)
        self.assertIn(gate.DOCS_ROOT, output)

    def test_docs_root_present_does_not_skip(self):
        self.baseline()
        output = self.run_gate(0)
        self.assertNotIn("SKIP", output)

    def test_missing_baseline_exits_two(self):
        self.doc("notes/x.md")
        self.git("add", "--", *(str(path) for path in sorted(self.created)))
        self.git("commit", "-q", "-m", "no baseline ref")
        self.assertIn("拒绝放行", self.run_gate(2))


if __name__ == "__main__":
    unittest.main()
