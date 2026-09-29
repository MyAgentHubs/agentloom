#!/usr/bin/env python3
"""Text-level regression checks for gate workflows in both repositories."""

from pathlib import Path
import re
import unittest


WORKFLOWS = (
    "clippy-gate", "conventions-gate", "doc-orphans-gate", "eslint-gate",
    "file-size-gate", "format-check", "full-tests", "oss-residue-gate",
)
RUST_JOBS = {"engine-clippy", "app-clippy", "engine-tests", "app-rust-tests"}
INTERNAL_REPO = "github.repository == 'MyAgentHubs/agentloom-internal'"
SELF_HOSTED = "fromJSON('[\"self-hosted\",\"macOS\",\"ARM64\"]')"
ROOT = Path(__file__).resolve().parent.parent / ".github/workflows"


def section(source, name):
    match = re.search(rf"(?m)^{re.escape(name)}:\s*\n", source)
    if not match:
        return ""
    end = re.search(r"(?m)^\S", source[match.end():])
    return source[match.end():match.end() + end.start()] if end else source[match.end():]


def event_branches(source, event):
    events = section(source, "on")
    match = re.search(rf"(?m)^  {event}:\s*\n", events)
    if not match:
        return []
    rest = events[match.end():]
    end = re.search(r"(?m)^  \S", rest)
    body = rest[:end.start()] if end else rest
    inline = re.search(r"(?m)^    branches:\s*\[([^\]]*)\]", body)
    if inline:
        return [item.strip().strip("'\"") for item in inline.group(1).split(",")]
    listed = re.search(r"(?m)^    branches:\s*\n((?:^      - .*\n)+)", body)
    return re.findall(r"(?m)^      - ['\"]?([\w/-]+)", listed.group(1)) if listed else []


class DualHomeWorkflowTests(unittest.TestCase):
    def test_gate_triggers_runners_and_policy(self):
        for name in WORKFLOWS:
            with self.subTest(workflow=name):
                source = (ROOT / f"{name}.yml").read_text()
                for event in ("push", "pull_request"):
                    self.assertEqual(set(event_branches(source, event)), {"master", "main"}, event)

                jobs = section(source, "jobs")
                job_matches = list(re.finditer(r"(?m)^  ([\w-]+):\s*$", jobs))
                self.assertTrue(job_matches)
                for index, match in enumerate(job_matches):
                    job = match.group(1)
                    end = job_matches[index + 1].start() if index + 1 < len(job_matches) else len(jobs)
                    runner_lines = re.findall(r"(?m)^    runs-on: (.+)$", jobs[match.end():end])
                    self.assertEqual(len(runner_lines), 1, job)
                    runner = runner_lines[0]
                    label = "macos-latest" if job in RUST_JOBS else "ubuntu-latest"
                    expected = f"${{{{ {INTERNAL_REPO} && {SELF_HOSTED} || '{label}' }}}}"
                    self.assertEqual(runner, expected, job)

                self.assertNotIn("refs/heads/master", source)
                self.assertNotIn("origin/master", source)
                self.assertNotIn("secrets.", source)
                self.assertRegex(source, r"(?m)^permissions:\s*\n  contents: read\s*$")
                self.assertRegex(source, r"(?m)^concurrency:\s*\n  group: .+\n  cancel-in-progress: true$")

    def test_oss_residue_uses_repository_specific_scan(self):
        source = (ROOT / "oss-residue-gate.yml").read_text()
        self.assertIn(
            "run: ${{ " + INTERNAL_REPO
            + " && 'bash scripts/check_oss_residue.sh'"
            + " || 'bash scripts/check_oss_residue.sh --public-tree' }}",
            source,
        )

    def test_baseline_steps_follow_default_branch(self):
        for name in ("conventions-gate", "doc-orphans-gate", "file-size-gate"):
            with self.subTest(workflow=name):
                source = (ROOT / f"{name}.yml").read_text()
                self.assertIn("DEFAULT_BRANCH: ${{ github.event.repository.default_branch }}", source)
                self.assertIn(
                    'git fetch --no-tags origin "+refs/heads/${DEFAULT_BRANCH}:refs/remotes/origin/${DEFAULT_BRANCH}"',
                    source,
                )
                self.assertIn(
                    'for b in master main; do [[ "$b" == "$DEFAULT_BRANCH" ]] || git update-ref -d "refs/remotes/origin/$b"; done',
                    source,
                )
                self.assertIn(
                    'git update-ref "refs/remotes/origin/${DEFAULT_BRANCH}" "$SIZE_GATE_BEFORE"',
                    source,
                )

    def test_all_workflows_avoid_privileged_triggers(self):
        for path in ROOT.iterdir():
            if path.suffix not in (".yml", ".yaml"):
                continue
            with self.subTest(workflow=path.name):
                self.assertNotRegex(path.read_text(), r"(?m)^\s*(?:pull_request_target|workflow_run):")

    def test_all_self_hosted_workflows_are_registered(self):
        registered = {f"{name}.yml" for name in WORKFLOWS}
        for path in ROOT.iterdir():
            if path.suffix not in (".yml", ".yaml"):
                continue
            with self.subTest(workflow=path.name):
                if "self-hosted" in path.read_text():
                    self.assertIn(path.name, registered)


if __name__ == "__main__":
    unittest.main()
