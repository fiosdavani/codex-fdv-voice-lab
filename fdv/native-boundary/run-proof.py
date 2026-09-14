#!/usr/bin/env python3
"""Run only this package's synthetic Node tests and preserve receipts locally."""
from datetime import datetime, timezone
from pathlib import Path
import hashlib
import json
import os
import re
import shutil
import subprocess
import uuid

ROOT = Path(__file__).resolve().parent
SOURCES = ['index.mjs', 'index.d.ts', 'fake-adapter.mjs', 'test-native-boundary.mjs', 'README.md', 'run-proof.py']


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    node = shutil.which('node')
    if node is None:
        raise SystemExit('NODE_RUNTIME_UNAVAILABLE')
    source_hashes = {name: sha(ROOT / name) for name in SOURCES}
    destination = ROOT / ('proof-' + datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ') + '-' + uuid.uuid4().hex[:8])
    destination.mkdir(exist_ok=False)
    runs = []

    def execute(label, arguments, *, negative=False):
        env = os.environ.copy()
        env.pop('FDV_BOUNDARY_TEST_NEGATIVE', None)
        if negative:
            env['FDV_BOUNDARY_TEST_NEGATIVE'] = '1'
        p = subprocess.run([node, *arguments], cwd=ROOT, env=env, text=True, capture_output=True, timeout=30)
        (destination / (label + '.stdout.txt')).write_text(p.stdout)
        (destination / (label + '.stderr.txt')).write_text(p.stderr)
        entry = {'label': label, 'argv': [node, *arguments], 'negative_control': negative, 'rc': p.returncode}
        runs.append(entry)
        return p

    runtime = execute('runtime', ['--version'])
    checks = [execute('syntax-' + name, ['--check', name]) for name in ['index.mjs', 'fake-adapter.mjs', 'test-native-boundary.mjs']]
    standard = execute('node-test', ['--test', 'test-native-boundary.mjs'])
    detailed = execute('node-test-detailed', ['test-native-boundary.mjs'])
    negative = execute('negative-node-test', ['--test', 'test-native-boundary.mjs'], negative=True)
    negative_detailed = execute('negative-detailed', ['test-native-boundary.mjs'], negative=True)
    tests = re.search(r'^# tests (\d+)$', detailed.stdout, re.M)
    passes = re.search(r'^# pass (\d+)$', detailed.stdout, re.M)
    fails = re.search(r'^# fail (\d+)$', detailed.stdout, re.M)
    executed = int(tests[1]) if tests else 0
    unchanged = source_hashes == {name: sha(ROOT / name) for name in SOURCES}
    success = all(p.returncode == 0 for p in [runtime, *checks, standard, detailed]) and executed == 30 and passes and int(passes[1]) == 30 and fails and int(fails[1]) == 0 and negative.returncode != 0 and negative_detailed.returncode != 0 and 'INTENTIONAL_NEGATIVE_CONTROL' in negative_detailed.stdout and unchanged
    receipt = {
        'schema': 'FDV_NATIVE_BOUNDARY_OFFLINE_PROOF_V1',
        'created_at': datetime.now(timezone.utc).isoformat(),
        'node': runtime.stdout.strip(), 'node_path': node,
        'adapter': 'FAKE', 'source_hashes': source_hashes,
        'source_unchanged': unchanged, 'runs': runs,
        'scenarios_executed': executed,
        'offline_boundary_proof': 'PASS' if success else 'FAIL',
        'negative_control': 'DETECTED' if negative.returncode and negative_detailed.returncode and 'INTENTIONAL_NEGATIVE_CONTROL' in negative_detailed.stdout else 'FAIL',
        'installed_native_hookup': 'NC', 'real_v3_onset': 'NC',
        'windows_audio': 'NOT_EXECUTED', 'voice_calls': 0, 'network_calls': 0,
        'state_persistence': 'MEMORY_ONLY_NOT_PROVED_ACROSS_RESTART',
        'typescript_typecheck': 'NOT_EXECUTED',
    }
    (destination / 'RECEIPT.json').write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + '\n')
    entries = ''.join(f'{sha(p)}  {p.name}\n' for p in sorted(destination.iterdir()) if p.is_file())
    (destination / 'SHA256SUMS').write_text(entries)
    print(json.dumps({'proof_path': str(destination), 'receipt_sha256': sha(destination / 'RECEIPT.json'), 'OFFLINE_BOUNDARY_PROOF': receipt['offline_boundary_proof'], 'SCENARIOS_EXECUTED': executed, 'NEGATIVE_CONTROL': receipt['negative_control'], 'REAL_NATIVE_HOOKUP': 'NC'}, ensure_ascii=False))
    raise SystemExit(0 if success else 1)


if __name__ == '__main__':
    main()
