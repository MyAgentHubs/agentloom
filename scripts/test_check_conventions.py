#!/usr/bin/env python3
"""Regression checks in disposable repositories; no changes to the real index/ref."""

import importlib.util
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True
SCRIPT = Path(__file__).resolve().with_name("check_conventions.py")
spec = importlib.util.spec_from_file_location("conventions_gate", SCRIPT)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class UnitTests(unittest.TestCase):
    def test_comment_line_detection(self):
        for extension, line, expected in (
            (".ts", "// hello", True),
            (".ts", "///doc", True),
            (".rs", "//! module doc", True),
            (".rs", "/* block */", True),
            (".rs", " * continuation", True),
            (".tsx", "{/* jsx comment */}", True),
            (".css", "<!-- html-ish -->", True),
            (".ts", "const x = 1; // trailing", False),
            (".py", "# a comment", True),
            (".sh", "# a comment", True),
            (".yml", "# a comment", True),
            (".py", "x = 1  # not line-start", False),
            (".py", "", False),
            (".py", "   ", False),
        ):
            with self.subTest(extension=extension, line=line):
                self.assertEqual(gate.is_comment_line(line, extension, False), expected)

    def test_shebang_exempt_only_on_first_line(self):
        self.assertFalse(gate.is_comment_line("#!/usr/bin/env python3", ".py", True))
        self.assertTrue(gate.is_comment_line("#!/usr/bin/env python3", ".py", False))

    def test_date_pattern(self):
        for line, expected in (
            ("// checked on 2026-09-19", True),
            ("// see 20260919 for the run", False),
        ):
            with self.subTest(line=line):
                self.assertEqual(bool(gate.DATE_RE.search(line)), expected)

    def test_date_pattern_alone_does_not_make_a_line_a_comment(self):
        # A non-comment line containing a date-shaped string must not be
        # counted; only is_comment_line() gates whether DATE_RE is applied.
        dated, ledger, cjk = gate.count_comment_stats("let x = \"2026-09-19\";\n", ".ts")
        self.assertEqual((dated, ledger, cjk), (0, 0, 0))

    def test_ledger_pattern(self):
        internal_doc_path = "/".join(("docs", "superpowers"))  # avoid the literal; see check_conventions.py
        positives = ["T12", "T1", "续64", "B2", "PR #3", internal_doc_path, "HANDOFF", "roadmap.html", "TRACKER"]
        for token in positives:
            with self.subTest(token=token):
                self.assertTrue(gate.LEDGER_RE.search(token), token)
        for token in ["T12abc", "Tx", "plain text", "Bx"]:
            with self.subTest(token=token):
                self.assertFalse(gate.LEDGER_RE.search(token), token)

    def test_source_files_do_not_embed_internal_doc_path_literal(self):
        # scripts/ ships in the public snapshot; the residue scan greps the
        # whole tree for this literal, so it must only ever be assembled.
        forbidden = "/".join(("docs", "superpowers"))
        for path in (SCRIPT, Path(__file__).resolve()):
            with self.subTest(path=path):
                self.assertNotIn(forbidden, path.read_text())

    def test_cjk_pattern(self):
        self.assertTrue(gate.CJK_RE.search("这是中文"))
        self.assertFalse(gate.CJK_RE.search("plain english"))

    def test_count_comment_stats_only_counts_comment_lines(self):
        text = "\n".join([
            "// checked on 2026-09-19",
            "let x = \"2026-09-19\";",
            "// T12 fixed this",
            "// 中文注释",
            "// plain english note",
        ])
        dated, ledger, cjk = gate.count_comment_stats(text, ".ts")
        self.assertEqual((dated, ledger, cjk), (1, 1, 1))

    def test_scan_roots_cover_required_locations(self):
        required = {
            "app/src", "harness-agent/src", "scripts", ".github/workflows",
            ".github/ISSUE_TEMPLATE", "remote-web/src",
        }
        self.assertTrue(required.issubset(gate.SCAN_ROOTS))

    def test_block_comment_interior_lines_are_counted(self):
        # A plain (non-JSDoc) block comment has interior lines with no "*"
        # prefix; they must still be treated as comment lines.
        css = "/*\n 2026-09-19 用户拍：T12 续88\n*/\n.foo{color:red}\n"
        self.assertEqual(gate.count_comment_stats(css, ".css"), (1, 1, 1))

    def test_html_style_block_comment_interior_lines_are_counted(self):
        markup = "<!--\n2026-09-19\n-->\ncode();\n"
        dated, ledger, cjk = gate.count_comment_stats(markup, ".tsx")
        self.assertEqual(dated, 1)

    def test_single_line_block_comment_is_unaffected(self):
        self.assertEqual(gate.count_comment_stats("/* 2026-09-19 */\n", ".css"), (1, 0, 0))

    def test_block_state_reopened_later_on_the_same_line_is_tracked(self):
        # A closed block followed by a second, unclosed opener on the same
        # line must still carry "in block" into the next line.
        text = "/* a */ /*\n2026-09-19\n*/\n"
        dated, _, _ = gate.count_comment_stats(text, ".ts")
        self.assertEqual(dated, 1)

    def test_line_after_a_fully_closed_block_is_not_swept_in(self):
        text = "/* a */ let x = 1;\n2026-09-19\n"
        dated, _, _ = gate.count_comment_stats(text, ".ts")
        self.assertEqual(dated, 0)

    def test_two_single_line_blocks_on_one_line_leave_no_open_state(self):
        text = "/* a */ /* b */\n2026-09-19\n"
        dated, _, _ = gate.count_comment_stats(text, ".ts")
        self.assertEqual(dated, 0)

    def test_block_opened_after_code_on_the_line_is_still_tracked(self):
        text = "x = 1; /*\n2026-09-19\n*/"
        dated, _, _ = gate.count_comment_stats(text, ".ts")
        self.assertEqual(dated, 1)

    def test_block_delimiter_inside_rust_string_is_ignored(self):
        line = 'let g = "agentloom/*";'
        self.assertEqual(gate.comment_flags([line], ".rs"), [False])
        self.assertIsNone(gate.scan_block_state(line, None, ".rs"))

    def test_block_delimiter_inside_slash_comment_does_not_leak_state(self):
        lines = ["// clean up agentloom/* refs", "let x = 1;"]
        for extension in (".rs", ".ts"):
            with self.subTest(extension=extension):
                self.assertEqual(gate.comment_flags(lines, extension), [True, False])
                self.assertIsNone(gate.scan_block_state(lines[0], None, extension))

    def test_block_delimiter_inside_typescript_string_is_ignored(self):
        line = 'const u = "https://x/*y";'
        self.assertEqual(gate.comment_flags([line], ".ts"), [False])
        self.assertIsNone(gate.scan_block_state(line, None, ".ts"))

    def test_block_delimiters_inside_template_string_are_ignored(self):
        lines = ["const tpl = `hello /* not a comment */ world`;", "const x = 1;"]
        self.assertEqual(gate.comment_flags(lines, ".ts"), [False, False])
        self.assertIsNone(gate.scan_block_state(lines[0], None, ".ts"))

    def test_rust_lifetime_does_not_hide_delimiter_inside_string(self):
        line = 'fn f<\'a>(s: &\'a str) { let x = "a/*b"; }'
        self.assertEqual(gate.comment_flags([line], ".rs"), [False])
        self.assertIsNone(gate.scan_block_state(line, None, ".rs"))

    def test_real_multiline_block_comment_still_carries_state(self):
        lines = ["let x = 1; /* opens", "still inside */ let y = 2;"]
        self.assertEqual(gate.comment_flags(lines, ".rs"), [False, True])

    def test_single_line_block_comment_leaves_no_open_state(self):
        line = "/* single line */ code"
        self.assertEqual(gate.comment_flags([line], ".rs"), [True])
        self.assertIsNone(gate.scan_block_state(line, None, ".rs"))

    def test_rust_lifetime_does_not_hide_real_block_opener(self):
        lines = ["fn f<'a>(s: &'a str) { /* real block", "let x = 1;"]
        self.assertEqual(gate.comment_flags(lines, ".rs"), [False, True])
        self.assertEqual(gate.scan_block_state(lines[0], None, ".rs"), "*/")

    def test_block_after_string_is_detected_and_closed(self):
        line = '"str" /* after string */'
        self.assertEqual(gate.comment_flags([line], ".rs"), [False])
        self.assertIsNone(gate.scan_block_state(line, None, ".rs"))

    def test_unmatched_rust_quote_does_not_hide_real_block_opener(self):
        lines = [
            "let c = '\"'; /* 2026-01-01 用户拍 T99 debt",
            "还有中文债务 continued",
            "block still open",
            "closing */ fn f(){}",
        ]
        self.assertEqual(gate.comment_flags(lines, ".rs"), [False, True, True, True])
        dated, ledger, _ = gate.count_comment_stats("\n".join(lines), ".rs")
        self.assertEqual((dated, ledger), (0, 0))

    def test_unmatched_typescript_quote_does_not_hide_real_block_opener(self):
        lines = ["const re = /\"/; /* open", "still */ x()"]
        self.assertEqual(gate.comment_flags(lines, ".ts"), [False, True])

    def test_closed_string_is_skipped_before_real_block_opener(self):
        lines = ['let s = "a/*b"; /* real', 'next line']
        self.assertEqual(gate.comment_flags(lines, ".rs"), [False, True])

    def test_html_block_comment_still_carries_state(self):
        lines = ["<!-- html", "-->"]
        self.assertEqual(gate.comment_flags(lines, ".html"), [True, True])
        self.assertEqual(gate.scan_block_state(lines[0], None, ".html"), "-->")

    def test_rust_doc_attribute_counts_as_comment(self):
        rust = '#[doc = "2026-09-19 T12 续999 用户拍 中文"]\nfn f(){}\n'
        self.assertEqual(gate.count_comment_stats(rust, ".rs"), (1, 1, 1))
        rust_inner = '#![doc = "2026-09-19"]\n'
        dated, _, _ = gate.count_comment_stats(rust_inner, ".rs")
        self.assertEqual(dated, 1)

    def test_bom_is_stripped_before_comment_detection(self):
        text = "﻿// 2026-09-19 用户拍 中文\n"
        self.assertEqual(gate.decode_text(text.encode("utf-8")), "// 2026-09-19 用户拍 中文\n")


class WorkflowTests(unittest.TestCase):
    def assert_public_ci_gate(self, text):
        jobs = re.split(r"(?=^  [a-z][a-z0-9-]*:\s*$)", text, flags=re.MULTILINE)
        checker = "python3 -I scripts/check_conventions.py"
        matching = [job for job in jobs if checker in job]
        self.assertEqual(len(matching), 1, "CI must run the conventions checker in one job")
        job = matching[0]
        self.assertIn("actions/checkout@", job)
        self.assertIn("git update-ref refs/remotes/origin/main", job)
        self.assertIn("python3 -I scripts/test_check_conventions.py", job)
        self.assertLess(job.index("git update-ref refs/remotes/origin/main"), job.index(checker))
        self.assertLess(job.index(checker), job.index("python3 -I scripts/test_check_conventions.py"))
        self.assertNotRegex(job, r"(?m)^    continue-on-error:\s*true\s*$")

        steps = re.split(r"(?=^      - (?:name|uses):)", job, flags=re.MULTILINE)
        matching_steps = [step for step in steps if checker in step]
        self.assertEqual(len(matching_steps), 1, "checker must run in one CI step")
        step = matching_steps[0]
        self.assertIn("- name: Check comment conventions\n", step)
        self.assertNotRegex(step, r"(?m)^\s{8}continue-on-error:\s*true\s*$")
        self.assertRegex(step, r"(?m)^\s{8}run: python3 -I scripts/check_conventions\.py\s*$")
        self.assertNotRegex(step, r"(?m)^\s{8}run:.*(?:\|\|\s*true|;\s*true)\s*$")

    def test_workflow_job_and_step_names_are_not_stale(self):
        internal_workflow = SCRIPT.parent.parent / ".github/workflows/conventions-gate.yml"
        if internal_workflow.is_file():
            text = internal_workflow.read_text()
            self.assertIn("  conventions-gate:\n", text)
            self.assertIn("Fetch and pin conventions baseline", text)
            self.assertIn("Check comment conventions before candidate execution", text)
            self.assertNotIn("file-size-gate:", text)
            self.assertNotIn("Fetch and pin file size baseline", text)
            self.assertNotIn("Check file sizes before candidate execution", text)
        else:
            text = (SCRIPT.parent.parent / ".github/workflows/ci.yml").read_text()
            self.assertIn("  file-size-gate:\n", text)
            self.assertIn("- name: Fetch and pin file size baseline", text)
            self.assertIn("- name: Check comment conventions\n", text)

    def test_public_ci_runs_conventions_gate_after_pinning_baseline(self):
        text = (SCRIPT.parent.parent / ".github/workflows/ci.yml").read_text()
        self.assert_public_ci_gate(text)

    def test_public_ci_rejects_fail_open_conventions_gate(self):
        text = (SCRIPT.parent.parent / ".github/workflows/ci.yml").read_text()
        step_name = "      - name: Check comment conventions\n"
        run = "        run: python3 -I scripts/check_conventions.py"
        for label, modified in (
            ("job continue-on-error", text.replace("    runs-on: ubuntu-latest\n", "    continue-on-error: true\n    runs-on: ubuntu-latest\n", 1)),
            ("step continue-on-error", text.replace(step_name, step_name + "        continue-on-error: true\n", 1)),
            ("run or true", text.replace(run, run + " || true", 1)),
            ("run semicolon true", text.replace(run, run + "; true", 1)),
        ):
            with self.subTest(label=label):
                self.assertNotEqual(modified, text)
                with self.assertRaises(AssertionError):
                    self.assert_public_ci_gate(modified)


class RepositoryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix=".conventions-gate-test-", dir=SCRIPT.parent.parent)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.created = set()
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.env.update({
            "GIT_AUTHOR_NAME": "Conventions Gate Test", "GIT_AUTHOR_EMAIL": "conventions@example.invalid",
            "GIT_COMMITTER_NAME": "Conventions Gate Test", "GIT_COMMITTER_EMAIL": "conventions@example.invalid",
        })
        self.git("init", "-q")
        self.git("symbolic-ref", "HEAD", "refs/heads/fixture")
        for root in gate.SCAN_ROOTS:
            (self.root / root).mkdir(parents=True)
        (self.root / "scripts").mkdir(exist_ok=True)
        shutil.copyfile(SCRIPT, self.root / "scripts/check_conventions.py")

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

    def baseline(self):
        if self.created:
            self.git("add", "--", *(str(path) for path in sorted(self.created)))
        self.git("commit", "-q", "--allow-empty", "-m", "fixture")
        commit = self.git("rev-parse", "HEAD")
        self.git("update-ref", "refs/remotes/origin/master", commit)
        return commit

    def run_gate(self, expected):
        result = subprocess.run(
            [sys.executable, "-I", str(self.root / "scripts/check_conventions.py")],
            cwd=self.root, env=self.env,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, check=False,
        )
        self.assertEqual(result.returncode, expected, result.stdout)
        return result.stdout

    def test_at_baseline_passes(self):
        self.write("app/src/a.ts", "// checked on 2026-09-19\n// 中文\n")
        self.baseline()
        output = self.run_gate(0)
        self.assertIn("PASS", output)

    def test_one_extra_comment_line_fails(self):
        self.write("app/src/a.ts", "// checked on 2026-09-19\n")
        self.baseline()
        self.write("app/src/a.ts", "// checked on 2026-09-19\n// checked on 2026-09-20\n")
        output = self.run_gate(1)
        self.assertIn("app/src/a.ts: dated 2 > 1 (baseline)", output)

    def test_new_file_with_one_offending_line_fails(self):
        self.baseline()
        self.write("app/src/new.ts", "// 中文注释\n")
        output = self.run_gate(1)
        self.assertIn("app/src/new.ts: cjk 1 > 0 (baseline)", output)

    def test_new_file_with_zero_offending_lines_passes(self):
        self.baseline()
        self.write("app/src/new.ts", "// plain english note\n")
        self.run_gate(0)

    def test_file_removed_from_baseline_does_not_error(self):
        self.write("app/src/a.ts", "// 中文\n")
        self.baseline()
        (self.root / "app/src/a.ts").unlink()
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "remove")
        self.run_gate(0)

    def test_missing_baseline_exits_two(self):
        self.write("app/src/a.ts", "// plain\n")
        self.git("add", "--", str(self.root / "app/src/a.ts"))
        self.git("commit", "-q", "-m", "fixture")
        output = self.run_gate(2)
        self.assertIn("missing baseline", output)

    def test_scan_roots_are_actually_scanned(self):
        # Every SCAN_ROOTS entry, not just a sample, must actually be walked.
        # ".githooks" scripts have no file extension (the one special case).
        probes = {root: ("probe" if root == ".githooks" else "probe.ts") for root in gate.SCAN_ROOTS}

        def comment(root, extra):
            return f"# 中文{extra}\n" if root == ".githooks" else f"// 中文{extra}\n"

        for root, filename in probes.items():
            self.write(f"{root}/{filename}", comment(root, "a"))
        self.baseline()
        for root, filename in probes.items():
            self.write(f"{root}/{filename}", comment(root, "a") + comment(root, "a2"))
        output = self.run_gate(1)
        for root, filename in probes.items():
            with self.subTest(root=root):
                self.assertIn(f"{root}/{filename}: cjk 2 > 1 (baseline)", output)

    def test_public_issue_template_comments_are_scanned_against_baseline(self):
        template = ".github/ISSUE_TEMPLATE/bug_report.yml"
        self.write(template, "# Existing note\nname: Bug report\n")
        self.baseline()
        self.write(template, "# Existing note\n# 中文 2026-09-21 T12\nname: Bug report\n")
        output = self.run_gate(1)
        for label in ("dated", "ledger", "cjk"):
            with self.subTest(label=label):
                self.assertIn(f"{template}: {label} 1 > 0 (baseline)", output)

    def test_uncovered_tracked_source_fails_closed(self):
        self.baseline()
        self.write("app/other-src/evil.ts", "// 中文\n")
        self.baseline()
        output = self.run_gate(2)
        self.assertIn("app/other-src/evil.ts", output)

    def test_uncovered_tracked_source_uses_the_gate_s_own_extensions(self):
        # check_file_size.py's SOURCE_EXTENSIONS excludes .py; this gate's
        # own scanning extensions include it, and coverage must too.
        self.baseline()
        self.write("tools/evil.py", "# 中文\n")
        self.baseline()
        output = self.run_gate(2)
        self.assertIn("tools/evil.py", output)

    def test_lint_config_files_are_allowed_outside_scan_roots(self):
        self.baseline()
        self.write("app/eslint.config.mjs", "// plain\n")
        self.baseline()
        self.run_gate(0)

    def test_remote_web_lint_config_is_allowed_outside_scan_roots(self):
        self.baseline()
        self.write("remote-web/eslint.config.mjs", "// plain\n")
        self.baseline()
        self.run_gate(0)

    def test_symlinked_scanned_path_fails_closed(self):
        target = self.write("app/src/a.ts", "// 中文\n")
        self.baseline()
        target.unlink()
        target.symlink_to("nonexistent-target.ts")
        self.created.add(target)
        self.git("add", "--", str(target))
        self.git("commit", "-q", "-m", "swap for a symlink")
        output = self.run_gate(2)
        self.assertIn("symlink", output.lower())

    def test_shrink_then_regrow_uses_committed_history_not_hard_cap(self):
        self.write("app/src/a.ts", "// 一\n// 二\n// 三\n")
        self.baseline()
        self.write("app/src/a.ts", "// 一\n")
        self.baseline()
        self.write("app/src/a.ts", "// 一\n// 二\n")
        output = self.run_gate(1)
        self.assertIn("app/src/a.ts: cjk 2 > 1 (baseline)", output)

    def test_unrelated_directories_are_not_scanned(self):
        self.baseline()
        self.write("docs/a.md", "// 中文\n")
        self.write("app/src/node_modules/vendor.ts", "// 中文\n")
        self.run_gate(0)


if __name__ == "__main__":
    unittest.main()
