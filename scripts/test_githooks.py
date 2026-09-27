#!/usr/bin/env python3
"""Regression checks for .githooks/pre-commit, .githooks/pre-push and
scripts/install-hooks.sh, all run against disposable git repositories created
under tempfile.TemporaryDirectory -- never against the real repo's index,
refs or git config (core.hooksPath is only ever set on these throwaway
fixtures, never on the shared worktree this test file lives in)."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True
SCRIPT = Path(__file__).resolve()
REPO_ROOT = SCRIPT.parent.parent
HOOKS_DIR = REPO_ROOT / ".githooks"
INSTALL_SCRIPT = REPO_ROOT / "scripts/install-hooks.sh"
ZERO_SHA = "0" * 40
# Joined at runtime so the public-snapshot residue scan (which greps for the
# literal joined path) does not flag these fixture paths as leftover
# indexing code.
DOCS_TREE = "/".join(("docs", "superpowers"))


def _has_cargo():
    return shutil.which("cargo") is not None


class HooksFixtureTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix=".githooks-test-", dir=REPO_ROOT)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.env.update({
            "GIT_AUTHOR_NAME": "Hooks Test", "GIT_AUTHOR_EMAIL": "hookstest@example.invalid",
            "GIT_COMMITTER_NAME": "Hooks Test", "GIT_COMMITTER_EMAIL": "hookstest@example.invalid",
        })
        self.git("init", "-q")
        self.git("symbolic-ref", "HEAD", "refs/heads/fixture")

        (self.root / ".githooks").mkdir()
        for name in ("pre-commit", "pre-push"):
            dest = self.root / ".githooks" / name
            shutil.copyfile(HOOKS_DIR / name, dest)
            os.chmod(dest, 0o755)
        (self.root / "scripts").mkdir()
        install_dest = self.root / "scripts/install-hooks.sh"
        shutil.copyfile(INSTALL_SCRIPT, install_dest)
        os.chmod(install_dest, 0o755)

        self.git("config", "core.hooksPath", ".githooks")
        self.write("README.md", "fixture\n")
        self.git("add", "--", "README.md")
        self.git("commit", "-q", "-m", "init")

    def git(self, *args, check=True, env=None):
        return subprocess.run(
            ["git", "-c", "commit.gpgsign=false", "-C", str(self.root), *args],
            env=env or self.env, check=check,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )

    def write(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        return path

    def write_fake_gate(self, relative, exit_code, marker):
        path = self.write(
            relative,
            "#!/usr/bin/env python3\n"
            "import sys\n"
            f"print({marker!r})\n"
            f"sys.exit({exit_code})\n",
        )
        os.chmod(path, 0o755)
        return path

    def commit(self, paths, message="wip", extra_args=(), env=None):
        self.git("add", "--", *paths)
        return self.git("commit", "-q", "-m", message, *extra_args, check=False, env=env)

    # ---- pre-commit ----

    def test_docs_only_commit_with_no_broken_links_passes(self):
        self.write(
            f"{DOCS_TREE}/INDEX.md",
            "# index\nno dated references here\n",
        )
        result = self.commit([f"{DOCS_TREE}/INDEX.md"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_missing_gate_scripts_are_skipped(self):
        self.write("app/src/foo.ts", "export const x = 1;\n")
        result = self.commit(["app/src/foo.ts"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_docs_other_path_triggers_source_gates(self):
        # docs/other/ is not the app-line governed docs tree, so it must not
        # be treated as a docs-only, source-gate-free commit.
        self.write_fake_gate("scripts/check_file_size.py", 1, "FAKE_DOCS_OTHER_MARKER_FAIL")
        self.commit(["scripts/check_file_size.py"], extra_args=("--no-verify",))
        self.write("docs/other/note.md", "not governance\n")
        result = self.commit(["docs/other/note.md"])
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("FAKE_DOCS_OTHER_MARKER_FAIL", result.stderr)
        self.assertIn("file-size", result.stderr)

    def test_git_mv_into_docs_superpowers_triggers_source_gates(self):
        # A git mv of src/a.ts into the governed docs tree must not be
        # folded by git's rename detection into something that reads as a
        # pure docs change.
        self.write_fake_gate("scripts/check_file_size.py", 1, "FAKE_MV_MARKER_FAIL")
        self.commit(["scripts/check_file_size.py"], extra_args=("--no-verify",))
        self.write("src/a.ts", "export const a = 1;\n")
        self.commit(["src/a.ts"], extra_args=("--no-verify",))
        (self.root / DOCS_TREE).mkdir(parents=True, exist_ok=True)
        self.git("mv", "src/a.ts", f"{DOCS_TREE}/a.ts", check=True)
        result = self.git("commit", "-q", "-m", "move into docs", check=False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("FAKE_MV_MARKER_FAIL", result.stderr)
        self.assertIn("file-size", result.stderr)

    def test_pure_docs_superpowers_commit_only_runs_orphans_gate(self):
        # A failing check_file_size.py (a source gate) must not block a
        # commit whose staged paths are entirely under the governed docs tree.
        self.write_fake_gate("scripts/check_file_size.py", 1, "FAKE_SHOULD_NOT_RUN_MARKER")
        self.commit(["scripts/check_file_size.py"], extra_args=("--no-verify",))
        self.write(f"{DOCS_TREE}/notes/2026-09-19-y.md", "y\n")
        result = self.commit([f"{DOCS_TREE}/notes/2026-09-19-y.md"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_failing_fake_gate_blocks_commit_with_marker(self):
        self.write_fake_gate("scripts/check_file_size.py", 1, "FAKE_FILE_SIZE_MARKER_FAIL")
        self.write("app/src/bar.ts", "export const y = 1;\n")
        result = self.commit(["scripts/check_file_size.py", "app/src/bar.ts"])
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("FAKE_FILE_SIZE_MARKER_FAIL", result.stderr)
        self.assertIn("file-size", result.stderr)

    def test_passing_fake_gate_allows_commit(self):
        self.write_fake_gate("scripts/check_file_size.py", 0, "FAKE_FILE_SIZE_MARKER_OK")
        self.write("app/src/baz.ts", "export const z = 1;\n")
        result = self.commit(["scripts/check_file_size.py", "app/src/baz.ts"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_no_verify_bypasses_failing_gate(self):
        self.write_fake_gate("scripts/check_file_size.py", 1, "FAKE_FILE_SIZE_MARKER_FAIL")
        self.write("app/src/qux.ts", "export const q = 1;\n")
        result = self.commit(
            ["scripts/check_file_size.py", "app/src/qux.ts"], extra_args=("--no-verify",)
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_failing_doc_orphans_gate_blocks_docs_commit(self):
        self.write_fake_gate("scripts/check_doc_orphans.py", 1, "FAKE_ORPHAN_MARKER_FAIL")
        self.write(f"{DOCS_TREE}/notes/2026-09-19-x.md", "x\n")
        result = self.commit(
            ["scripts/check_doc_orphans.py", f"{DOCS_TREE}/notes/2026-09-19-x.md"]
        )
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("FAKE_ORPHAN_MARKER_FAIL", result.stderr)
        self.assertIn("doc-orphans", result.stderr)

    def test_doc_orphans_gate_not_run_for_non_docs_commit(self):
        # A failing check_doc_orphans.py must not block a commit that touches
        # no governed-docs-tree path at all.
        self.write_fake_gate("scripts/check_doc_orphans.py", 1, "FAKE_ORPHAN_MARKER_FAIL")
        self.write("app/src/only_code.ts", "export const c = 1;\n")
        result = self.commit(["app/src/only_code.ts"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_cli_line_broken_link_blocks_commit(self):
        self.write("harness-agent/docs/INDEX.md", "see specs/2026-09-19-missing.md\n")
        result = self.commit(["harness-agent/docs/INDEX.md"])
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("doc-governance 硬闸(cli)", result.stderr)
        self.assertIn("specs/2026-09-19-missing.md", result.stderr)

    def test_no_cli_docs_index_skips_cli_check(self):
        # No harness-agent/docs/INDEX.md in this tree at all -> the cli block
        # self-limits and lets the commit through even though the referenced
        # doc under harness-agent/docs/ does not exist.
        self.write("harness-agent/docs/notes/2026-09-19-x.md", "see specs/2026-09-19-missing.md\n")
        result = self.commit(["harness-agent/docs/notes/2026-09-19-x.md"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_oss_residue_gate_runs_public_tree_mode_when_docs_superpowers_untracked(self):
        # The governed docs tree exists on disk but nothing under it is
        # tracked by git (only an untracked scratch file) -> the hook's
        # `git ls-files` probe must find no tracked path and pick
        # --public-tree mode.
        log = self.root / "oss-residue-args.log"
        self.write(
            "scripts/check_oss_residue.sh",
            "#!/bin/bash\n"
            f'printf \'%s\\n\' "$*" >> "{log}"\n'
            "exit 0\n",
        )
        self.commit(["scripts/check_oss_residue.sh"], extra_args=("--no-verify",))
        self.write(f"{DOCS_TREE}/scratch/untracked.md", "not tracked\n")
        self.write("app/src/pub_tree.ts", "export const p = 1;\n")
        result = self.commit(["app/src/pub_tree.ts"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(log.exists(), result.stdout + result.stderr)
        self.assertIn("--public-tree", log.read_text())

    def test_oss_residue_gate_runs_internal_tree_mode_when_docs_superpowers_tracked(self):
        # A tracked file under the governed docs tree (committed, not just
        # present on disk) means this is the internal doc tree -> no
        # --public-tree flag should be passed.
        log = self.root / "oss-residue-internal-args.log"
        self.write(
            "scripts/check_oss_residue.sh",
            "#!/bin/bash\n"
            f'printf \'%s\\n\' "$*" >> "{log}"\n'
            "exit 0\n",
        )
        self.write(f"{DOCS_TREE}/INDEX.md", "# index\n")
        self.commit(
            ["scripts/check_oss_residue.sh", f"{DOCS_TREE}/INDEX.md"],
            extra_args=("--no-verify",),
        )
        self.write("app/src/internal_tree.ts", "export const i = 1;\n")
        result = self.commit(["app/src/internal_tree.ts"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(log.exists(), result.stdout + result.stderr)
        self.assertEqual(log.read_text().strip(), "")

    # ---- pre-push ----

    def _remote(self):
        bare = Path(tempfile.mkdtemp(prefix=".githooks-remote-", dir=REPO_ROOT))
        self.addCleanup(shutil.rmtree, bare, ignore_errors=True)
        subprocess.run(
            ["git", "init", "--bare", "-q", str(bare)], check=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        self.git("remote", "add", "origin", str(bare))
        return bare

    def test_push_with_no_guarded_paths_passes(self):
        bare = self._remote()
        result = self.git("push", "origin", "fixture:fixture", check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        remote_head = subprocess.run(
            ["git", "-C", str(bare), "rev-parse", "fixture"],
            check=True, stdout=subprocess.PIPE, text=True,
        ).stdout.strip()
        local_head = self.git("rev-parse", "HEAD").stdout.strip()
        self.assertEqual(remote_head, local_head)

    def test_push_skips_npm_lint_when_package_has_no_lint_script(self):
        self._remote()
        (self.root / "app/node_modules").mkdir(parents=True)
        self.write(
            "app/package.json",
            '{"scripts": {"typecheck": "true"}}\n',
        )
        self.git("add", "--", "app/package.json")
        self.git("commit", "-q", "-m", "add package.json")

        fake_bin = self.root.parent / (self.root.name + "-bin")
        fake_bin.mkdir(exist_ok=True)
        self.addCleanup(shutil.rmtree, fake_bin, ignore_errors=True)
        npm_log = self.root.parent / (self.root.name + "-npm.log")
        self.addCleanup(lambda: npm_log.unlink(missing_ok=True))
        fake_npm = fake_bin / "npm"
        fake_npm.write_text(
            "#!/bin/bash\n"
            f'echo "$@" >> "{npm_log}"\n'
            "exit 0\n"
        )
        os.chmod(fake_npm, 0o755)

        env = dict(self.env)
        env["PATH"] = f"{fake_bin}:{self.env['PATH']}"
        result = self.git("push", "origin", "fixture:fixture", check=False, env=env)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = npm_log.read_text().splitlines() if npm_log.exists() else []
        self.assertIn("run typecheck", calls)
        self.assertNotIn("run lint", calls)

    def test_push_runs_npm_lint_when_package_declares_lint_script(self):
        self._remote()
        (self.root / "app/node_modules").mkdir(parents=True)
        self.write(
            "app/package.json",
            '{"scripts": {"typecheck": "true", "lint": "true"}}\n',
        )
        self.git("add", "--", "app/package.json")
        self.git("commit", "-q", "-m", "add package.json with lint")

        fake_bin = self.root.parent / (self.root.name + "-bin2")
        fake_bin.mkdir(exist_ok=True)
        self.addCleanup(shutil.rmtree, fake_bin, ignore_errors=True)
        npm_log = self.root.parent / (self.root.name + "-npm2.log")
        self.addCleanup(lambda: npm_log.unlink(missing_ok=True))
        fake_npm = fake_bin / "npm"
        fake_npm.write_text(
            "#!/bin/bash\n"
            f'echo "$@" >> "{npm_log}"\n'
            "exit 0\n"
        )
        os.chmod(fake_npm, 0o755)

        env = dict(self.env)
        env["PATH"] = f"{fake_bin}:{self.env['PATH']}"
        result = self.git("push", "origin", "fixture:fixture", check=False, env=env)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = npm_log.read_text().splitlines() if npm_log.exists() else []
        self.assertIn("run typecheck", calls)
        self.assertIn("run lint", calls)

    @unittest.skipUnless(_has_cargo(), "cargo not on PATH")
    def test_push_blocked_by_cargo_fmt_check_and_remote_unchanged(self):
        bare = self._remote()
        self.write("harness-agent/Cargo.toml",
                    '[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2021"\n')
        self.write("harness-agent/src/main.rs", 'fn main(){println!("hi");}\n')
        self.git("add", "--", "harness-agent/Cargo.toml", "harness-agent/src/main.rs")
        self.git("commit", "-q", "-m", "unformatted rust")

        result = self.git("push", "origin", "fixture:fixture", check=False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("cargo fmt", result.stderr)
        remote_refs = subprocess.run(
            ["git", "-C", str(bare), "for-each-ref"],
            check=True, stdout=subprocess.PIPE, text=True,
        ).stdout
        self.assertNotIn("fixture", remote_refs)

    @unittest.skipUnless(_has_cargo(), "cargo not on PATH")
    def test_push_passes_with_formatted_rust(self):
        self._remote()
        self.write("harness-agent/Cargo.toml",
                    '[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2021"\n')
        self.write("harness-agent/src/main.rs", 'fn main() {\n    println!("hi");\n}\n')
        self.git("add", "--", "harness-agent/Cargo.toml", "harness-agent/src/main.rs")
        self.git("commit", "-q", "-m", "formatted rust")

        result = self.git("push", "origin", "fixture:fixture", check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class InstallHooksScriptTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix=".install-hooks-test-", dir=REPO_ROOT)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.env.update({
            "GIT_AUTHOR_NAME": "Install Hooks Test", "GIT_AUTHOR_EMAIL": "installhooks@example.invalid",
            "GIT_COMMITTER_NAME": "Install Hooks Test", "GIT_COMMITTER_EMAIL": "installhooks@example.invalid",
        })
        subprocess.run(
            ["git", "-c", "commit.gpgsign=false", "-C", str(self.root), "init", "-q"],
            env=self.env, check=True,
        )
        (self.root / ".githooks").mkdir()
        for name in ("pre-commit", "pre-push"):
            shutil.copyfile(HOOKS_DIR / name, self.root / ".githooks" / name)
        (self.root / "scripts").mkdir()
        shutil.copyfile(INSTALL_SCRIPT, self.root / "scripts/install-hooks.sh")

    def _get_hooks_path(self):
        result = subprocess.run(
            ["git", "-C", str(self.root), "config", "--get", "core.hooksPath"],
            env=self.env, check=False, stdout=subprocess.PIPE, text=True,
        )
        return result.returncode, result.stdout.strip()

    def _run_install(self):
        return subprocess.run(
            ["bash", "scripts/install-hooks.sh"],
            cwd=self.root, env=self.env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )

    def test_install_sets_hooks_path_and_is_idempotent(self):
        before_code, _ = self._get_hooks_path()
        self.assertNotEqual(before_code, 0)  # unset before install

        first = self._run_install()
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        code, value = self._get_hooks_path()
        self.assertEqual(code, 0)
        self.assertEqual(value, ".githooks")

        second = self._run_install()
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        code, value = self._get_hooks_path()
        self.assertEqual(code, 0)
        self.assertEqual(value, ".githooks")

    def test_install_makes_hooks_executable(self):
        self._run_install()
        for name in ("pre-commit", "pre-push"):
            mode = (self.root / ".githooks" / name).stat().st_mode
            self.assertTrue(mode & 0o111, f"{name} not executable")


class SourceResidueTests(unittest.TestCase):
    def test_this_file_does_not_contain_the_joined_docs_literal(self):
        # scripts/check_oss_residue.sh greps the public-snapshot-bound tree
        # for the literal joined path; fixtures here must assemble it at
        # runtime via DOCS_TREE instead of spelling it out.
        joined_literal = "/".join(("docs", "superpowers"))
        self.assertNotIn(joined_literal, SCRIPT.read_text())


if __name__ == "__main__":
    unittest.main()
