#!/usr/bin/env python3
"""Exercise the CI baseline shell step with an isolated git command stub."""

import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[3]
WORKFLOW = ROOT / '.github/workflows/ci.yml'
PUBLIC_MAIN = '16db362c93724d352272972c28d1a09dab470813'
ZERO = '0' * 40
BEFORE = 'a' * 40
PR_BASE = 'b' * 40
BRANCH = 'refs/heads/myagenthubs/number1-debt-graph'


def pin_script():
    workflow = WORKFLOW.read_text()
    section = workflow.split('      - name: Fetch and pin file size baseline\n', 1)[1]
    section = section.split('      - name: Check file sizes before candidate execution\n', 1)[0]
    return textwrap.dedent(section.split('        run: |\n', 1)[1])


class BaselineSelectionTest(unittest.TestCase):
    def run_event(self, event, ref, before='', pr_base=''):
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            git = temporary / 'git'
            git.write_text(textwrap.dedent('''\
                #!/usr/bin/env python3
                import os
                from pathlib import Path
                import sys

                args = sys.argv[1:]
                with Path(os.environ['MOCK_GIT_LOG']).open('a') as log:
                    log.write(' '.join(args) + '\\n')
                if args[:2] == ['rev-parse', '--verify']:
                    print(args[2].removesuffix('^{commit}'))
                '''))
            git.chmod(0o755)
            output = temporary / 'output'
            log = temporary / 'git.log'
            environment = dict(os.environ,
                               PATH=f"{temporary}:{os.environ['PATH']}",
                               GITHUB_EVENT_NAME=event, GITHUB_REF=ref,
                               SIZE_GATE_BEFORE=before, SIZE_GATE_PR_BASE=pr_base,
                               GITHUB_OUTPUT=str(output), MOCK_GIT_LOG=str(log))
            result = subprocess.run(['bash', '-c', pin_script()], env=environment,
                                    capture_output=True, text=True)
            return result, output.read_text() if output.exists() else '', \
                log.read_text().splitlines() if log.exists() else []

    def assert_selected(self, event, ref, expected, before='', pr_base=''):
        result, output, calls = self.run_event(event, ref, before, pr_base)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, f'baseline={expected}\n')
        self.assertEqual(calls, [
            f'fetch --no-tags origin {expected}',
            f'rev-parse --verify {expected}^{{commit}}',
            f'update-ref refs/remotes/origin/main {expected}',
        ])

    def assert_rejected(self, event, ref, before='', pr_base=''):
        result, output, calls = self.run_event(event, ref, before, pr_base)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('ERROR:', result.stderr)
        self.assertEqual(output, '')
        self.assertEqual(calls, [])

    def test_normal_push_uses_before_on_main_and_task_branch(self):
        for ref in ('refs/heads/main', BRANCH):
            with self.subTest(ref=ref):
                self.assert_selected('push', ref, BEFORE, before=BEFORE)

    def test_pr_uses_base_sha_even_when_before_is_present(self):
        self.assert_selected('pull_request', BRANCH, PR_BASE,
                             before=BEFORE, pr_base=PR_BASE)

    def test_task_branch_first_push_uses_approved_public_main(self):
        self.assert_selected('push', BRANCH, PUBLIC_MAIN, before=ZERO)

    def test_other_first_push_and_invalid_values_fail_closed(self):
        self.assert_rejected('push', 'refs/heads/main', before=ZERO)
        self.assert_rejected('push', 'refs/heads/other', before=ZERO)
        self.assert_rejected('push', BRANCH, before='')
        self.assert_rejected('push', BRANCH, before='not-a-sha')
        self.assert_rejected('pull_request', BRANCH, pr_base='')
        self.assert_rejected('pull_request', BRANCH, pr_base=ZERO)
        self.assert_rejected('pull_request', BRANCH, pr_base='not-a-sha')
        self.assert_rejected('workflow_dispatch', BRANCH, before=BEFORE)


if __name__ == '__main__':
    unittest.main()
