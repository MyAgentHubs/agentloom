#!/usr/bin/env python3
"""Regression + drift-guard checks for scripts/check_oss_residue.sh.

The daily gate is a mirror of the six residue regexes that
scripts/build-oss-snapshot.sh runs against a stripped snapshot, plus a KEEP
list mirroring every "stripped wholesale, then explicitly copied back"
exception in that script. This test asserts neither ever drifts, then
exercises the daily gate against disposable temporary git repositories.
"""

import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parent.parent
SNAPSHOT_SCRIPT = ROOT / "scripts/build-oss-snapshot.sh"
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

# The oss-release spec directory build-oss-snapshot.sh reads public drafts
# and templates from. Built from DOC_PATH_LEAK for the same reason as above.
SPEC_DIR_PREFIX = DOC_PATH_LEAK + "/specs/2026-08-01-oss-release"

# Bump only after mirroring a newly-added check in both scripts.
EXPECTED_SCAN_COUNT = 6


def extract_scan_patterns(script_path):
    """Evaluate each literal `scan "label" pattern` line (indentation
    tolerated) with a stub `scan` function. Evaluating -- rather than
    regex-parsing the quoting -- means both a plain single-quoted pattern
    and a concatenated one resolve to their real runtime string, so the
    comparison is on effective behavior."""
    lines = [line for line in script_path.read_text().splitlines() if re.match(r'^\s*scan\s+"', line)]
    stub = "scan() { printf '%s\\x1f%s\\x1e' \"$1\" \"$2\"; }\n"
    result = subprocess.run(
        ["bash", "-c", stub + "\n".join(lines)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=True,
    )
    entries = [chunk for chunk in result.stdout.split("\x1e") if chunk]
    return [tuple(chunk.split("\x1f", 1)) for chunk in entries]


def get_debug_entries(label):
    """Ask the real check_oss_residue.sh (running against this repo) for one
    of its resolved arrays (EXCLUDE/KEEP/REPLACED/PRIVATE_PATHS/PRIVATE_EXCEPT),
    so guard tests compare against actual runtime values rather than
    re-parsing bash literals. The introspection dump goes to stderr alongside
    a real, normal scan (never a shortcut that skips it), so the scan's own
    exit code is ignored here."""
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


def get_keep_entries():
    return get_debug_entries("KEEP")


def keep_covers(keep_entries, path):
    for entry in keep_entries:
        if entry == path:
            return True
        if entry.endswith("/*"):
            base = entry[:-2]
            if path == base or path.startswith(base + "/"):
                return True
    return False


# Top-level directories build-oss-snapshot.sh's STRIP array removes
# wholesale; a $SRC/-rooted variable under one of these that later feeds a
# cp/cp -R into $OUT is a "stripped, then copied back" exception.
STRIPPED_TOP_LEVEL = (
    "docs/", "evals/", ".github/", ".githooks/", ".claude/", ".superpowers/",
    ".remember/", "harness-agent/docs/", "harness-agent/evals/", "remote-web/",
    "remote-relay/",
)

SRC_VAR_ASSIGN_RE = re.compile(r'^(\w+)="\$SRC/([^"]+)"$', re.MULTILINE)
INSTALL_DOC_CALL_RE = re.compile(r'^install_doc\s+(\S+)\s+(\S+)$', re.MULTILINE)
EXTRA_LOOP_RE = re.compile(r'for extra in ([^;]+); do')


class DriftGuardTests(unittest.TestCase):
    def test_six_patterns_match_snapshot_script(self):
        expected = extract_scan_patterns(SNAPSHOT_SCRIPT)
        actual = extract_scan_patterns(RESIDUE_SCRIPT)
        self.assertEqual(len(expected), EXPECTED_SCAN_COUNT, expected)
        self.assertEqual(len(actual), EXPECTED_SCAN_COUNT, actual)
        self.assertEqual(expected, actual)

    def test_indented_new_check_is_detected_by_extraction(self):
        """A 7th check added inside an `if`/loop (naturally indented) must
        still be picked up by extraction, so the count comparison above
        actually has a chance to catch it if check_oss_residue.sh is not
        updated to match."""
        mutated_text = SNAPSHOT_SCRIPT.read_text() + '\n  scan "internal jira ticket ids" \'PROJ-[0-9]{4,}\'\n'
        with tempfile.NamedTemporaryFile("w", suffix=".sh", delete=False) as handle:
            handle.write(mutated_text)
            mutated_path = Path(handle.name)
        self.addCleanup(mutated_path.unlink)
        mutated_patterns = extract_scan_patterns(mutated_path)
        self.assertEqual(len(mutated_patterns), EXPECTED_SCAN_COUNT + 1, mutated_patterns)
        self.assertNotEqual(mutated_patterns, extract_scan_patterns(RESIDUE_SCRIPT))


def extract_strip_array(script_path):
    """STRIP is a plain, unquoted bash array literal (no variable expansion),
    so a direct line-range regex is enough -- no need for the extract_bash_array
    approach the KEEP/PRIVATE_PATHS arrays use in get_debug_entries()."""
    match = re.search(r'^STRIP=\(\n(.*?)^\)\n', script_path.read_text(), re.MULTILINE | re.DOTALL)
    assert match is not None, "STRIP array not found in build-oss-snapshot.sh"
    return [line.strip() for line in match.group(1).splitlines() if line.strip()]


def normalize_private_path(entry):
    return entry[:-2] if entry.endswith("/*") else entry


class PrivatePathsDriftGuardTests(unittest.TestCase):
    """PRIVATE_PATHS (scripts/check_oss_residue.sh, --public-tree mode) is
    the single definition of "paths that move to the private repo". Every
    entry must either be identical to one of build-oss-snapshot.sh's STRIP
    entries, or a documented-narrower subpath of one (DOC_PATH_LEAK's tree is
    narrower than STRIP's whole-of-docs, because docs/benchmarks.md stays
    public) -- so a typo or an un-mirrored addition on either side goes red
    here instead of silently drifting."""

    def test_every_private_path_is_covered_by_strip(self):
        strip = extract_strip_array(SNAPSHOT_SCRIPT)
        private_paths = sorted({normalize_private_path(e) for e in get_debug_entries("PRIVATE_PATHS")})
        self.assertTrue(private_paths)
        uncovered = [
            path for path in private_paths
            if not any(path == entry or path.startswith(entry + "/") for entry in strip)
        ]
        self.assertEqual(uncovered, [])

    def test_every_private_except_is_nested_under_a_private_path(self):
        private_paths = get_debug_entries("PRIVATE_PATHS")
        private_except = get_debug_entries("PRIVATE_EXCEPT")
        self.assertTrue(private_except)
        for entry in private_except:
            base = normalize_private_path(entry)
            self.assertTrue(keep_covers(private_paths, base), f"{entry} not nested under PRIVATE_PATHS")


class StructuralGuardTests(unittest.TestCase):
    """Parse build-oss-snapshot.sh itself: if it grows a new
    strip-then-copy-back step, KEEP must grow with it or these go red."""

    def setUp(self):
        self.snapshot_text = SNAPSHOT_SCRIPT.read_text()
        self.keep = get_keep_entries()

    def test_evals_and_harness_evals_copy_backs_are_covered(self):
        checked = 0
        for name, src_path in SRC_VAR_ASSIGN_RE.findall(self.snapshot_text):
            if not src_path.startswith(STRIPPED_TOP_LEVEL):
                continue
            # Only count vars actually used as a bare `cp`/`cp -R` source
            # into $OUT -- e.g. SPEC is $SRC/docs/... too, but it is never
            # copied wholesale, only read piecemeal by install_doc() etc.
            used_as_copy_source = re.search(rf'cp\s+(-R\s+)?"\${re.escape(name)}"\s+"\$OUT/', self.snapshot_text)
            if not used_as_copy_source:
                continue
            checked += 1
            self.assertTrue(keep_covers(self.keep, src_path), f"{src_path} not covered by KEEP")
        self.assertGreaterEqual(checked, 3, "expected at least the 3 known evals/ copy-backs")

    def test_install_doc_sources_are_covered(self):
        pairs = INSTALL_DOC_CALL_RE.findall(self.snapshot_text)
        self.assertEqual(len(pairs), 7, pairs)
        for from_name, _to in pairs:
            expected = f"{SPEC_DIR_PREFIX}/{from_name}"
            self.assertTrue(keep_covers(self.keep, expected), f"{expected} not covered by KEEP")

    def test_extra_root_files_are_covered(self):
        match = EXTRA_LOOP_RE.search(self.snapshot_text)
        self.assertIsNotNone(match)
        extras = match.group(1).split()
        self.assertEqual(extras, ["LICENSE", "TRADEMARK.md", "CODE_OF_CONDUCT.md"])
        for extra in extras:
            expected = f"{SPEC_DIR_PREFIX}/{extra}"
            self.assertTrue(keep_covers(self.keep, expected), f"{expected} not covered by KEEP")

    def test_github_templates_source_is_covered(self):
        self.assertIn('cp -R "$SPEC/github-templates/."', self.snapshot_text)
        expected_glob = f"{SPEC_DIR_PREFIX}/github-templates/*"
        self.assertIn(expected_glob, self.keep)


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

    def test_internal_abs_path_under_strip_dir_passes(self):
        self.write("docs/notes/leak.md", f"{HOME_LEAK}\n")
        self.commit()
        self.run_gate(0)

    def test_internal_doc_path_label(self):
        self.write("app/src/leak2.ts", f"// {DOC_PATH_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  internal doc paths", output)

    def test_replaced_file_content_not_scanned(self):
        self.write("harness-agent/README.md", f"// {HOME_LEAK}\n")
        self.commit()
        self.run_gate(0)

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

    def test_keep_harness_agent_swebench_fair30_scanned(self):
        self.write("harness-agent/evals/swebench-venv/scratch/fairA/fair30_ids.json", f"{HOME_LEAK}\n")
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

    def test_keep_install_doc_source_scanned(self):
        self.write(f"{SPEC_DIR_PREFIX}/README-draft.en.md", f"{HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  internal abs paths", output)

    def test_keep_github_templates_source_scanned(self):
        self.write(f"{SPEC_DIR_PREFIX}/github-templates/workflows/ci.yml", f"{HOME_LEAK}\n")
        self.commit()
        output = self.run_gate(1)
        self.assertIn("FAIL  internal abs paths", output)

    def test_docs_outside_spec_release_stays_excluded(self):
        self.write(f"{DOC_PATH_LEAK}/unrelated-note.md", f"{HOME_LEAK}\n")
        self.commit()
        self.run_gate(0)

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

    def test_public_tree_scans_paths_the_default_mode_excludes(self):
        # DOC_PATH_LEAK's tree is excluded from content scanning in default
        # mode (test_internal_abs_path_under_strip_dir_passes); --public-tree mode
        # scans every tracked file, so the same content trips the six regexes
        # here even before the private-path check runs.
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
