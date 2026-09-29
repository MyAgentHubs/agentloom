#!/usr/bin/env python3
"""Regression checks for scripts/check_oss_residue.sh.

Default mode scans all tracked files except PRIVATE_PATHS, with PRIVATE_EXCEPT
restoring public fixtures to the scan. Public-tree mode also reports tracked
private paths. Tests exercise both modes in disposable temporary git repositories
and verify that every private exception lies under a private path.
"""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parent.parent
RESIDUE_SCRIPT = ROOT / "scripts/check_oss_residue.sh"

# Concatenated so this file's own source never spells these strings out
# contiguously (mirrors the same rule check_oss_residue.sh follows, and
# keeps this file itself from tripping the very gate it tests once it is a
# tracked file rather than a scratch fixture).
HOME_LEAK = "/Users/" + "ai/"
DOC_PATH_LEAK = "docs/" + "superpowers"
PRIVATE_REPO_LEAK = "github-" + "coding-agent"
COMPANY_LEAK = "after" + "ship"
PERSONAL_LEAK = "panda" + "withai"


def get_debug_entries(label):
    """Ask the real check_oss_residue.sh (running against this repo) for one
    of its resolved private-path arrays, so guard tests compare against
    actual runtime values rather than re-parsing bash literals. The
    introspection dump goes to stderr alongside a normal scan, whose exit
    code is ignored here."""
    result = subprocess.run(
        ["bash", str(RESIDUE_SCRIPT)],
        cwd=ROOT, env={**os.environ, "OSS_RESIDUE_DEBUG_ARRAYS": "1"},
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=False,
    )
    entries = [chunk for chunk in result.stderr.split("\x1e") if chunk]
    values = []
    for chunk in entries:
        chunk_label, value = chunk.split("\x1f", 1)
        if chunk_label == label:
            values.append(value)
    return values


def keep_covers(keep_entries, path):
    for entry in keep_entries:
        if entry == path:
            return True
        if entry.endswith("/*"):
            base = entry[:-2]
            if path == base or path.startswith(base + "/"):
                return True
    return False


def normalize_private_path(entry):
    return entry[:-2] if entry.endswith("/*") else entry


class PrivatePathsDriftGuardTests(unittest.TestCase):
    """PRIVATE_EXCEPT entries must remain nested under PRIVATE_PATHS."""

    def test_every_private_except_is_nested_under_a_private_path(self):
        private_paths = get_debug_entries("PRIVATE_PATHS")
        private_except = get_debug_entries("PRIVATE_EXCEPT")
        self.assertTrue(private_except)
        for entry in private_except:
            base = normalize_private_path(entry)
            self.assertTrue(keep_covers(private_paths, base), f"{entry} not nested under PRIVATE_PATHS")


class RepositoryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix=".oss-residue-test-", dir=ROOT)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.created = set()
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.env.update({
            "GIT_AUTHOR_NAME": "Residue Gate Test", "GIT_AUTHOR_EMAIL": "residue@example.invalid",
            "GIT_COMMITTER_NAME": "Residue Gate Test", "GIT_COMMITTER_EMAIL": "residue@example.invalid",
        })
        self.git("init", "-q")
        (self.root / "scripts").mkdir()
        target = self.root / "scripts/check_oss_residue.sh"
        target.write_bytes(RESIDUE_SCRIPT.read_bytes())
        target.chmod(0o755)

    def git(self, *args):
        return subprocess.run(
            ["git", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null",
             "-C", str(self.root), *args],
            env=self.env, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        ).stdout.strip().decode()

    def write(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        self.created.add(path)
        return path

    def commit(self):
        if self.created:
            self.git("add", "--", *(str(path) for path in sorted(self.created)))
        self.git("commit", "-q", "--allow-empty", "-m", "fixture")

    def run_gate(self, expected, env=None, extra_args=()):
        result = subprocess.run(
            ["bash", "scripts/check_oss_residue.sh", *extra_args],
            cwd=self.root, env=env or self.env,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, check=False,
        )
        self.assertEqual(result.returncode, expected, result.stdout)
        return result.stdout

    def test_clean_repo_passes(self):
        self.write("app/src/clean.ts", "export const ok = true;\n")
        self.commit()
        output = self.run_gate(0)
        self.assertIn("ok    private repo names", output)

    def test_untracked_file_not_scanned(self):
        self.write("app/src/clean.ts", "export const ok = true;\n")
        self.commit()
        (self.root / "app/src/untracked.ts").write_text(f"// {HOME_LEAK}\n")
        self.run_gate(0)

    def test_internal_abs_path_in_scope_fails(self):
        self.write("app/src/leak.ts", f"// {HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  internal abs paths", output)
        self.assertIn("app/src/leak.ts", output)

    def test_dash_named_tracked_file_does_not_hide_leak(self):
        self.write("-q", "clean\n")
        self.write("app/src/leak.ts", f"// {HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("app/src/leak.ts", output)

    def test_internal_doc_path_label(self):
        self.write("app/src/leak2.ts", f"// {DOC_PATH_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  internal doc paths", output)

    def test_private_repo_name_label(self):
        self.write("app/src/leak3.ts", f"// {PRIVATE_REPO_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  private repo names", output)

    def test_company_identity_label(self):
        self.write("app/src/leak4.ts", f"// {COMPANY_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  company identity", output)

    def test_secret_shaped_token_label(self):
        self.write("app/src/leak5.ts", "// sk-" + "A" * 20 + "\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  secret-shaped tokens", output)

    def test_personal_identifier_label(self):
        self.write("app/src/leak6.ts", f"// {PERSONAL_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  personal identifiers", output)

    def test_keep_evals_engine_bridge_fixtures_scanned(self):
        self.write("evals/engine-bridge/fixtures/leak.json", f"{HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  internal abs paths", output)

    def test_keep_evals_run_replay_fixtures_scanned(self):
        self.write("evals/run-replay/fixtures/leak.json", f"{HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  internal abs paths", output)

    def test_evals_outside_kept_fixtures_stays_excluded(self):
        self.write("evals/other/leak.json", f"{HOME_LEAK}\n")
        self.commit()
        self.run_gate(0)

    def test_harness_agent_evals_outside_swebench_stays_excluded(self):
        self.write("harness-agent/evals/other/leak.json", f"{HOME_LEAK}\n")
        self.commit()
        self.run_gate(0)

    def test_docs_outside_spec_release_stays_excluded(self):
        self.write(f"{DOC_PATH_LEAK}/unrelated-note.md", f"{HOME_LEAK}\n")
        self.commit()
        self.run_gate(0)

    def test_private_path_globs_are_anchored(self):
        paths = (
            f"{DOC_PATH_LEAK}-public/x.md",
            "harness-agent/docs-extra/x.md",
            "evalsX/x.md",
        )
        for path in paths:
            self.write(path, f"{HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        for path in paths:
            self.assertIn(path, output)

    def test_debug_arrays_flag_never_skips_the_scan(self):
        self.write("app/src/leak7.ts", f"// {HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1, env={**self.env, "OSS_RESIDUE_DEBUG_ARRAYS": "1"})
        self.assertIn("FAIL  internal abs paths", output)

    def test_default_mode_has_no_private_path_check(self):
        self.write("app/src/clean.ts", "export const ok = true;\n")
        self.commit()
        output = self.run_gate(0)
        self.assertNotIn("private path", output)

    def test_default_scans_remote_web(self):
        path = "remote-web/src/leak.ts"
        self.write(path, f"// {HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  internal abs paths", output)
        self.assertIn(path, output)

    def test_default_scans_github_dir(self):
        path = ".github/workflows/x.yml"
        self.write(path, f"# {HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  internal abs paths", output)
        self.assertIn(path, output)

    def test_default_scans_githooks_dir(self):
        path = ".githooks/pre-commit"
        self.write(path, f"# {HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  internal abs paths", output)
        self.assertIn(path, output)

    def test_default_scans_root_rule_file(self):
        path = "AGENTS.md"
        self.write(path, f"{HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  internal abs paths", output)
        self.assertIn(path, output)

    def test_default_scans_docs_outside_private_tree(self):
        path = "docs/notes/leak.md"
        self.write(path, f"{HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  internal abs paths", output)
        self.assertIn(path, output)

    def test_self_exclusion_matches_full_path_not_basename(self):
        path = "app/scripts/check_oss_residue.sh"
        self.write(path, f"{HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn(path, output)

    def test_default_skips_private_tree(self):
        self.write(f"{DOC_PATH_LEAK}/notes/leak.md", f"{HOME_LEAK}\n")
        self.commit()
        self.run_gate(0)

    def test_default_reports_private_except_fixture(self):
        path = "evals/swebench/fair30_ids.json"
        self.write(path, f"{HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  internal abs paths", output)
        self.assertIn(path, output)

    def test_public_tree_clean_repo_passes_and_scans_self_excluded(self):
        self.write("app/src/clean.ts", "export const ok = true;\n")
        self.commit()
        output = self.run_gate(0, extra_args=["--public-tree"])
        self.assertIn("ok    private repo names", output)
        self.assertIn("ok    tracked file under a private path", output)

    def test_public_tree_flags_tracked_private_path(self):
        self.write(f"{DOC_PATH_LEAK}/leak.md", "internal note\n")
        self.commit()
        output = self.run_gate(1, extra_args=["--public-tree"])
        self.assertIn("FAIL  tracked file under a private path", output)
        self.assertIn(f"{DOC_PATH_LEAK}/leak.md", output)

    def test_public_tree_flags_new_private_mount_points(self):
        paths = (".private/notes.md", "CLAUDE.local.md", "AGENTS.private.md")
        for path in paths:
            self.write(path, "clean\n")
        self.commit()
        output = self.run_gate(1, extra_args=["--public-tree"])
        self.assertIn("FAIL  tracked file under a private path", output)
        for path in paths:
            self.assertIn(path, output)

    def test_public_tree_scans_paths_the_default_mode_excludes(self):
        # DOC_PATH_LEAK's tree is excluded from content scanning in default
        # mode; --public-tree scans private trees and also runs the private-path
        # check, so this content trips the residue scan.
        self.write(f"{DOC_PATH_LEAK}/leak.md", f"{HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1, extra_args=["--public-tree"])
        self.assertIn("FAIL  internal abs paths", output)

    def test_public_tree_keep_exceptions_not_flagged_as_private_path(self):
        self.write("evals/engine-bridge/fixtures/sample.json", "{}\n")
        self.write("evals/run-replay/fixtures/sample.json", "{}\n")
        self.write("evals/swebench/fair30_ids.json", "[]\n")
        self.commit()
        output = self.run_gate(0, extra_args=["--public-tree"])
        self.assertIn("ok    tracked file under a private path", output)

    def test_public_tree_flags_evals_file_outside_kept_fixtures(self):
        self.write("evals/other/leak.json", "{}\n")
        self.commit()
        output = self.run_gate(1, extra_args=["--public-tree"])
        self.assertIn("FAIL  tracked file under a private path", output)
        self.assertIn("evals/other/leak.json", output)

    def test_scan_survives_a_hit_list_too_large_for_the_pipe_buffer(self):
        # Regression for P2-1: printf | head -10 used to SIGPIPE(141) the
        # producer once head closed its read end early on a large hit list,
        # aborting the whole script under pipefail before the later
        # categories (including the private-path check) ever ran. 50 lines
        # of ~5KB each (~250KB total) reliably exceeds the pipe buffer.
        padding = "x" * 5000
        lines = "\n".join(f"{PRIVATE_REPO_LEAK} {padding}" for _ in range(50))
        self.write("app/src/big_leak.ts", lines + "\n")
        self.commit()
        output = self.run_gate(1, extra_args=["--public-tree"])
        self.assertIn("FAIL  private repo names", output)
        # Every later category still ran to completion (none skipped by an
        # early abort), including the private-path check this diff added.
        for label in (
            "company identity", "secret-shaped tokens", "internal doc paths",
            "internal abs paths", "personal identifiers", "tracked file under a private path",
        ):
            self.assertTrue(f"ok    {label}" in output or f"FAIL  {label}" in output, (label, output))


if __name__ == "__main__":
    unittest.main()
