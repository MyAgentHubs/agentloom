#!/usr/bin/env python3
"""Regression and pattern drift checks for check_commit_identity.sh."""

import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

SCRIPT_DIR = Path(__file__).resolve().parent
IDENTITY_SCRIPT = SCRIPT_DIR / "check_commit_identity.sh"
RESIDUE_SCRIPT = SCRIPT_DIR / "check_oss_residue.sh"
GOOD_EMAIL = "panda@myagenthubs.com"
OTHER_EMAIL = "someone@example.com"
BAD_EMAIL = "x@" + "after" + "ship.com"


def extract_scan_patterns(script_path):
    """Evaluate scan lines with the same stub used by the residue tests."""
    lines = [line for line in script_path.read_text().splitlines()
             if re.match(r'^\s*scan\s+"', line)]
    stub = "scan() { printf '%s\\x1f%s\\x1e' \"$1\" \"$2\"; }\n"
    result = subprocess.run(
        ["bash", "-c", stub + "\n".join(lines)],
        capture_output=True, text=True, check=True,
    )
    entries = [chunk for chunk in result.stdout.split("\x1e") if chunk]
    return [tuple(chunk.split("\x1f", 1)) for chunk in entries]


class CommitIdentityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.env = os.environ.copy()
        self.env.update(HOME=str(self.repo), GIT_CONFIG_GLOBAL=os.devnull,
                        GIT_CONFIG_NOSYSTEM="1")
        for key in ("GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_NAME",
                    "GIT_COMMITTER_EMAIL", "IDENTITY_BASELINE"):
            self.env.pop(key, None)
        self.git("init", "-q")
        self.git("config", "user.name", "Test User")
        self.git("config", "user.email", GOOD_EMAIL)

    def git(self, *args, env=None):
        return subprocess.run(
            ["git", *args], cwd=self.repo, env=env or self.env,
            capture_output=True, text=True, check=True,
        ).stdout.strip()

    def check_identity(self, *args, env=None):
        return subprocess.run(
            ["bash", str(IDENTITY_SCRIPT), *args], cwd=self.repo,
            env=env or self.env, capture_output=True, text=True,
        )

    def commit(self, email=GOOD_EMAIL):
        self.git("-c", f"user.email={email}", "commit", "-q", "--allow-empty",
                 "-m", "fixture")
        return self.git("rev-parse", "HEAD")

    def commit_as(self, author_email, committer_email):
        env = {**self.env, "GIT_AUTHOR_EMAIL": author_email,
               "GIT_COMMITTER_EMAIL": committer_email}
        self.git("commit", "-q", "--allow-empty", "-m", "fixture", env=env)
        return self.git("rev-parse", "HEAD")

    def test_bad_configured_author(self):
        self.git("config", "user.email", BAD_EMAIL)
        result = self.check_identity()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("AUTHOR", result.stderr)
        self.assertIn("git config user.email <你的邮箱>", result.stderr)

    def test_author_environment_override(self):
        env = {**self.env, "GIT_AUTHOR_EMAIL": BAD_EMAIL}
        result = self.check_identity(env=env)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("AUTHOR", result.stderr)

    def test_bad_committer_with_clean_author(self):
        env = {**self.env, "GIT_COMMITTER_EMAIL": BAD_EMAIL}
        result = self.check_identity(env=env)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("COMMITTER", result.stderr)
        self.assertNotIn("AUTHOR", result.stderr)

    def test_good_configured_address(self):
        result = self.check_identity()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_unrelated_address(self):
        self.git("config", "user.email", OTHER_EMAIL)
        result = self.check_identity()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_similar_address(self):
        self.git("config", "user.email", "after@shipping.example")
        result = self.check_identity()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_range_finds_bad_middle_commit(self):
        first = self.commit()
        self.commit()
        bad = self.commit(BAD_EMAIL)
        self.commit()
        result = self.check_identity("--range", f"{first}..HEAD")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(self.git("rev-parse", "--short", bad), result.stderr)
        self.assertIn(BAD_EMAIL, result.stderr)

    def test_range_all_clean(self):
        first = self.commit()
        self.commit(OTHER_EMAIL)
        result = self.check_identity("--range", f"{first}..HEAD")
        self.assertEqual(result.returncode, 0, result.stderr)
        empty = self.check_identity("--range", "HEAD..HEAD")
        self.assertEqual(empty.returncode, 0, empty.stderr)

    def test_range_bad_author_with_clean_committer(self):
        first = self.commit()
        bad = self.commit_as(BAD_EMAIL, GOOD_EMAIL)
        result = self.check_identity("--range", f"{first}..HEAD")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(self.git("rev-parse", "--short", bad), result.stderr)

    def test_range_bad_committer_with_clean_author(self):
        first = self.commit()
        bad = self.commit_as(GOOD_EMAIL, BAD_EMAIL)
        result = self.check_identity("--range", f"{first}..HEAD")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(self.git("rev-parse", "--short", bad), result.stderr)

    def test_uppercase_address_in_range(self):
        first = self.commit()
        self.commit_as(BAD_EMAIL.upper(), GOOD_EMAIL)
        result = self.check_identity("--range", f"{first}..HEAD")
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_uppercase_author_environment_override(self):
        env = {**self.env, "GIT_AUTHOR_EMAIL": BAD_EMAIL.upper()}
        result = self.check_identity(env=env)
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_invalid_range_fails(self):
        self.commit()
        result = self.check_identity("--range", "nosuch..HEAD")
        self.assertNotEqual(result.returncode, 0, result.stderr)

    def test_missing_residue_pattern_fails_in_both_modes(self):
        scripts = self.repo / "copied-scripts"
        scripts.mkdir()
        identity_copy = scripts / IDENTITY_SCRIPT.name
        residue_copy = scripts / RESIDUE_SCRIPT.name
        shutil.copy2(IDENTITY_SCRIPT, identity_copy)
        residue_copy.write_text("\n".join(
            line for line in RESIDUE_SCRIPT.read_text().splitlines()
            if 'scan "company identity"' not in line
        ) + "\n")
        self.commit()
        for args in ((), ("--range", "HEAD")):
            with self.subTest(args=args):
                result = subprocess.run(
                    ["bash", str(identity_copy), *args], cwd=self.repo,
                    env=self.env, capture_output=True, text=True,
                )
                self.assertEqual(result.returncode, 2, result.stderr)

    def test_mailmap_does_not_hide_bad_commit(self):
        first = self.commit()
        self.commit_as(BAD_EMAIL, GOOD_EMAIL)
        (self.repo / ".mailmap").write_text(
            f"Test User <{GOOD_EMAIL}> <{BAD_EMAIL}>\n"
        )
        result = self.check_identity("--range", f"{first}..HEAD")
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_company_like_name_with_clean_email_is_allowed(self):
        env = {**self.env, "GIT_AUTHOR_NAME": "After" + "ship Fan",
               "GIT_COMMITTER_NAME": "After" + "ship Fan"}
        result = self.check_identity(env=env)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_range_empty_email_commit(self):
        first = self.commit()
        raw = (f"tree {self.git('write-tree')}\nparent {first}\n"
               "author Bad <> 1700000000 +0000\n"
               "committer Test User <" + GOOD_EMAIL + "> 1700000000 +0000\n"
               "\nfixture\n")
        created = subprocess.run(
            ["git", "hash-object", "--literally", "-t", "commit", "-w", "--stdin"],
            cwd=self.repo, env=self.env, input=raw, capture_output=True, text=True,
        )
        if created.returncode != 0:
            result = self.check_identity(env={**self.env, "GIT_AUTHOR_EMAIL": ""})
            self.assertNotEqual(result.returncode, 0, result.stderr)
            return
        updated = subprocess.run(
            ["git", "update-ref", "HEAD", created.stdout.strip()],
            cwd=self.repo, env=self.env, capture_output=True, text=True,
        )
        if updated.returncode != 0:
            result = self.check_identity(env={**self.env, "GIT_AUTHOR_EMAIL": ""})
            self.assertNotEqual(result.returncode, 0, result.stderr)
            return
        result = self.check_identity("--range", f"{first}..HEAD")
        self.assertNotEqual(result.returncode, 0, result.stderr)

    def test_single_revision_checks_bad_middle_commit_after_baseline(self):
        first = self.commit()
        self.commit_as(BAD_EMAIL, GOOD_EMAIL)
        tip = self.commit()
        env = {**self.env, "IDENTITY_BASELINE": first}
        result = self.check_identity("--range", tip, env=env)
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_baseline_exempts_history_but_checks_later_commits(self):
        first = self.commit()
        baseline = self.commit(BAD_EMAIL)
        self.commit()
        env = {**self.env, "IDENTITY_BASELINE": baseline}
        clean = self.check_identity("--range", f"{first}..HEAD", env=env)
        self.assertEqual(clean.returncode, 0, clean.stderr)

        later_bad = self.commit(BAD_EMAIL)
        red = self.check_identity("--range", f"{first}..HEAD", env=env)
        self.assertEqual(red.returncode, 1, red.stderr)
        self.assertIn(self.git("rev-parse", "--short", later_bad), red.stderr)
        self.assertNotIn(self.git("rev-parse", "--short", baseline) + " ", red.stderr)

    def test_missing_baseline_does_not_exempt(self):
        first = self.commit()
        bad = self.commit(BAD_EMAIL)
        env = {**self.env, "IDENTITY_BASELINE": "0" * 40}
        result = self.check_identity("--range", f"{first}..HEAD", env=env)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(self.git("rev-parse", "--short", bad), result.stderr)

    def test_pattern_matches_residue_scan(self):
        expected = dict(extract_scan_patterns(RESIDUE_SCRIPT))["company identity"]
        result = self.check_identity("--print-pattern")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), expected)


if __name__ == "__main__":
    unittest.main()
