#!/usr/bin/env python3
"""Count actual named Node tests; never count an rc=0 per-file wrapper as coverage."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

repo, out = Path(sys.argv[1]), Path(sys.argv[2])
out.mkdir(parents=True, exist_ok=True)
base = repo / 'fdv/native-boundary'
runs = []
for name, expected in [('test-native-boundary.mjs', 30), ('test-combined-playback.mjs', 30),
                       ('test-lifecycle-wire.mjs', 15)]:
    result = subprocess.run(['node', str(base / name)], capture_output=True, text=True, timeout=90)
    (out / (name + '.tap')).write_text(result.stdout)
    (out / (name + '.stderr')).write_text(result.stderr)
    counts = {k: int(m[1]) if (m := re.search(r'^# ' + k + r' (\d+)$', result.stdout, re.M)) else None
              for k in ['tests', 'pass', 'fail', 'skipped', 'cancelled']}
    passed = result.returncode == 0 and counts == dict(tests=expected, **{'pass': expected}, fail=0, skipped=0, cancelled=0)
    runs.append(dict(file=name, rc=result.returncode, counts=counts, passed=passed))
env = dict(os.environ, FDV_LIFECYCLE_NEGATIVE='1')
negative = subprocess.run(['node', str(base / 'test-lifecycle-negative.mjs')], env=env,
                          capture_output=True, text=True, timeout=30)
(out / 'negative.tap').write_text(negative.stdout)
negative_ok = negative.returncode != 0 and 'INTENTIONAL_LIFECYCLE_NEGATIVE_CONTROL' in negative.stdout
sources = {str(p.relative_to(repo)): hashlib.sha256(p.read_bytes()).hexdigest()
           for p in sorted(base.glob('*.mjs'))}
receipt = dict(suites=runs, negative_control_detected=negative_ok,
               tests_executed=sum(r['counts']['tests'] or 0 for r in runs),
               tests_passed=sum(r['counts']['pass'] or 0 for r in runs), source_sha256=sources,
               scope='OFFLINE_FAKE_TTS_PLAYER_AND_NATIVE_PORTS', runtime_transport_installed=False,
               egressAuthorized=False)
receipt['PASS'] = negative_ok and all(r['passed'] for r in runs)
(out / 'NODE-RECEIPT.json').write_text(json.dumps(receipt, indent=2) + '\n')
print(json.dumps(receipt))
raise SystemExit(0 if receipt['PASS'] else 1)
