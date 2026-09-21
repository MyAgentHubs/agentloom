#!/usr/bin/env python3
"""Subprocess regressions for graph and target validation with asserts disabled."""

import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]
TARGET = 'app/src/App.tsx'


class GraphValidationTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.doc = self.root / 'docs/number1'
        tools = self.doc / 'tools'
        tools.mkdir(parents=True)
        shutil.copyfile(ROOT / 'docs/number1/tools/check_graph.py', tools / 'check_graph.py')
        scripts = self.root / 'scripts'
        scripts.mkdir()
        size_script = scripts / 'check_file_size.py'
        shutil.copyfile(ROOT / 'scripts/check_file_size.py', size_script)

        spec = importlib.util.spec_from_file_location('size_gate_fixture', size_script)
        size_gate = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(size_gate)
        cap = size_gate.category(PurePosixPath(TARGET), b'')[1]
        target = self.root / TARGET
        target.parent.mkdir(parents=True)
        target.write_bytes(b'// synthetic line\n' * (cap + 1))

        self.node = {
            'id': 'fixture', 'kind': 'file_epic', 'target': TARGET,
            'goal': 'fixture', 'scope': 'fixture', 'state': 'pending',
            'worker': 'fixture', 'verify': 'fixture',
            'budget': {'max_rounds': 1, 'max_tokens': 1},
            'stop': 'fixture', 'escalate': 'fixture', 'depends_on': [],
            'expand_before_execute': True, 'steps': ['fixture'],
        }
        self.write_json('graph.json', {'nodes': [self.node]})
        self.write_json('inventory.json', {'files': [{'path': TARGET}], 'counts': {'all': 1}})
        self.write_json('long-functions.json', {
            'files': [], 'counts': {'app_functions': 0, 'remote_web_functions': 0},
        })
        self.write_json('state/index.json', {'nodes': ['fixture']})
        self.write_json('patches/manifest.json', [])

    def write_json(self, relative, value):
        path = self.doc / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value))

    def run_checker(self, optimized, *args):
        environment = dict(os.environ, PYTHONDONTWRITEBYTECODE='1')
        environment.pop('PYTHONPATH', None)
        if optimized:
            environment['PYTHONOPTIMIZE'] = '1'
        else:
            environment.pop('PYTHONOPTIMIZE', None)
        active = subprocess.check_output(
            [sys.executable, '-c', 'import sys; print(sys.flags.optimize)'],
            env=environment, text=True,
        ).strip()
        self.assertEqual(active, '1' if optimized else '0')
        return subprocess.run(
            [sys.executable, str(self.doc / 'tools/check_graph.py'), *args],
            env=environment, capture_output=True, text=True, check=False,
        )

    def test_valid_graph_and_oversized_target_in_both_modes(self):
        for optimized in (False, True):
            with self.subTest(optimized=optimized):
                graph = self.run_checker(optimized)
                self.assertEqual(graph.returncode, 0, graph.stderr)
                self.assertIn('PASS: 1 DAG nodes; 1 targets', graph.stdout)

                oversized = self.run_checker(optimized, '--target', TARGET)
                self.assertNotEqual(oversized.returncode, 0)
                self.assertIn('debt remains', oversized.stderr)
                self.assertNotIn(f'PASS: {TARGET}', oversized.stdout)

    def test_invalid_graph_rejected_in_both_modes(self):
        self.write_json('graph.json', {'nodes': [self.node, self.node]})
        for optimized in (False, True):
            with self.subTest(optimized=optimized):
                result = self.run_checker(optimized)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('duplicate node ID', result.stderr)
                self.assertNotIn('PASS:', result.stdout)


if __name__ == '__main__':
    unittest.main()
