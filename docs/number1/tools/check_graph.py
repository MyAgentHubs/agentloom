#!/usr/bin/env python3
"""Read-only handoff contract validation; no model, network, or third-party package."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
DOC = ROOT / 'docs/number1'

def require(condition, message):
    if not condition:
        raise ValueError(message)

def validate():
    graph = json.loads((DOC / 'graph.json').read_text())
    nodes = graph['nodes']
    lookup = {n['id']: n for n in nodes}
    require(len(lookup) == len(nodes), 'duplicate node ID')
    required = {'goal', 'scope', 'state', 'worker', 'verify', 'budget', 'stop', 'escalate'}
    for n in nodes:
        require(required <= n.keys(), n['id'])
        require(bool(n['goal'] and n['scope'] and n['verify']), n['id'])
        require(all(d in lookup for d in n['depends_on']), n['id'])
        require(n['budget']['max_rounds'] > 0 and n['budget']['max_tokens'] > 0, n['id'])
        if n['kind'] in {'file_epic', 'function_epic'}:
            require(bool(n['expand_before_execute'] and n['steps']), n['id'])
    visited, active = set(), set()
    def visit(key):
        require(key not in active, f'cycle at {key}')
        if key in visited:
            return
        active.add(key)
        for dep in lookup[key]['depends_on']:
            visit(dep)
        active.remove(key)
        visited.add(key)
    for key in lookup:
        visit(key)
    inv = json.loads((DOC / 'inventory.json').read_text())
    require(len(inv['files']) == inv['counts']['all'], 'inventory count mismatch')
    require(len({f['path'] for f in inv['files']}) == len(inv['files']), 'duplicate inventory path')
    require({n['target'] for n in nodes if n['kind'] == 'file_epic'} == {f['path'] for f in inv['files']}, 'file epic targets mismatch')
    long = json.loads((DOC / 'long-functions.json').read_text())
    require({n['target'] for n in nodes if n['kind'] == 'function_epic'} == {f['path'] for f in long['files']}, 'function epic targets mismatch')
    require(sum(f['functions'] for f in long['files']) == long['counts']['app_functions'] + long['counts']['remote_web_functions'], 'long function count mismatch')
    state = json.loads((DOC / 'state/index.json').read_text())
    require(set(state['nodes']) == set(lookup), 'state node IDs mismatch')
    for p in json.loads((DOC / 'patches/manifest.json').read_text()):
        require(hashlib.sha256((DOC / p['file']).read_bytes()).hexdigest() == p['sha256'], p['id'])
    print(f'PASS: {len(nodes)} DAG nodes; {len(inv["files"])} targets; state IDs and patch digests')

def target(path):
    p = Path(path)
    require(not p.is_absolute() and '..' not in p.parts, 'repository-relative path required')
    full = ROOT / p
    require(full.is_file() and not full.is_symlink(), path)
    spec = importlib.util.spec_from_file_location('number1_size', ROOT / 'scripts/check_file_size.py')
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    data = full.read_bytes()
    count = mod.count_lines(data, path)
    label, cap = mod.category(p, data)
    require(count <= cap, f'{path}: {count} > {cap} ({label}); debt remains')
    print(f'PASS: {path}: {count} <= {cap}; behavior verification still required')

if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--target')
    parser.add_argument('--node', help='Print only one node contract to keep LLM context bounded')
    args = parser.parse_args()
    validate()
    if args.target:
        target(args.target)
    if args.node:
        nodes = json.loads((DOC / 'graph.json').read_text())['nodes']
        selected = next((n for n in nodes if n['id'] == args.node), None)
        require(selected is not None, 'unknown node ID')
        print(json.dumps(selected, ensure_ascii=False, indent=2))
