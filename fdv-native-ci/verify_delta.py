#!/usr/bin/env python3
import hashlib
import json
from pathlib import Path
import subprocess

repo = Path(__file__).resolve().parents[1]
manifest = json.loads((repo / 'fdv-native-ci/DELTA-SHA256.json').read_text())
for name, expected in manifest.items():
    path = repo / name
    assert path.is_file() and not path.is_symlink(), name
    assert hashlib.sha256(path.read_bytes()).hexdigest() == expected, name
base = '04e7eca4009915f96953b328bafb3969517414de'
changed = set(subprocess.check_output(['git', 'diff', '--name-only', base, 'HEAD'], cwd=repo, text=True).splitlines())
assert changed <= set(manifest) | {'fdv-native-ci/DELTA-SHA256.json'}, sorted(changed - set(manifest))
assert changed, 'no delta'
print(json.dumps({'BASE_APPROVED': base, 'HASHES_PASS': len(manifest), 'CHANGED_FILES': sorted(changed)}))
