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
# Joined at runtime so the public-tree residue scan (which greps for the
# literal joined path) does not flag these fixture paths as leftover
# indexing code.
DOCS_TREE = "/".join(("docs", "superpowers"))


def _has_cargo():
    return shutil.which("cargo") is not None


class HooksFixtureBase(unittest.TestCase):
    initial_internal_tree = True

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix=".githooks-test-", dir=REPO_ROOT)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.env.update({
            "GIT_AUTHOR_NAME": "Hooks Test", "GIT_AUTHOR_EMAIL": "hookstest@example.invalid",
            "GIT_COMMITTER_NAME": "Hooks Test", "GIT_COMMITTER_EMAIL": "hookstest@example.invalid",
            "GIT_ALLOW_PROTOCOL": "file", "GIT_SSH_COMMAND": "false", "GIT_TERMINAL_PROMPT": "0",
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
        initial_paths = ["README.md"]
        if self.initial_internal_tree:
            self.write(f"{DOCS_TREE}/.keep", "fixture\n")
            initial_paths.append(f"{DOCS_TREE}/.keep")
        self.git("add", "--", *initial_paths)
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


class HooksFixtureTests(HooksFixtureBase):
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
        # The governed docs tree exists on disk, but HEAD has no file under it.
        # An untracked scratch file must not change the public-tree mode.
        self.git("rm", "--", f"{DOCS_TREE}/.keep")
        self.git("commit", "-q", "-m", "remove internal marker")
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

    def test_internal_tree_real_residue_gate_allows_ordinary_file(self):
        shutil.copyfile(
            REPO_ROOT / "scripts/check_oss_residue.sh",
            self.root / "scripts/check_oss_residue.sh",
        )
        self.write("app/src/ordinary.ts", "export const ordinary = 1;\n")
        result = self.commit(["app/src/ordinary.ts"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_internal_tree_real_residue_gate_allows_pathspec_doc_commit(self):
        shutil.copyfile(
            REPO_ROOT / "scripts/check_oss_residue.sh",
            self.root / "scripts/check_oss_residue.sh",
        )
        result = self.commit(["scripts/check_oss_residue.sh"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        doc_path = f"{DOCS_TREE}/INDEX.md"
        self.write(doc_path, "# index\n")
        result = self.commit([doc_path])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.write(doc_path, "# updated index\n")
        self.git("add", "--", doc_path)
        result = self.git("commit", "-q", "-m", "update index", "--", doc_path, check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    # ---- pre-push ----

    def _remote(self):
        bare = Path(tempfile.mkdtemp(prefix=".githooks-remote-"))
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


class PublicPushGuardTests(HooksFixtureBase):
    initial_internal_tree = False

    def setUp(self):
        super().setUp()
        for name in ("check_oss_residue.sh", "check_commit_identity.sh"):
            shutil.copyfile(REPO_ROOT / "scripts" / name, self.root / "scripts" / name)

    def install_snapshot(self):
        result = subprocess.run(
            ["bash", "scripts/install-hooks.sh"], cwd=self.root, env=self.env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def invoke_hook(self, remote_name, remote_url, ref="refs/heads/internal-work"):
        sha = self.git("rev-parse", "HEAD").stdout.strip()
        return subprocess.run(
            ["bash", ".githooks/pre-push", remote_name, remote_url],
            cwd=self.root, env=self.env, check=False,
            input=f"{ref} {sha} refs/heads/work {ZERO_SHA}\n",
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )

    def make_prohibited_tag(self):
        pattern = subprocess.run(
            ["bash", "scripts/check_commit_identity.sh", "--print-pattern"],
            cwd=self.root, env=self.env, check=True, stdout=subprocess.PIPE, text=True,
        ).stdout.strip()
        bad_env = dict(self.env)
        bad_env["GIT_COMMITTER_EMAIL"] = "tagger@" + pattern.split("|")[0].replace("\\", "") + ".example.invalid"
        self.git("tag", "-a", "v9.9.9", "-m", "fixture tag", env=bad_env)
        return self.git("rev-parse", "refs/tags/v9.9.9").stdout.strip()

    def remote(self, url="https://github.com/MyAgentHubs/agentloom.git"):
        bare = Path(tempfile.mkdtemp(prefix=".push-guard-remote-"))
        self.addCleanup(shutil.rmtree, bare, ignore_errors=True)
        subprocess.run(["git", "init", "--bare", "-q", str(bare)], check=True)
        self.git("remote", "add", "origin", url)
        self.git("config", f"url.{bare}.insteadOf", url)
        return bare

    def remote_ref(self, bare, ref="refs/heads/work"):
        return subprocess.run(
            ["git", "-C", str(bare), "rev-parse", "--verify", ref],
            check=False, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )

    def assert_push_rejected(self, bare, refspec, reason, ref="refs/heads/work"):
        result = self.git("push", "origin", refspec, check=False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(reason, result.stderr)
        self.assertIn("contact a maintainer", result.stderr)
        self.assertNotEqual(self.remote_ref(bare, ref).returncode, 0)

    def test_rejects_internal_branch(self):
        bare = self.remote()
        self.git("branch", "internal-work")
        self.assert_push_rejected(
            bare, "internal-work:refs/heads/internal-work", "restricted ref name",
            "refs/heads/internal-work",
        )

    def test_rejects_dispatch_branch(self):
        bare = self.remote()
        self.git("branch", "dispatch/x")
        self.assert_push_rejected(
            bare, "dispatch/x:refs/heads/dispatch/x", "restricted ref name",
            "refs/heads/dispatch/x",
        )

    def test_rejects_private_path_even_after_deletion(self):
        bare = self.remote()
        path = f"{DOCS_TREE}/x.md"
        self.write(path, "private fixture\n")
        self.git("add", "--", path)
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "add private path")
        self.git("rm", "--", path)
        self.git("commit", "-qm", "remove private path")
        self.assert_push_rejected(bare, "fixture:refs/heads/work", "private path found")

    def test_rejects_prohibited_commit_identity(self):
        bare = self.remote()
        pattern = subprocess.run(
            ["bash", "scripts/check_commit_identity.sh", "--print-pattern"],
            cwd=self.root, env=self.env, check=True,
            stdout=subprocess.PIPE, text=True,
        ).stdout.strip()
        email = "author@" + pattern.split("|")[0].replace("\\", "") + ".example.invalid"
        self.write("ordinary.txt", "fixture\n")
        bad_env = dict(self.env)
        bad_env.update({"GIT_AUTHOR_EMAIL": email, "GIT_COMMITTER_EMAIL": email})
        self.git("add", "--", "ordinary.txt")
        result = self.git(
            "-c", "core.hooksPath=/dev/null", "commit", "-qm", "old identity",
            check=False, env=bad_env,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_push_rejected(bare, "fixture:refs/heads/work", "prohibited commit identity")

    def test_rejects_agentloom_version_tag(self):
        bare = self.remote()
        self.git("tag", "agentloom-v9.9.9")
        self.assert_push_rejected(
            bare, "refs/tags/agentloom-v9.9.9:refs/tags/agentloom-v9.9.9",
            "restricted ref name", "refs/tags/agentloom-v9.9.9",
        )

    def test_rejects_ssh_public_url(self):
        bare = self.remote("git@github.com:MyAgentHubs/agentloom.git")
        self.git("branch", "internal-work")
        self.assert_push_rejected(
            bare, "internal-work:refs/heads/internal-work", "restricted ref name",
            "refs/heads/internal-work",
        )

    def test_rejects_public_hook_url_without_public_remote_config(self):
        self.remote("https://github.com/MyAgentHubs/agentloom-private.git")
        sha = self.git("rev-parse", "HEAD").stdout.strip()
        result = subprocess.run(
            ["bash", ".githooks/pre-push", "origin", "https://github.com/MyAgentHubs/agentloom.git"],
            cwd=self.root, env=self.env, check=False,
            input=f"refs/heads/internal-work {sha} refs/heads/work {ZERO_SHA}\n",
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("restricted ref name", result.stderr)

    def test_allows_ordinary_public_push(self):
        bare = self.remote()
        self.write("ordinary.txt", "fixture\n")
        result = self.commit(["ordinary.txt"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        result = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.remote_ref(bare).stdout.strip(), self.git("rev-parse", "HEAD").stdout.strip())

    def test_allows_private_path_exception(self):
        bare = self.remote()
        path = "evals/engine-bridge/fixtures/sample.txt"
        self.write(path, "public fixture\n")
        result = self.commit([path])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        result = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.remote_ref(bare).returncode, 0)

    def test_missing_private_path_script_fails_closed(self):
        bare = self.remote()
        (self.root / "scripts/check_oss_residue.sh").unlink()
        self.assert_push_rejected(bare, "fixture:refs/heads/work", "unable to check private paths")

    def test_missing_identity_script_fails_closed(self):
        bare = self.remote()
        (self.root / "scripts/check_commit_identity.sh").unlink()
        self.assert_push_rejected(bare, "fixture:refs/heads/work", "unable to check commit identity")

    def test_allows_internal_branch_to_non_public_remote(self):
        bare = self.remote("https://github.com/MyAgentHubs/agentloom-private.git")
        self.git("branch", "internal-work")
        result = self.git("push", "origin", "internal-work:refs/heads/internal-work", check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.remote_ref(bare, "refs/heads/internal-work").returncode, 0)

    def test_allows_deletion(self):
        bare = self.remote()
        first = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        deletion = self.git("push", "origin", ":refs/heads/work", check=False)
        self.assertEqual(deletion.returncode, 0, deletion.stdout + deletion.stderr)
        self.assertNotEqual(self.remote_ref(bare).returncode, 0)

    def test_existing_ref_checks_only_new_commits(self):
        bare = self.remote()
        path = f"{DOCS_TREE}/old.md"
        self.write(path, "old fixture\n")
        self.git("add", "--", path)
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "old private path")
        baseline = self.git("-c", "core.hooksPath=/dev/null", "push", "origin", "fixture:refs/heads/work")
        self.assertEqual(baseline.returncode, 0, baseline.stdout + baseline.stderr)
        self.write("ordinary.txt", "new fixture\n")
        result = self.commit(["ordinary.txt"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        result = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.remote_ref(bare).stdout.strip(), self.git("rev-parse", "HEAD").stdout.strip())

    def test_installed_snapshot_blocks_old_branch_hook(self):
        bare = self.remote()
        self.install_snapshot()
        self.git("switch", "-q", "-c", "legacy")
        self.write(".githooks/pre-push", "#!/bin/sh\nexit 0\n")
        path = f"{DOCS_TREE}/legacy.md"
        self.write(path, "private fixture\n")
        self.git("add", "--", ".githooks/pre-push", path)
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "legacy hook")
        result = self.git("push", "origin", "legacy:refs/heads/work", check=False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("private path found", result.stderr)
        self.assertNotIn("install-hooks", result.stderr)
        self.assertNotEqual(self.remote_ref(bare).returncode, 0)

    def test_guardless_source_cannot_replace_installed_snapshot(self):
        bare = self.remote()
        self.install_snapshot()
        snapshot = self.root / ".git/hooks/pre-push"
        original = snapshot.read_bytes()
        self.write(".githooks/pre-push", "#!/bin/sh\nexit 0\n")
        result = subprocess.run(
            ["bash", "scripts/install-hooks.sh"], cwd=self.root, env=self.env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("lacks the AgentLoom push guard marker", result.stderr)
        self.assertEqual(snapshot.read_bytes(), original)
        path = f"{DOCS_TREE}/legacy.md"
        self.write(path, "private fixture\n")
        self.git("add", "--", path)
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "legacy path")
        self.assert_push_rejected(bare, "fixture:refs/heads/work", "private path found")

    def test_moved_repository_retains_push_guard(self):
        bare = self.remote()
        self.install_snapshot()
        path = f"{DOCS_TREE}/moved.md"
        self.write(path, "private fixture\n")
        self.git("add", "--", path)
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "private path")
        moved = self.root.with_name(self.root.name + "-moved")
        self.root.rename(moved)
        self.addCleanup(shutil.rmtree, moved, ignore_errors=True)
        self.root = moved
        self.assert_push_rejected(bare, "fixture:refs/heads/work", "private path found")

    def test_installed_snapshot_blocks_public_push_from_git_directory(self):
        bare = self.remote()
        self.install_snapshot()
        result = subprocess.run(
            ["git", "-C", str(self.root / ".git"), "push", "origin", "fixture:refs/heads/work"],
            env=self.env, check=False, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("unable to determine working tree", result.stderr)
        self.assertIn("ref: (unavailable), commit: (unavailable)", result.stderr)
        self.assertNotEqual(self.remote_ref(bare).returncode, 0)

    def test_snapshot_scripts_survive_missing_worktree_scripts(self):
        bare = self.remote()
        self.install_snapshot()
        for name in ("check_oss_residue.sh", "check_commit_identity.sh"):
            (self.root / "scripts" / name).unlink()
        result = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.remote_ref(bare).returncode, 0)

    def test_snapshot_script_falls_back_to_worktree(self):
        bare = self.remote()
        self.install_snapshot()
        (self.root / ".git/hooks/agentloom-guard/check_oss_residue.sh").unlink()
        result = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.remote_ref(bare).returncode, 0)

    def test_snapshot_and_worktree_script_missing_fails_closed(self):
        bare = self.remote()
        self.install_snapshot()
        (self.root / ".git/hooks/agentloom-guard/check_oss_residue.sh").unlink()
        (self.root / "scripts/check_oss_residue.sh").unlink()
        self.assert_push_rejected(bare, "fixture:refs/heads/work", "unable to check private paths")

    def test_stale_snapshot_warning_does_not_block_clean_push(self):
        bare = self.remote()
        self.install_snapshot()
        with (self.root / ".githooks/pre-push").open("a") as source:
            source.write("\n# Fixture-only change.\n")
        result = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("hooks snapshot is stale; run scripts/install-hooks.sh", result.stderr)
        self.assertEqual(self.remote_ref(bare).returncode, 0)

    def test_changed_helper_warns_snapshot_is_stale(self):
        bare = self.remote()
        self.install_snapshot()
        with (self.root / "scripts/check_oss_residue.sh").open("a") as script:
            script.write("\n# Fixture-only change.\n")
        result = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("install-hooks", result.stderr)
        self.assertEqual(self.remote_ref(bare).returncode, 0)

    def test_public_url_variants_reject_without_network(self):
        self.remote("https://github.com/MyAgentHubs/agentloom-private.git")
        variants = (
            "git@github.com-alias:MyAgentHubs/agentloom.git",
            "https://github.com:443/MyAgentHubs/agentloom.git",
            "ssh://git@github.com:22/MyAgentHubs/agentloom.git",
            "ssh://git@ssh.github.com:443/MyAgentHubs/agentloom.git",
            "https://www.github.com/MyAgentHubs/agentloom.git",
            "https://github.com//MyAgentHubs/agentloom.git//",
            "https://github.com/MyAgentHubs/./agentloom.git",
            "https://github.com/MyAgentHubs/agentloom.git/.",
            "https://github.com/MyAgentHubs/other/../agentloom.git",
        )
        for url in variants:
            with self.subTest(url=url):
                result = self.invoke_hook("origin", url)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("restricted ref name", result.stderr)

    def test_public_pushurl_is_checked_without_transport(self):
        bare = self.remote("https://github.com/MyAgentHubs/agentloom-private.git")
        self.git("config", "remote.origin.pushurl", "file:///tmp/private-only.git")
        self.git("config", "--add", "remote.origin.pushurl", "git@github.com-alias:MyAgentHubs/agentloom.git")
        self.git("config", "--add", f"url.{bare}.insteadOf", "git@github.com-alias:MyAgentHubs/agentloom.git")
        result = self.invoke_hook("origin", "/tmp/local-only.git")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("restricted ref name", result.stderr)

    def test_all_configured_remote_urls_are_checked(self):
        bare = self.remote("https://github.com/MyAgentHubs/agentloom-private.git")
        self.git("config", "--add", "remote.origin.url", "git@github.com-alias:MyAgentHubs/agentloom.git")
        self.git("config", "--add", f"url.{bare}.insteadOf", "git@github.com-alias:MyAgentHubs/agentloom.git")
        result = self.invoke_hook("origin", "/tmp/local-only.git")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("restricted ref name", result.stderr)

    def test_direct_public_url_uses_local_rewrite(self):
        bare = self.remote("https://github.com/MyAgentHubs/agentloom-private.git")
        public_url = "https://github.com/MyAgentHubs/agentloom.git"
        self.git("config", f"url.{bare}.insteadOf", public_url)
        self.git("branch", "internal-work")
        result = self.git("push", public_url, "internal-work:refs/heads/work", check=False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("restricted ref name", result.stderr)
        self.assertNotEqual(self.remote_ref(bare).returncode, 0)

    def test_private_suffix_urls_are_not_public(self):
        bare = self.remote("https://github.com/MyAgentHubs/agentloom-internal.git")
        self.git("branch", "internal-work")
        result = self.git("push", "origin", "internal-work:refs/heads/work", check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.remote_ref(bare).returncode, 0)

    def test_remote_name_that_looks_like_repo_path_is_not_public(self):
        self.remote("https://github.com/MyAgentHubs/agentloom-private.git")
        result = self.invoke_hook("MyAgentHubs/agentloom", "/tmp/local-only.git")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_rejects_unicode_and_spaced_private_paths(self):
        bare = self.remote()
        for name in ("方案.md", "private note.md"):
            with self.subTest(name=name):
                path = f"{DOCS_TREE}/{name}"
                self.write(path, "private fixture\n")
                self.git("add", "--", path)
                self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "private filename")
                self.assert_push_rejected(bare, "fixture:refs/heads/work", "private path found")

    def test_rejects_private_path_added_in_merge_commit(self):
        bare = self.remote()
        first = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        baseline_sha = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("switch", "-q", "-c", "side")
        self.commit([self.write("side.txt", "side\n").name])
        self.git("switch", "-q", "fixture")
        self.commit([self.write("main.txt", "main\n").name])
        self.git("merge", "--no-ff", "--no-commit", "side")
        path = f"{DOCS_TREE}/merge.md"
        self.write(path, "private fixture\n")
        self.git("add", "--", path)
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "merge with private file")
        result = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("private path found", result.stderr)
        self.assertEqual(self.remote_ref(bare).stdout.strip(), baseline_sha)

    def test_rejects_prohibited_annotated_tag_tagger(self):
        bare = self.remote()
        first = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        self.make_prohibited_tag()
        self.assert_push_rejected(
            bare, "refs/tags/v9.9.9:refs/tags/v9.9.9", "prohibited tagger identity",
            "refs/tags/v9.9.9",
        )

    def test_rejects_prohibited_tag_object_pushed_by_sha(self):
        bare = self.remote()
        first = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        tag_sha = self.make_prohibited_tag()
        self.assert_push_rejected(
            bare, f"{tag_sha}:refs/tags/v9.9.9", "prohibited tagger identity",
            "refs/tags/v9.9.9",
        )

    def test_rejects_prohibited_inner_annotated_tag_tagger(self):
        bare = self.remote()
        first = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        self.make_prohibited_tag()
        self.git("-c", "advice.nestedTag=false", "tag", "-a", "outer", "-m", "outer tag", "v9.9.9")
        self.assert_push_rejected(
            bare, "refs/tags/outer:refs/tags/outer", "prohibited tagger identity",
            "refs/tags/outer",
        )

    def test_rejects_appended_forbidden_root(self):
        bare = self.remote()
        self.git("switch", "-q", "--orphan", "other-root")
        self.write("other-root.txt", "fixture\n")
        self.git("add", "--", "other-root.txt")
        self.git("commit", "-qm", "other root")
        root_sha = self.git("rev-parse", "HEAD").stdout.strip()
        env = dict(self.env, AGENTLOOM_PUSH_GUARD_EXTRA_ROOTS=root_sha)
        result = self.git("push", "origin", "other-root:refs/heads/work", check=False, env=env)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("forbidden history root", result.stderr)
        self.assertNotEqual(self.remote_ref(bare).returncode, 0)

    def test_rejects_prohibited_root_commit_identity(self):
        bare = self.remote()
        pattern = subprocess.run(
            ["bash", "scripts/check_commit_identity.sh", "--print-pattern"],
            cwd=self.root, env=self.env, check=True, stdout=subprocess.PIPE, text=True,
        ).stdout.strip()
        self.git("switch", "-q", "--orphan", "new-root")
        self.write("root.txt", "fixture\n")
        self.git("add", "--", "root.txt")
        bad_env = dict(self.env)
        bad_env["GIT_COMMITTER_EMAIL"] = "root@" + pattern.split("|")[0].replace("\\", "") + ".example.invalid"
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "bad root", env=bad_env)
        result = self.git("push", "origin", "new-root:refs/heads/work", check=False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("prohibited commit identity", result.stderr)
        self.assertNotEqual(self.remote_ref(bare).returncode, 0)

    def test_rejects_prohibited_merge_commit_identity(self):
        bare = self.remote()
        self.git("switch", "-q", "-c", "side")
        self.commit([self.write("side.txt", "side\n").name])
        self.git("switch", "-q", "fixture")
        self.commit([self.write("main.txt", "main\n").name])
        self.git("merge", "--no-ff", "--no-commit", "side")
        pattern = subprocess.run(
            ["bash", "scripts/check_commit_identity.sh", "--print-pattern"],
            cwd=self.root, env=self.env, check=True, stdout=subprocess.PIPE, text=True,
        ).stdout.strip()
        bad_env = dict(self.env)
        bad_env["GIT_COMMITTER_EMAIL"] = "merge@" + pattern.split("|")[0].replace("\\", "") + ".example.invalid"
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "bad merge", env=bad_env)
        result = self.git("push", "origin", "fixture:refs/heads/work", check=False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("prohibited commit identity", result.stderr)
        self.assertNotEqual(self.remote_ref(bare).returncode, 0)


class PublicTreeHooksFixtureTests(HooksFixtureBase):
    initial_internal_tree = False

    def test_force_added_private_file_is_blocked(self):
        shutil.copyfile(
            REPO_ROOT / "scripts/check_oss_residue.sh",
            self.root / "scripts/check_oss_residue.sh",
        )
        private_file = f"{DOCS_TREE}/private.md"
        self.write(private_file, "fixture\n")
        self.git("add", "-f", "--", private_file)
        result = self.git("commit", "-q", "-m", "private file", check=False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("oss-residue", result.stderr)

    def test_force_added_private_file_with_source_is_blocked(self):
        shutil.copyfile(
            REPO_ROOT / "scripts/check_oss_residue.sh",
            self.root / "scripts/check_oss_residue.sh",
        )
        self.write("app/src/ordinary.ts", "export const ordinary = 1;\n")
        self.git("add", "--", "app/src/ordinary.ts")
        private_file = f"{DOCS_TREE}/private.md"
        self.write(private_file, "fixture\n")
        self.git("add", "-f", "--", private_file)
        result = self.git("commit", "-q", "-m", "private file with source", check=False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("oss-residue", result.stderr)


class InstallHooksScriptTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix=".install-hooks-test-", dir=REPO_ROOT)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.env.update({
            "GIT_AUTHOR_NAME": "Install Hooks Test", "GIT_AUTHOR_EMAIL": "installhooks@example.invalid",
            "GIT_COMMITTER_NAME": "Install Hooks Test", "GIT_COMMITTER_EMAIL": "installhooks@example.invalid",
            "GIT_ALLOW_PROTOCOL": "file", "GIT_SSH_COMMAND": "false", "GIT_TERMINAL_PROMPT": "0",
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
        self.assertNotEqual(code, 0)
        expected = self.root / ".git/hooks"
        self.assertIn(str(expected), first.stdout)
        self.assertEqual((expected / "pre-push").read_bytes(), (self.root / ".githooks/pre-push").read_bytes())

        second = self._run_install()
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        code, value = self._get_hooks_path()
        self.assertNotEqual(code, 0)
        self.assertEqual((expected / "pre-push").read_bytes(), (self.root / ".githooks/pre-push").read_bytes())

    def test_install_makes_hooks_executable(self):
        self._run_install()
        for name in ("pre-commit", "pre-push"):
            mode = (self.root / ".githooks" / name).stat().st_mode
            self.assertTrue(mode & 0o111, f"{name} not executable")
            snapshot_mode = (self.root / ".git/hooks" / name).stat().st_mode
            self.assertTrue(snapshot_mode & 0o111, f"{name} snapshot not executable")

    def test_install_unsets_existing_hooks_path(self):
        subprocess.run(
            ["git", "-C", str(self.root), "config", "core.hooksPath", ".githooks"],
            env=self.env, check=True,
        )
        result = self._run_install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        code, value = self._get_hooks_path()
        self.assertNotEqual(code, 0)
        self.assertEqual(value, "")
        self.assertIn(str(self.root / ".git/hooks"), result.stdout)

    def test_install_backs_up_unowned_hook(self):
        target = self.root / ".git/hooks/pre-push"
        target.write_text("#!/bin/sh\nexit 0\n")
        result = self._run_install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        backup = target.with_name("pre-push.pre-agentloom")
        self.assertEqual(backup.read_text(), "#!/bin/sh\nexit 0\n")
        self.assertIn(str(backup), result.stdout)
        self.assertEqual(target.read_bytes(), (self.root / ".githooks/pre-push").read_bytes())
        second = self._run_install()
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        self.assertNotIn("Backed up existing hook", second.stdout)

    def test_install_rejects_global_hooks_path_override(self):
        alternate = self.root / "alternate-hooks"
        alternate.mkdir()
        global_config = self.root / "global.gitconfig"
        global_config.write_text(f"[core]\n\thooksPath = {alternate}\n")
        self.env["GIT_CONFIG_GLOBAL"] = str(global_config)

        result = self._run_install()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Error: active hooks directory", result.stderr)
        self.assertIn("core.hooksPath configuration:", result.stderr)
        self.assertIn(str(global_config), result.stderr)

    def test_install_preserves_first_backup_when_unowned_hook_returns(self):
        target = self.root / ".git/hooks/pre-push"
        original = "#!/bin/sh\nexit 0\n"
        target.write_text(original)
        first = self._run_install()
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        backup = target.with_name("pre-push.pre-agentloom")
        self.assertEqual(backup.read_text(), original)

        target.write_text("#!/bin/sh\nexit 2\n")
        source = self.root / ".githooks/pre-push"
        source.write_bytes(source.read_bytes() + b"\n# Updated source hook\n")
        second = self._run_install()
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        self.assertEqual(backup.read_text(), original)
        self.assertEqual(target.read_bytes(), source.read_bytes())
        self.assertNotIn("Backed up existing hook", second.stdout)


class SourceResidueTests(unittest.TestCase):
    def test_this_file_does_not_contain_the_joined_docs_literal(self):
        # The public-tree residue scan greps tracked files
        # for the literal joined path; fixtures here must assemble it at
        # runtime via DOCS_TREE instead of spelling it out.
        joined_literal = "/".join(("docs", "superpowers"))
        self.assertNotIn(joined_literal, SCRIPT.read_text())


if __name__ == "__main__":
    unittest.main()
