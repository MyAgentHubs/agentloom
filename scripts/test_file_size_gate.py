#!/usr/bin/env python3
"""Regression checks in disposable repositories; no changes to the real index/ref."""

import importlib.util
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from unittest.mock import patch


sys.dont_write_bytecode = True
SCRIPT = Path(__file__).resolve().with_name("check_file_size.py")
spec = importlib.util.spec_from_file_location("file_size_gate", SCRIPT)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


def workflow_variants():
    root = SCRIPT.parent.parent
    candidates = [root / ".github/workflows/file-size-gate.yml", root / ".github/workflows/ci.yml"]
    candidates.extend((root / "docs").glob("**/github-templates/workflows/ci.yml"))
    return [(path, "master" if path.name == "file-size-gate.yml" else "main")
            for path in candidates if path.is_file()]


def workflow_shell(path, step):
    """Execute the actual YAML run block in fixtures, not a reimplementation."""
    lines = path.read_text().splitlines()
    marker = "      - name: " + step
    index = lines.index(marker) + 1
    while lines[index] != "        run: |":
        index += 1
    body = []
    for line in lines[index + 1:]:
        if line and not line.startswith("          "):
            break
        body.append(line)
    return textwrap.dedent("\n".join(body))


class ClassificationTests(unittest.TestCase):
    def test_categories(self):
        cases = [
            ("app/src-tauri/src/conn_test.rs", b"", 800),
            ("app/src-tauri/src/xxx_test.rs", b"", 800),
            ("app/src-tauri/tests/integration.rs", b"", 1500),
            ("harness-agent/src/tests.rs", b"", 1500),
            ("harness-agent/src/tests/helper.rs", b"", 800),
            ("app/src-tauri/src/tests/helper.rs", b"", 800),
            ("harness-agent/src/tests_helpers/helper.rs", b"", 800),
            ("harness-agent/src/contest.rs", b"", 800),
            ("app/src/a.test.ts", b"", 1500),
            ("app/src/a.test.tsx", b"", 1500),
            ("app/src/a.spec.ts", b"", 1500),
            ("app/src/a.spec.tsx", b"", 1500),
            ("app/src/__tests__/helper.ts", b"", 500),
            ("remote-relay/src/room-do.js", b"", 500),
            ("remote-web/src/helper.mts", b"", 500),
            ("app/src/a_test.ts", b"", 500),
            ("app/src/a.tsx", b"", 500),
            ("app/src/__tests__/style.css", b"", 800),
        ]
        for path, data, cap in cases:
            with self.subTest(path=path):
                self.assertEqual(gate.category(PurePosixPath(path), data)[1], cap)

    def test_first_code_line(self):
        for data in (
            b"#![cfg(test)]",
            b"\n// comment\n\r\n#![cfg(test)]\r\n",
            b"//! doc\n/* outer\n /* nested */\n */\n#![cfg(test)]\n",
            b"/* comment */ #![cfg(test)] // test only\n",
            b"\xef\xbb\xbf#![cfg(test)]\n",
        ):
            with self.subTest(data=data):
                self.assertTrue(gate.starts_with_test_cfg(data))
        for data in (
            b"// #![cfg(test)]\nuse std::io;\n",
            b"/* #![cfg(test)] */\nuse std::io;\n",
            b"#![allow(dead_code)]\n#![cfg(test)]\n",
            b"use std::io;\n#![cfg(test)]\n",
            b"#![cfg(test)] mod production {}\n",
            b"#/* comment */![cfg(test)]\n",
            b"/* unterminated\n#![cfg(test)]\n",
        ):
            with self.subTest(data=data):
                self.assertFalse(gate.starts_with_test_cfg(data))

    def test_explicit_scope(self):
        for root, extensions in gate.SCAN_ROOTS.items():
            for extension in extensions:
                self.assertTrue(gate.in_scope(PurePosixPath(root, "file" + extension)))
            for excluded in gate.EXCLUDED_DIRS:
                self.assertFalse(gate.in_scope(PurePosixPath(root, excluded, "file.rs")))
        for path in ("app/src2/a.ts", "scripts/a.rs", "app/src-tauri/a.rs"):
            self.assertFalse(gate.in_scope(PurePosixPath(path)))
        for path in gate.SCAN_FILES:
            self.assertTrue(gate.in_scope(PurePosixPath(path)))

    def test_tracked_source_coverage(self):
        """A new tracked source line must force a scope/exemption policy decision."""
        for path in gate.tracked_sources(SCRIPT.parent.parent):
            with self.subTest(path=path):
                self.assertTrue(gate.in_scope(path) or gate.coverage_exempt(path), str(path))

    def test_line_counts(self):
        for data, expected in (
            (b"", 0), (b"x", 1), (b"\n", 1), (b"x\r\n", 1), (b"x\r", 1),
            ("x\u2028".encode(), 1), ("x\u2029y".encode(), 2),
            ("a\r\nb\nc\rd\u2028e\u2029f".encode(), 6),
        ):
            with self.subTest(data=data):
                self.assertEqual(gate.count_lines(data, "fixture"), expected)
                # The historical decoder must agree even for empty/short blobs.
                response = b"abc blob " + str(len(data)).encode() + b"\n" + data + b"\n"
                with patch.object(gate, "git", return_value=response):
                    self.assertEqual(gate.historical_lines(Path("."), [b"abc"])[b"abc"], expected)
        for data in (b"\xff", b"\xc0\x8a", b"\xe2\x80"):
            with self.subTest(data=data):
                with self.assertRaisesRegex(gate.GateError, "非法 UTF-8"):
                    gate.count_lines(data, "fixture")

    def test_malformed_batch_response_fails_closed(self):
        for response in (b"abc missing\n", b"abc blob 4\na\n", b"abc blob -1\n"):
            with self.subTest(response=response), patch.object(gate, "git", return_value=response):
                with self.assertRaises(gate.GateError):
                    gate.historical_lines(Path("."), [b"abc"])

    def test_ci_gate_precedes_candidate_execution(self):
        variants = workflow_variants()
        self.assertTrue(variants, "A shipping CI workflow must run the gate")
        for path, branch in variants:
            with self.subTest(path=path):
                source = path.read_text()
                gate_job = source.split("  file-size-gate:\n", 1)[1].split("\n  frontend:\n", 1)[0]
                self.assertLess(gate_job.index("Fetch and pin file size baseline"),
                                gate_job.index("python3 -I scripts/check_file_size.py"))
                self.assertLess(gate_job.index("python3 -I scripts/check_file_size.py"),
                                gate_job.index("python3 -I scripts/test_file_size_gate.py"))
                self.assertNotIn("npm ", gate_job.split("        run: |", 1)[1])
                self.assertNotIn("continue-on-error", source)
                if branch == "main":
                    for name in ("frontend", "engine", "app-backend", "windows-check"):
                        self.assertIn(f"  {name}:\n    needs: file-size-gate\n", source)
                    self.assertEqual(source.count("needs.file-size-gate.outputs.baseline"), 2)


class RepositoryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix=".file-size-gate-test-", dir=SCRIPT.parent.parent)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.created = set()
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.env.update({
            "GIT_AUTHOR_NAME": "Size Gate Test", "GIT_AUTHOR_EMAIL": "sizegate@example.invalid",
            "GIT_COMMITTER_NAME": "Size Gate Test", "GIT_COMMITTER_EMAIL": "sizegate@example.invalid",
        })
        self.git("init", "-q")
        self.git("symbolic-ref", "HEAD", "refs/heads/fixture")
        for root in gate.SCAN_ROOTS:
            (self.root / root).mkdir(parents=True)
        (self.root / "scripts").mkdir()
        shutil.copyfile(SCRIPT, self.root / "scripts/check_file_size.py")

    def git(self, *args):
        return subprocess.run(
            ["git", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null",
             "-C", str(self.root), *args],
            env=self.env, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        ).stdout.strip().decode()

    def write(self, relative, count, content=b"// line\n"):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content * count)
        self.created.add(path)
        return path

    def baseline(self):
        if self.created:
            self.git("add", "--", *(str(path) for path in sorted(self.created)))
        self.git("commit", "-q", "--allow-empty", "-m", "fixture")
        commit = self.git("rev-parse", "HEAD")
        self.git("update-ref", "refs/remotes/origin/master", commit)
        return commit

    def run_gate(self, expected, *args, env=None, cwd=None):
        result = subprocess.run(
            [sys.executable, str(self.root / "scripts/check_file_size.py"), *args],
            cwd=cwd or self.root, env=env or self.env,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, check=False,
        )
        self.assertEqual(result.returncode, expected, result.stdout)
        return result.stdout

    def test_unchanged_history_and_read_only(self):
        self.write("app/src/big.ts", 600)
        self.baseline()
        before = self.git("status", "--porcelain", "--untracked-files=all")
        output = self.run_gate(0, cwd=self.root / "app")
        self.assertIn("超标总量：0 行；债务总量：100 行", output)
        self.assertEqual(before, self.git("status", "--porcelain", "--untracked-files=all"))

    def test_existing_growth_and_total_excess(self):
        self.write("app/src/big.ts", 600)
        self.baseline()
        self.write("app/src/big.ts", 700)
        self.write("harness-agent/src/new.rs", 900)
        output = self.run_gate(1)
        self.assertIn("超标总量：200 行；债务总量：300 行", output)
        self.assertIn("基线历史额度", output)
        self.assertIn("新文件不享有历史额度", output)

    def test_committed_shrink_reduces_allowance(self):
        self.write("app/src-tauri/src/big.rs", 1000)
        self.baseline()
        self.write("app/src-tauri/src/big.rs", 900)
        self.baseline()
        self.write("app/src-tauri/src/big.rs", 901)
        self.assertIn(" / 901 / 900 / 基线历史额度", self.run_gate(1))

    def test_existing_below_cap_can_grow_to_cap(self):
        self.write("app/src/a.ts", 10)
        self.baseline()
        self.write("app/src/a.ts", 500)
        self.run_gate(0)
        self.write("app/src/a.ts", 501)
        self.assertIn(" / 501 / 500 / 硬上限", self.run_gate(1))

    def test_rename_has_no_history_and_test_suffix_is_production(self):
        old = self.write("app/src-tauri/src/production.rs", 900)
        self.baseline()
        self.git("mv", "--", str(old), str(old.with_name("xxx_test.rs")))
        self.assertIn("Rust 普通源文件 / 900 / 800 / 硬上限（新文件）", self.run_gate(1))

    def test_new_files_at_and_above_each_cap(self):
        self.baseline()
        for name, cap in (
            ("app/src/a.tsx", 500), ("app/src/a.css", 800),
            ("app/src/a.test.ts", 1500), ("app/src/__tests__/a.tsx", 500),
            ("app/src-tauri/src/conn_test.rs", 800),
            ("app/src-tauri/tests/a.rs", 1500), ("harness-agent/src/a.rs", 800),
            ("harness-agent/tests/a.rs", 1500),
            ("remote-web/src/a.tsx", 500), ("remote-relay/src/a.js", 500),
            ("app/src/a.js", 500), ("app/src/a.mts", 500),
        ):
            with self.subTest(name=name):
                path = self.write(name, cap)
                self.run_gate(0)
                path.write_bytes(path.read_bytes() + b"\n")
                self.assertIn(f" / {cap + 1} / {cap} / 硬上限（新文件）", self.run_gate(1))
                path.unlink()

    def test_physical_newlines_include_blank_comments_and_crlf(self):
        self.baseline()
        path = self.write("app/src/a.ts", 500, b"// comment\r\n")
        path.write_bytes(path.read_bytes() + b"unterminated last line")
        self.assertIn(" / 501 / 500 / ", self.run_gate(1))
        path.write_bytes(path.read_bytes() + b"\n")
        self.assertIn(" / 501 / 500 / ", self.run_gate(1))
        self.write("app/src/a.ts", 501, b"\n")
        self.run_gate(1)

    def test_history_uses_the_same_newline_count(self):
        path = self.write("app/src/a.ts", 600, b"// comment\r\n")
        path.write_bytes(path.read_bytes() + b"last line without newline")
        self.baseline()
        self.run_gate(0)
        path.write_bytes(path.read_bytes() + b"\n")
        self.run_gate(0)  # Terminating the existing final line adds no line.
        path.write_bytes(path.read_bytes() + b"another line")
        self.assertIn(" / 602 / 601 / 基线历史额度", self.run_gate(1))

    def test_exclusions_and_untracked_ignored_files(self):
        self.baseline()
        for excluded in gate.EXCLUDED_DIRS:
            self.write(f"app/src/{excluded}/oversized.ts", 2000)
        self.run_gate(0)
        (self.root / ".gitignore").write_text("app/src/ignored.ts\n")
        self.write("app/src/ignored.ts", 501)
        self.assertIn("ignored.ts", self.run_gate(1))

    def test_archive_and_dist_do_not_exempt_large_files(self):
        self.baseline()
        for prefix, extensions in gate.SCAN_ROOTS.items():
            for directory in ("_archive", "dist"):
                name = f"{prefix}/{directory}/huge{sorted(extensions)[0]}"
                with self.subTest(name=name):
                    path = self.write(name, 5000)
                    self.assertIn(name, self.run_gate(1))
                    path.unlink()

    def test_test_directory_names_do_not_grant_allowances(self):
        self.baseline()
        for name, actual, cap in (
            ("app/src-tauri/src/tests/huge.rs", 1400, 800),
            ("harness-agent/src/tests/helper.rs", 1400, 800),
            ("app/src/__tests__/huge.ts", 1400, 500),
        ):
            with self.subTest(name=name):
                path = self.write(name, actual, b"const VALUE: u8 = 1;\n" if name.endswith(".rs") else b"void 0;\n")
                self.assertIn(f" / {actual} / {cap} / ", self.run_gate(1))
                path.unlink()

    def test_all_newline_styles_current_and_history(self):
        for separator in (b"\n", b"\r\n", b"\r", "\u2028".encode(), "\u2029".encode()):
            with self.subTest(separator=separator):
                self.write("app/src/a.ts", 500, b"void 0;" + separator)
                self.baseline()
                self.write("app/src/a.ts", 900, b"void 0;" + separator)
                self.assertIn(" / 900 / 500 / ", self.run_gate(1))
                self.baseline()
                self.run_gate(0)
                self.write("app/src/a.ts", 901, b"void 0;" + separator)
                self.assertIn(" / 901 / 900 / 基线历史额度", self.run_gate(1))

    def test_invalid_utf8_current_and_history_fail_closed_in_order(self):
        self.baseline()
        path = self.write("app/src/a.ts", 1, b"\xff")
        output = self.run_gate(2)
        self.assertIn("非法 UTF-8", output)
        self.assertLess(output.index("基线："), output.index("ERROR："))
        self.baseline()
        path.write_bytes(b"void 0;\n")  # History must be validated even below cap.
        self.assertIn("非法 UTF-8：基线 blob", self.run_gate(2))

    def test_uncovered_tracked_source_fails_but_bench_fixtures_are_exempt(self):
        self.write("harness-agent/evals/bench/fixture.rs", 5000, b"\xff")
        self.baseline()
        self.run_gate(0)
        self.write("new-product/src/new.mts", 1)
        self.baseline()
        self.assertIn("未纳入扫描或显式目录豁免", self.run_gate(2))

    def test_tracked_dependency_directory_is_not_silently_exempt(self):
        self.write("app/src/node_modules/hidden.ts", 2000)
        self.baseline()
        self.assertIn("未纳入扫描或显式目录豁免", self.run_gate(2))

    def test_local_ref_tampering_passes_ci_is_authoritative(self):
        """Known local bypass, closed by CI fetching origin before candidate code."""
        self.created.add(self.root / "scripts/check_file_size.py")
        self.write("app/src/a.ts", 900)
        looser = self.baseline()
        self.write("app/src/a.ts", 500)
        strict = self.baseline()
        self.write("app/src/a.ts", 900)
        self.baseline()
        self.git("update-ref", "refs/remotes/origin/master", strict)
        self.run_gate(1)
        status = self.git("status", "--porcelain", "--untracked-files=all")
        self.assertEqual(status, "")
        self.git("update-ref", "refs/remotes/origin/master", looser)
        self.run_gate(0)  # By CI design, no promise to secure mutable local refs.
        self.assertEqual(status, self.git("status", "--porcelain", "--untracked-files=all"))

    def test_staged_worktree_divergence_passes_ci_checks_committed_tree(self):
        """Known local bypass: CI checks the checkout, never this staging split."""
        self.write("app/src/a.ts", 500)
        self.baseline()
        path = self.write("app/src/a.ts", 900)
        self.git("add", "--", str(path))
        self.run_gate(1)
        path.write_bytes(b"void 0;\n" * 500)
        self.assertEqual(len(self.git("show", ":app/src/a.ts").splitlines()), 900)
        self.run_gate(0)  # Intentionally no index inspection in the local gate.

    def test_main_ref_fallback_and_master_precedence(self):
        self.write("app/src/a.ts", 900)
        loose = self.baseline()
        self.git("update-ref", "refs/remotes/origin/main", loose)
        self.git("update-ref", "-d", "refs/remotes/origin/master")
        self.assertIn("基线：origin/main", self.run_gate(0))
        self.write("app/src/a.ts", 500)
        strict = self.baseline()
        self.write("app/src/a.ts", 900)
        self.assertIn(strict, self.run_gate(1))
        # An existing invalid master cannot silently fall through to valid main.
        blob = self.git("rev-parse", "HEAD:app/src/a.ts")
        self.git("update-ref", "refs/remotes/origin/master", blob)
        self.assertIn("拒绝放行", self.run_gate(2))
        (self.root / ".git/refs/remotes/origin/master").write_text("broken\n")
        self.assertIn("拒绝放行", self.run_gate(2))

    def test_historical_symlink_grants_no_allowance_and_does_not_block_repair(self):
        path = self.root / "app/src/linked.ts"
        path.symlink_to("target.ts")
        self.created.add(path)
        self.baseline()
        self.assertIn("符号链接", self.run_gate(2))
        path.unlink()
        path.write_bytes(b"void 0;\n" * 500)
        self.run_gate(0)
        path.write_bytes(b"void 0;\n" * 501)
        self.assertIn(" / 501 / 500 / 硬上限（新文件）", self.run_gate(1))
        path.unlink()
        self.run_gate(0)

    def test_cfg_test_allowance_depends_on_compile_gate(self):
        """Accepted policy: even non-test-looking code is absent in production.

        1500 is intentional; production use of its exports must fail compilation.
        CI compilation is the backstop, not this text classifier.
        """
        self.baseline()
        path = self.write("app/src-tauri/src/feature.rs", 1,
                          b"#![cfg(test)]\n" + b"// production-looking implementation\n" * 1498 + b"pub struct Y;\n")
        self.run_gate(0)
        path.write_bytes(path.read_bytes() + b"// one more\n")
        self.assertIn("Rust 测试文件 / 1501 / 1500", self.run_gate(1))
        path.write_bytes(path.read_bytes().replace(b"#![cfg(test)]\n", b"", 1))
        self.assertIn("Rust 普通源文件 / 1500 / 800", self.run_gate(1))

    def test_unicode_and_space_path_output(self):
        self.baseline()
        name = "app/src/含 空格 组件.tsx"
        self.write(name, 501)
        self.assertIn('"' + name + '"', self.run_gate(1))

    def test_public_snapshot_can_omit_only_untracked_nonhistorical_remote_roots(self):
        self.baseline()
        for prefix in gate.SNAPSHOT_OPTIONAL_ROOTS:
            (self.root / prefix).rmdir()
        self.assertIn("公开快照未包含扫描根", self.run_gate(0))
        path = self.write("remote-web/src/a.ts", 1)
        self.baseline()
        path.unlink()
        path.parent.rmdir()
        self.assertIn("拒绝放行", self.run_gate(2))

    def run_ci_shell(self, workflow, event, before, expected):
        env = dict(self.env, GITHUB_EVENT_NAME=event, SIZE_GATE_BEFORE=before,
                   GITHUB_OUTPUT=str(self.root / "ci-output"))
        result = subprocess.run(
            ["bash", "-c", workflow_shell(workflow, "Fetch and pin file size baseline")],
            cwd=self.root, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, check=False,
        )
        self.assertEqual(result.returncode, expected, result.stdout)
        return result.stdout

    def test_ci_pr_fetches_missing_ref_before_gate(self):
        for workflow, branch in workflow_variants():
            with self.subTest(branch=branch):
                self.write("app/src/a.ts", 500)
                strict = self.baseline()
                # Use a local remote transport, with no dependency on GitHub credentials.
                self.git("config", "remote.origin.url", str(self.root))
                self.git("update-ref", f"refs/heads/{branch}", strict)
                self.write("app/src/a.ts", 900)
                self.git("add", "--", str(self.root / "app/src/a.ts"))
                self.git("commit", "-q", "-m", "candidate")
                for ref in gate.BASELINE_REFS:
                    self.git("update-ref", "-d", ref)
                self.assertIn("缺少基线", self.run_gate(2))
                self.run_ci_shell(workflow, "pull_request", "", 0)
                self.assertEqual(self.git("rev-parse", f"refs/remotes/origin/{branch}"), strict)
                self.assertIn(" / 900 / 500 / ", self.run_gate(1))

    def test_ci_push_pins_before_even_for_multiple_commits_and_later_jobs(self):
        for workflow, branch in workflow_variants():
            with self.subTest(branch=branch):
                self.write("app/src/a.ts", 500)
                strict = self.baseline()
                self.write("app/src/a.ts", 900)
                self.baseline()  # First pushed commit contains the growth.
                candidate = self.baseline()  # Last commit has unchanged file size.
                self.git("config", "remote.origin.url", str(self.root))
                self.git("update-ref", f"refs/heads/{branch}", candidate)
                for ref in gate.BASELINE_REFS:
                    self.git("update-ref", "-d", ref)
                self.git("update-ref", f"refs/remotes/origin/{branch}", candidate)
                self.run_gate(0)  # Reproduce checkout's false green before pinning.
                self.run_ci_shell(workflow, "push", strict, 0)
                self.assertEqual(self.git("rev-parse", f"refs/remotes/origin/{branch}"), strict)
                self.assertIn(" / 900 / 500 / ", self.run_gate(1))
                if branch == "main":
                    self.assertIn("baseline=" + strict, (self.root / "ci-output").read_text())
                    self.git("update-ref", "refs/remotes/origin/main", candidate)
                    result = subprocess.run(
                        ["bash", "-c", workflow_shell(workflow, "Restore pinned file size baseline")],
                        cwd=self.root, env=dict(self.env, SIZE_GATE_BASELINE=strict),
                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, check=False,
                    )
                    self.assertEqual(result.returncode, 0, result.stdout)
                    self.assertIn(" / 900 / 500 / ", self.run_gate(1))

    def test_ci_first_import_and_missing_before_fail_closed(self):
        self.write("app/src/a.ts", 1)
        commit = self.baseline()
        self.git("config", "remote.origin.url", str(self.root))
        for workflow, branch in workflow_variants():
            self.git("update-ref", f"refs/heads/{branch}", commit)
            for before in ("0" * 40, "", "not-a-commit"):
                with self.subTest(branch=branch, before=before):
                    self.assertIn("first import requires a reviewed baseline",
                                  self.run_ci_shell(workflow, "push", before, 1))
            # A syntactically valid but missing previous object must also fail.
            with self.subTest(branch=branch, before="missing object"):
                self.run_ci_shell(workflow, "push", "a" * 40, 128)

    def test_ci_real_shallow_push_fetches_previous_object(self):
        self.created.add(self.root / "scripts/check_file_size.py")
        for prefix in gate.SCAN_ROOTS:
            self.write(prefix + "/.keep", 0)
        self.write("app/src/a.ts", 500)
        strict = self.baseline()
        self.write("app/src/a.ts", 900)
        self.baseline()
        candidate = self.baseline()
        for workflow, branch in workflow_variants():
            with self.subTest(branch=branch):
                self.git("update-ref", f"refs/heads/{branch}", candidate)
                checkout = self.root / ("checkout-" + branch)
                self.git("clone", "--quiet", "--depth", "1", "--branch", branch,
                         self.root.as_uri(), str(checkout))
                original = self.root
                try:
                    self.root = checkout
                    self.assertEqual(self.git("rev-parse", "--is-shallow-repository"), "true")
                    self.run_gate(0)  # Shallow checkout points the baseline at HEAD.
                    self.run_ci_shell(workflow, "push", strict, 0)
                    self.assertIn(" / 900 / 500 / ", self.run_gate(1))
                finally:
                    self.root = original

    def test_missing_ref_and_missing_repository_fail_closed(self):
        self.assertIn("拒绝放行", self.run_gate(2))
        (self.root / ".git").rename(self.root / "saved-git")
        self.assertIn("拒绝放行", self.run_gate(2))

    def test_missing_root_and_symlinks_fail_closed(self):
        self.baseline()
        root = self.root / "app/src"
        root.rmdir()
        self.assertIn("拒绝放行", self.run_gate(2))
        root.symlink_to(self.root / "harness-agent/src", target_is_directory=True)
        self.assertIn("不能是符号链接", self.run_gate(2))
        root.unlink()
        root.mkdir()
        (root / "alias").symlink_to(self.root / "harness-agent/src", target_is_directory=True)
        self.assertIn("符号链接", self.run_gate(2))
        (root / "alias").unlink()
        (root / "alias.ts").symlink_to(self.root / "missing")
        self.assertIn("符号链接", self.run_gate(2))

    def test_missing_git_fails_closed(self):
        self.baseline()
        self.assertIn("拒绝放行", self.run_gate(2, env=dict(self.env, PATH="")))

    def test_walk_errors_are_not_silently_ignored(self):
        with patch.object(gate.os, "scandir", side_effect=PermissionError("unreadable")):
            with self.assertRaises(PermissionError):
                gate.collect_current(self.root)

    def test_arguments_and_environment_cannot_override_ref(self):
        self.write("app/src/a.ts", 900)
        old = self.baseline()
        self.git("branch", "looser", old)
        self.write("app/src/a.ts", 500)
        current = self.baseline()
        self.write("app/src/a.ts", 900)
        self.assertIn("不接受命令行参数", self.run_gate(2, "--baseline", "looser"))
        env = dict(self.env, BASELINE_REF="looser", FILE_SIZE_BASELINE_REF="looser",
                   GIT_DIR=str(self.root / "missing"), GIT_WORK_TREE=str(self.root / "missing"))
        self.assertIn(current, self.run_gate(1, env=env))

    def test_git_replace_cannot_override_history(self):
        self.write("app/src/a.ts", 900)
        old = self.baseline()
        self.write("app/src/a.ts", 500)
        current = self.baseline()
        self.git("replace", current, old)
        self.write("app/src/a.ts", 900)
        self.assertIn(" / 900 / 500 / ", self.run_gate(1))

    def test_ref_name_collision_cannot_override_history(self):
        self.write("app/src/a.ts", 900)
        old = self.baseline()
        self.git("tag", "origin/master", old)
        self.git("branch", "origin/master", old)
        self.write("app/src/a.ts", 500)
        self.baseline()
        self.write("app/src/a.ts", 900)
        self.assertIn(" / 900 / 500 / ", self.run_gate(1))


if __name__ == "__main__":
    unittest.main(verbosity=2)
