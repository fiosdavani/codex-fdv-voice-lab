#!/usr/bin/env python3
"""Execute boundary and combined-gate suites; never infer coverage from file rc0."""
from datetime import datetime, timezone
from pathlib import Path
import hashlib
import json
import os
import re
import subprocess
import uuid

ROOT = Path(__file__).resolve().parent
SOURCES = ['index.mjs', 'index.d.ts', 'fake-adapter.mjs', 'playback-evidence.mjs',
           'test-native-boundary.mjs', 'test-combined-playback.mjs',
           'fixtures/PLAYBACK-EVIDENCE-FAKE.json', 'README.md', 'run-proof.py']

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def main():
    before = {name: sha(ROOT / name) for name in SOURCES}
    out = ROOT / ('proof-' + datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ') + '-' + uuid.uuid4().hex[:8])
    out.mkdir(exist_ok=False)
    runs = []
    def run(label, args, negative=False):
        env = os.environ.copy()
        for key in ['FDV_BOUNDARY_TEST_NEGATIVE', 'FDV_COMBINED_TEST_NEGATIVE']:
            env.pop(key, None)
            if negative:
                env[key] = '1'
        result = subprocess.run(['node', *args], cwd=ROOT, env=env, capture_output=True, text=True, timeout=30)
        (out / (label + '.stdout.txt')).write_text(result.stdout)
        (out / (label + '.stderr.txt')).write_text(result.stderr)
        runs.append({'argv': ['node', *args], 'label': label, 'negative_control': negative, 'rc': result.returncode})
        return result
    checks = [run('runtime', ['--version'])]
    for name in SOURCES:
        if name.endswith('.mjs'):
            checks.append(run('syntax-' + name, ['--check', name]))
    suites = []
    for name in ['test-native-boundary.mjs', 'test-combined-playback.mjs']:
        standard = run(name + '-runner', ['--test', name])
        detailed = run(name + '-detailed', [name])
        negative = run(name + '-negative', ['--test', name], True)
        negative_detail = run(name + '-negative-detailed', [name], True)
        counts = {k: int(m[1]) if (m := re.search(r'^# ' + k + r' (\d+)$', detailed.stdout, re.M)) else None
                  for k in ['tests', 'pass', 'fail', 'skipped', 'cancelled']}
        good = (standard.returncode == detailed.returncode == 0 and counts == {
            'tests': 30, 'pass': 30, 'fail': 0, 'skipped': 0, 'cancelled': 0}
            and negative.returncode != 0 and negative_detail.returncode != 0
            and 'INTENTIONAL_NEGATIVE_CONTROL' in negative_detail.stdout)
        suites.append({'file': name, 'counts': counts, 'result': 'PASS' if good else 'FAIL',
                       'negative_control_detected': negative.returncode != 0 and negative_detail.returncode != 0})
    unchanged = before == {name: sha(ROOT / name) for name in SOURCES}
    success = unchanged and all(p.returncode == 0 for p in checks) and all(x['result'] == 'PASS' for x in suites)
    receipt = {'schema': 'fdv.native.combined.delta.proof.v1', 'result': 'PASS' if success else 'FAIL',
               'source_hashes': before, 'source_unchanged': unchanged, 'suites': suites, 'runs': runs,
               'scenarios': sum(s['counts']['tests'] or 0 for s in suites),
               'runtime': checks[0].stdout.strip(), 'adapter': 'FAKE',
               'windows': False, 'voice': False, 'elevenlabs': False, 'production': False,
               'typescript_typecheck': 'PENDING_NEW_DELTA_NOT_EXECUTED',
               'native_readback_authentication': 'NC_OFFLINE_FAKE_ONLY'}
    (out / 'RECEIPT.json').write_text(json.dumps(receipt, indent=2) + '\n')
    (out / 'SHA256SUMS').write_text(''.join(f'{sha(f)}  {f.name}\n' for f in sorted(out.iterdir()) if f.is_file()))
    print(json.dumps({'result': receipt['result'], 'scenarios': receipt['scenarios'],
                      'receipt': str(out / 'RECEIPT.json'), 'sha256': sha(out / 'RECEIPT.json')}))
    raise SystemExit(0 if success else 1)

if __name__ == '__main__':
    main()
