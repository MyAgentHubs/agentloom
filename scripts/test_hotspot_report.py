#!/usr/bin/env python3

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import check_file_size
import hotspot_report


SCRIPT = SCRIPT_DIR / "hotspot_report.py"


class HotspotReportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.git("init")
        self.git("config", "user.email", "hotspot@example.test")
        self.git("config", "user.name", "Hotspot Test")
        self.default_branch = self.git("branch", "--show-current").stdout.strip()

    def tearDown(self):
        self.temp.cleanup()

    def git(self, *args):
        return subprocess.run(
            ["git", *args],
            cwd=self.root,
            check=True,
            capture_output=True,
            text=True,
        )

    def write(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def commit(self, subject):
        self.git("add", ".")
        self.git("commit", "-m", subject)

    def report(self):
        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--days",
                "3650",
                "--format",
                "json",
                "--top",
                "50",
            ],
            cwd=self.root,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def direct_report(self):
        return hotspot_report.build_report(self.root, 3650, 50, "HEAD")

    def row(self, report, path):
        return next(item for item in report["files"] if item["path"] == path)

    def test_merge_commit_is_excluded(self):
        target = "app/src-tauri/src/foo.rs"
        self.write(target, "fn value() -> i32 { 1 }\n")
        self.commit("initial")
        self.git("checkout", "-b", "feature")
        self.write(target, "fn value() -> i32 { 2 }\n")
        self.commit("feature change")
        self.git("checkout", self.default_branch)
        self.write("README.md", "main line\n")
        self.commit("main change")
        self.git("merge", "--no-ff", "feature", "-m", "merge feature")

        self.assertEqual(self.row(self.report(), target)["churn"], 2)

    def test_refactor_and_chore_commits_are_excluded(self):
        target = "app/src-tauri/src/foo.rs"
        self.write(target, "fn value() -> i32 { 1 }\n")
        self.commit("initial")
        self.write(target, "fn value() -> i32 { 2 }\n")
        self.commit("refactor: reorganize value")
        self.write(target, "fn value() -> i32 { 3 }\n")
        self.commit("chore: refresh value")

        self.assertEqual(self.row(self.report(), target)["churn"], 1)

    def test_optional_block_cap_reports_warning_status(self):
        target = "app/src/warn_band.ts"
        self.write(target, "export const value = 0;\n" * 700)
        self.commit("initial")

        row = self.row(self.direct_report(), target)

        self.assertEqual(row["status"], "超提醒")
        self.assertEqual(row["block_cap"], 1000)

    def test_optional_block_cap_reports_blocking_status(self):
        target = "app/src-tauri/src/over_block.rs"
        self.write(target, "fn value() {}\n" * 1200)
        self.commit("initial")

        row = self.row(self.direct_report(), target)

        self.assertEqual(row["status"], "超拦截")
        self.assertEqual(row["block_cap"], 1000)

    def test_missing_block_cap_preserves_existing_status(self):
        target = "app/src-tauri/src/foo.rs"
        self.write(target, "fn value() {}\n" * 801)
        self.commit("initial")

        legacy_check_file_size = types.SimpleNamespace()
        with patch.object(hotspot_report, "check_file_size", legacy_check_file_size):
            self.assertFalse(hasattr(hotspot_report.check_file_size, "block_cap"))
            row = self.row(self.direct_report(), target)

        self.assertEqual(row["status"], "超")
        self.assertIsNone(row["block_cap"])

    def test_bulk_commit_over_forty_files_is_excluded(self):
        target = "app/src-tauri/src/foo.rs"
        self.write(target, "fn value() -> i32 { 1 }\n")
        self.commit("initial")
        self.write(target, "fn value() -> i32 { 2 }\n")
        for index in range(40):
            self.write(f"bulk/file_{index}.txt", f"{index}\n")
        self.commit("bulk update")

        self.assertEqual(self.row(self.report(), target)["churn"], 1)

    def test_high_similarity_rename_is_excluded(self):
        old_path = "app/src-tauri/src/old.rs"
        new_path = "app/src-tauri/src/new.rs"
        content = "".join(f"const V{index}: i32 = {index};\n" for index in range(80))
        self.write(old_path, content)
        self.commit("initial")
        self.git("mv", old_path, new_path)
        self.write(new_path, content.replace("const V0: i32 = 0;", "const V0: i32 = 1;"))
        self.commit("rename module")

        self.assertEqual(self.row(self.report(), new_path)["churn"], 1)

    def test_cochange_averages_distinct_two_segment_directories(self):
        target = "app/src-tauri/src/foo.rs"
        self.write(target, "fn value() -> i32 { 1 }\n")
        self.commit("initial")
        self.write(target, "fn value() -> i32 { 2 }\n")
        self.write("app/src/peer.ts", "export const peer = 1;\n")
        self.write("harness-agent/src/peer.rs", "pub fn peer() {}\n")
        self.commit("cross directory change")

        row = self.row(self.report(), target)
        self.assertEqual(row["churn"], 2)
        self.assertEqual(row["cochange"], 2.0)

    def test_test_files_are_not_reported(self):
        production = "app/src-tauri/src/foo.rs"
        rust_test = "app/src-tauri/tests/integration.rs"
        cfg_test = "app/src-tauri/src/test_only.rs"
        self.write(production, "pub fn value() {}\n")
        self.write(rust_test, "fn integration_test() {}\n")
        self.write(cfg_test, "#![cfg(test)]\nfn unit_test() {}\n")
        self.commit("add sources")

        paths = {item["path"] for item in self.report()["files"]}
        self.assertIn(production, paths)
        self.assertNotIn(rust_test, paths)
        self.assertNotIn(cfg_test, paths)

    def test_score_sorting_places_higher_score_first(self):
        low = "app/src-tauri/src/a.rs"
        high = "app/src-tauri/src/b.rs"
        self.write(low, "pub fn a() -> i32 { 1 }\n")
        self.write(high, "pub fn b() -> i32 { 1 }\n")
        self.commit("initial")
        self.write(high, "pub fn b() -> i32 { 2 }\n")
        self.commit("change b")

        report = self.report()
        paths = [item["path"] for item in report["files"]]
        self.assertLess(paths.index(high), paths.index(low))
        self.assertGreater(self.row(report, high)["score"], self.row(report, low)["score"])


if __name__ == "__main__":
    unittest.main()
