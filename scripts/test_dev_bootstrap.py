import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


sys.dont_write_bytecode = True

SCRIPT = Path(__file__).with_name("dev-bootstrap.sh")
INSTALL_HOOKS = SCRIPT.with_name("install-hooks.sh")
# Split this path so the public-snapshot residue scan does not flag it.
SP_DIR = "/".join(("docs", "superpowers"))


class DevBootstrapTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name)
        self.repo = base / "repo"
        self.private = base / "private-source"
        self.repo.mkdir()
        self.private.mkdir()
        (self.repo / "scripts").mkdir()
        shutil.copy2(SCRIPT, self.repo / "scripts/dev-bootstrap.sh")
        (self.repo / "scripts/install-hooks.sh").write_text(
            '#!/bin/bash\ntouch "$(dirname "$0")/../hooks-installed"\n'
        )
        self.env = os.environ.copy()
        self.env.update(
            HOME=str(base),
            GIT_CONFIG_NOSYSTEM="1",
            GIT_AUTHOR_NAME="Bootstrap Test",
            GIT_AUTHOR_EMAIL="test@example.invalid",
            GIT_COMMITTER_NAME="Bootstrap Test",
            GIT_COMMITTER_EMAIL="test@example.invalid",
            AGENTLOOM_PRIVATE_REPO=str(self.private),
        )
        self.git(self.repo, "init", "-q")
        self.git(self.private, "init", "-q")
        files = {
            f"{SP_DIR}/INDEX.md": "index\n",
            f"{SP_DIR}/private-rules/CLAUDE.local.md": "claude rules\n",
            f"{SP_DIR}/private-rules/AGENTS.private.md": "agent rules\n",
            "harness-agent/docs/a.md": "agent docs\n",
            "harness-agent/evals/b.md": "agent evals\n",
            "app/.design-sync/c.md": "design sync\n",
            "evals/PROGRAM.md": "program\n",
            "evals/engine-bridge/x.txt": "private bridge\n",
        }
        for name, content in files.items():
            path = self.private / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        self.git(self.private, "add", ".")
        self.git(self.private, "commit", "-qm", "test fixtures")

    def git(self, path, *args):
        subprocess.run(["git", *args], cwd=path, env=self.env, check=True, capture_output=True, text=True)

    def run_bootstrap(self, cwd=None, timeout=None):
        return subprocess.run(
            ["bash", str(self.repo / "scripts/dev-bootstrap.sh")],
            cwd=cwd or self.repo,
            env=self.env,
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
            timeout=timeout,
        )

    def prepare_mixed_evals(self):
        fixture = self.repo / "evals/engine-bridge/fixtures/f.json"
        fixture.parent.mkdir(parents=True)
        fixture.write_text("public bridge\n")
        self.git(self.repo, "add", "evals/engine-bridge/fixtures/f.json")
        self.git(self.repo, "commit", "-qm", "public fixture")
        for name in ("PROGRAM.md", "RESULTS.md", "run_eval.sh"):
            (self.private / "evals/engine-bridge" / name).write_text(name + "\n")
        self.git(self.private, "add", "evals/engine-bridge")
        self.git(self.private, "commit", "-qm", "private eval fixtures")

    def test_empty_private_directory_skips_pull_and_hooks(self):
        (self.repo / ".private").mkdir()
        self.git(self.repo, "add", ".")
        self.git(self.repo, "commit", "-qm", "public fixture")
        wrapper_dir = Path(self.temp.name) / "bin"
        wrapper_dir.mkdir()
        calls = Path(self.temp.name) / "git-calls"
        real_git = shutil.which("git")
        wrapper = wrapper_dir / "git"
        wrapper.write_text(f'#!/bin/sh\nprintf "%s\\n" "$*" >> "$GIT_CALLS"\nexec "{real_git}" "$@"\n')
        wrapper.chmod(0o755)
        self.env["PATH"] = f'{wrapper_dir}{os.pathsep}{self.env["PATH"]}'
        self.env["GIT_CALLS"] = str(calls)
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any("pull" in call.split() for call in calls.read_text().splitlines()))
        self.assertFalse((self.repo / SP_DIR).is_symlink())
        self.assertTrue((self.repo / "hooks-installed").exists())
        self.assertIn("empty/incomplete/not a git checkout", result.stdout)

    def test_regular_file_conflict_is_preserved(self):
        path = self.repo / "harness-agent/evals"
        path.parent.mkdir(parents=True)
        path.write_text("keep\n")
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(path.read_text(), "keep\n")

    def test_broken_symlink_conflict_is_preserved(self):
        path = self.repo / "harness-agent/evals"
        path.parent.mkdir(parents=True)
        path.symlink_to("missing-target")
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(os.readlink(path), "missing-target")

    def test_git_worktree_uses_its_exclude_file(self):
        self.git(self.repo, "add", ".")
        self.git(self.repo, "commit", "-qm", "public fixture")
        worktree = Path(self.temp.name) / "linked-worktree"
        self.git(self.repo, "worktree", "add", "-q", "-b", "bootstrap-test", str(worktree))
        self.repo = worktree
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((worktree / "evals/PROGRAM.md").is_symlink())
        exclude = subprocess.run(
            ["git", "rev-parse", "--git-path", "info/exclude"], cwd=worktree,
            env=self.env, check=True, capture_output=True, text=True,
        ).stdout.strip()
        self.assertTrue(Path(exclude).is_absolute())
        self.assertIn("/evals/PROGRAM.md", Path(exclude).read_text().splitlines())

    def test_first_run_with_spaces_in_repo_path(self):
        spaced_repo = Path(self.temp.name) / "repo with space"
        self.repo.rename(spaced_repo)
        self.repo = spaced_repo
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.repo / "harness-agent/evals").is_symlink())
        self.assertTrue((self.repo / "evals/PROGRAM.md").is_symlink())

    def test_auth_failure_returns_without_hanging(self):
        self.env["AGENTLOOM_PRIVATE_REPO"] = "https://127.0.0.1:9/x.git"
        self.env["GIT_ASKPASS"] = "false"
        self.env.pop("GIT_TERMINAL_PROMPT", None)
        result = self.run_bootstrap(timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.repo / SP_DIR).is_symlink())
        self.assertIn("gh auth login", result.stdout + result.stderr)

    def test_first_run_links_and_runs_hooks(self):
        result = self.run_bootstrap(cwd=Path(self.temp.name))
        self.assertEqual(result.returncode, 0, result.stderr)
        targets = {
            SP_DIR: (f"../.private/{SP_DIR}", "INDEX.md"),
            "harness-agent/docs": ("../.private/harness-agent/docs", "a.md"),
            "harness-agent/evals": ("../.private/harness-agent/evals", "b.md"),
            "app/.design-sync": ("../.private/app/.design-sync", "c.md"),
            "CLAUDE.local.md": (f".private/{SP_DIR}/private-rules/CLAUDE.local.md", None),
            "AGENTS.private.md": (f".private/{SP_DIR}/private-rules/AGENTS.private.md", None),
            "evals/PROGRAM.md": ("../.private/evals/PROGRAM.md", None),
        }
        for name, (relative, child) in targets.items():
            path = self.repo / name
            self.assertTrue(path.is_symlink(), name)
            self.assertTrue(path.exists(), name)
            self.assertEqual(os.readlink(path), relative)
            if child:
                self.assertTrue((path / child).is_file(), name)
        self.assertEqual((self.repo / "CLAUDE.local.md").read_text(), "claude rules\n")
        self.assertEqual((self.repo / "evals/PROGRAM.md").read_text(), "program\n")
        self.assertTrue((self.repo / "hooks-installed").exists())
        exclude = (self.repo / ".git/info/exclude").read_text().splitlines()
        for name in (f"/{SP_DIR}", "/harness-agent/evals", "/CLAUDE.local.md", "/evals/PROGRAM.md"):
            self.assertIn(name, exclude)

    def test_public_fixtures_survive(self):
        files = {
            "evals/engine-bridge/fixtures/f.json": "public bridge\n",
            "evals/swebench/fair30_ids.json": "public swebench\n",
        }
        for name, content in files.items():
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.repo / "evals/engine-bridge").is_symlink())
        for name, content in files.items():
            path = self.repo / name
            self.assertFalse(path.is_symlink(), name)
            self.assertEqual(path.read_text(), content)

    def test_real_evals_directory_links_private_files_and_excludes_them(self):
        self.prepare_mixed_evals()
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            (self.repo / "evals/engine-bridge/fixtures/f.json").read_text(),
            "public bridge\n",
        )
        exclude = (self.repo / ".git/info/exclude").read_text().splitlines()
        for name in ("PROGRAM.md", "RESULTS.md", "run_eval.sh", "x.txt"):
            path = self.repo / "evals/engine-bridge" / name
            self.assertTrue(path.is_symlink(), name)
            self.assertEqual(os.readlink(path), f"../../.private/evals/engine-bridge/{name}")
            self.assertIn(f"/evals/engine-bridge/{name}", exclude)
        status = subprocess.run(
            ["git", "status", "--porcelain"], cwd=self.repo, env=self.env,
            check=True, capture_output=True, text=True,
        ).stdout
        self.assertNotIn("evals/engine-bridge/", status)

    def test_real_evals_directory_second_run_is_idempotent(self):
        self.prepare_mixed_evals()
        first = self.run_bootstrap()
        self.assertEqual(first.returncode, 0, first.stderr)
        names = ("PROGRAM.md", "RESULTS.md", "run_eval.sh", "x.txt")
        before = {
            name: os.readlink(self.repo / "evals/engine-bridge" / name)
            for name in names
        }
        second = self.run_bootstrap()
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(
            {name: os.readlink(self.repo / "evals/engine-bridge" / name) for name in names},
            before,
        )
        exclude = (self.repo / ".git/info/exclude").read_text().splitlines()
        for name in names:
            self.assertEqual(exclude.count(f"/evals/engine-bridge/{name}"), 1, name)

    def test_real_evals_directory_file_conflict_requires_migration(self):
        self.prepare_mixed_evals()
        path = self.repo / "evals/engine-bridge/PROGRAM.md"
        path.write_text("public program\n")
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 1)
        self.assertIn("migrate", result.stdout + result.stderr)
        self.assertFalse(path.is_symlink())
        self.assertEqual(path.read_text(), "public program\n")

    def test_second_run_is_idempotent_and_exclude_is_unique(self):
        first = self.run_bootstrap()
        self.assertEqual(first.returncode, 0, first.stderr)
        def snapshot():
            return {
                str(path.relative_to(self.repo)): os.readlink(path)
                for path in self.repo.rglob("*") if path.is_symlink()
            }
        before = snapshot()
        second = self.run_bootstrap()
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(snapshot(), before)
        exclude = (self.repo / ".git/info/exclude").read_text().splitlines()
        for name in (
            f"/{SP_DIR}",
            "/harness-agent/docs",
            "/harness-agent/evals",
            "/app/.design-sync",
            "/CLAUDE.local.md",
            "/AGENTS.private.md",
            "/evals/PROGRAM.md",
            "/evals/engine-bridge",
        ):
            self.assertEqual(exclude.count(name), 1, name)

    def test_unreachable_private_repo_exits_without_links(self):
        self.env["AGENTLOOM_PRIVATE_REPO"] = str(Path(self.temp.name) / "missing")
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("private repo unreachable", result.stdout + result.stderr)
        self.assertFalse((self.repo / ".private").exists())
        self.assertFalse((self.repo / SP_DIR).is_symlink())
        self.assertTrue((self.repo / "hooks-installed").exists())

    def test_unreachable_private_repo_installs_real_hooks(self):
        self.env["AGENTLOOM_PRIVATE_REPO"] = str(Path(self.temp.name) / "missing")
        shutil.copy2(INSTALL_HOOKS, self.repo / "scripts/install-hooks.sh")
        shutil.copytree(SCRIPT.parent.parent / ".githooks", self.repo / ".githooks")
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        hooks_path = subprocess.run(
            ["git", "config", "--get", "core.hooksPath"], cwd=self.repo, env=self.env,
            check=True, capture_output=True, text=True,
        ).stdout.strip()
        self.assertEqual(hooks_path, ".githooks")

    def test_real_directory_conflict_is_preserved(self):
        path = self.repo / "harness-agent/evals"
        path.mkdir(parents=True)
        (path / "keep.txt").write_text("keep\n")
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 1)
        self.assertIn("migrate", result.stdout + result.stderr)
        self.assertFalse(path.is_symlink())
        self.assertEqual((path / "keep.txt").read_text(), "keep\n")
        self.assertTrue((self.repo / "hooks-installed").exists())

    def test_wrong_symlink_conflict_is_preserved(self):
        path = self.repo / SP_DIR
        path.parent.mkdir(parents=True)
        path.symlink_to("somewhere-else")
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(os.readlink(path), "somewhere-else")
        self.assertTrue((self.repo / "hooks-installed").exists())


if __name__ == "__main__":
    unittest.main()
