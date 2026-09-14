#!/usr/bin/env python3
"""New native lifecycle delta only, using the already validated nextest/JUnit parser."""
import base64
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys

repo, out = Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve()
out.mkdir(parents=True, exist_ok=True)
sys.path.insert(0, str(repo / 'fdv-ci'))
from run_gate import Gate, EvidenceError, parse_discovery, parse_junit, summarize, verify_expected

BASE = '04e7eca4009915f96953b328bafb3969517414de'
gate = Gate(repo, out)
gate.receipt.update(schema_version=2, BASE_APPROVED_RUST_PASS=BASE,
                    PRIOR_RUST_GATE='ACCEPTED_NO_REOPEN', NATIVE_DELTA_GATE='FAIL')
gate.receipt['planned_test_selections'] = ['core realtime/voice and directly affected turn_input']
gate.receipt.pop('ABORT_SIX', None)
gate.receipt.pop('EXPECTED_58', None)
try:
    required = json.loads((repo / 'fdv-native-ci/EXPECTED-RUST-TESTS.json').read_text())
    if not required or len({x['test_name'] for x in required}) != len(required):
        raise EvidenceError('native expected test inventory empty/duplicated')
    for label, command, cwd in [
        ('base-ancestor', ['git', 'merge-base', '--is-ancestor', BASE, 'HEAD'], repo),
        ('initial-clean', ['git', 'diff', '--exit-code'], repo),
        ('delta-hashes', ['python3', '-B', 'fdv-native-ci/verify_delta.py'], repo),
    ]:
        if gate.command(label, command, cwd)[0]: raise EvidenceError(label)
    for label, command in [('RUSTC_VERSION', 'rustc'), ('CARGO_VERSION', 'cargo')]:
        rc, log = gate.command(label.lower(), [command, '--version'])
        gate.receipt[label] = log.read_text().strip()
        if rc or not gate.receipt[label].startswith(command + ' 1.95.0 '): raise EvidenceError(label)
    fmt = ['cargo', 'fmt', '--all', '--', '--config', 'imports_granularity=Item']
    rc, _ = gate.command('fmt-initial', fmt + ['--check'])
    gate.receipt['FMT_INITIAL'] = 'PASS' if rc == 0 else 'FAIL'
    if rc:
        if gate.command('fmt-mechanical-correction', fmt)[0]: raise EvidenceError('fmt failed')
        _, patch = gate.command('fmt-only-diff', ['git', 'diff', '--binary'], repo)
        gate.receipt['FMT_CORRECTION_SHA256'] = hashlib.sha256(patch.read_bytes()).hexdigest()
        rc, _ = gate.command('fmt-recheck', fmt + ['--check'])
    gate.receipt['FMT'] = 'PASS' if rc == 0 else 'FAIL'
    # Export exact rustfmt bytes for application in the offline workspace. Full
    # blobs, not a truncated console diff, bind the source to the compiled proof.
    changed = subprocess.check_output(['git', 'diff', '--name-only'], cwd=repo, text=True).splitlines()
    gate.receipt['FORMAT_EXPORT'] = []
    for name in changed:
        if not name.startswith('codex-rs/') or not name.endswith('.rs'):
            raise EvidenceError('formatter changed an unexpected path: ' + name)
        data = (repo / name).read_bytes()
        sha = hashlib.sha256(data).hexdigest()
        encoded = base64.b64encode(data).decode()
        chunks = [encoded[i:i+8000] for i in range(0, len(encoded), 8000)]
        gate.receipt['FORMAT_EXPORT'].append({'path': name, 'sha256': sha, 'chunks': len(chunks)})
        for i, chunk in enumerate(chunks):
            print('FORMATTED_SOURCE_CHUNK=' + json.dumps({'path': name, 'sha256': sha,
                  'chunk_index': i, 'chunk_count': len(chunks), 'base64': chunk}), flush=True)
    for label, args in [
        ('CARGO_CHECK', ['cargo', 'check', '--locked', '--tests']),
        ('CLIPPY', ['cargo', 'clippy', '--locked', '--tests']),
    ]:
        args += ['-p', 'codex-core', '-p', 'codex-extension-api', '-p', 'codex-app-server']
        if label == 'CLIPPY': args += ['--', '-D', 'warnings']
        rc, _ = gate.command(label.lower(), args)
        gate.receipt[label] = 'PASS' if rc == 0 else 'FAIL'
        if rc and label == 'CARGO_CHECK': raise EvidenceError('check failed; no test discovery claimed')
    arguments = ['-p', 'codex-core', '--lib', '-E',
                 'test(/^realtime_voice_/) | test(/^realtime_conversation::/) | test(/^session::turn_input::tests::/)']
    rc, listing = gate.command('native-list', ['cargo', 'nextest', 'list', '--locked', *arguments, '--message-format', 'json'])
    if rc: raise EvidenceError('compiled discovery failed')
    discovered = parse_discovery(json.loads(listing.read_text()))
    verify_expected(discovered, required)
    gate.discovered.update(discovered); gate.save()
    junit = repo / 'codex-rs/target/nextest/default/junit.xml'
    if junit.exists(): junit.unlink()
    rc, _ = gate.command('native-run', ['just', 'test', '--locked', *arguments,
                        '--profile', 'default', '--retries', '0', '--test-threads', '2'])
    if not junit.is_file(): raise EvidenceError('fresh JUnit absent')
    shutil.copyfile(junit, out / 'native.junit.xml')
    gate.results.update(parse_junit(junit.read_bytes(), discovered))
    stats = summarize(discovered, gate.results)
    gate.receipt['NATIVE_EXPECTED_TESTS'] = required
    gate.receipt['NATIVE_EXPECTED_RESULT'] = gate.expected_outcomes(required)
    gate.receipt['compiled_source_sha256'] = {str(p.relative_to(repo)): hashlib.sha256(p.read_bytes()).hexdigest()
        for p in sorted((repo / 'codex-rs/core/src').glob('realtime*.rs'))}
    success = rc == 0 and stats['PASS'] and all(gate.receipt[x] == 'PASS' for x in ['FMT', 'CARGO_CHECK', 'CLIPPY', 'NATIVE_EXPECTED_RESULT'])
    gate.receipt['NATIVE_DELTA_GATE'] = 'PASS' if success else 'FAIL'
except Exception as exc:
    gate.receipt['errors'].append(f'{type(exc).__name__}: {exc}')
    success = False
finally:
    # Legacy keys are not reasserted by this gate. Only NATIVE_DELTA_GATE is verdict.
    gate.receipt.pop('RUST_GATE', None)
    gate.save()
    print('NATIVE_RUST_RECEIPT=' + json.dumps(gate.receipt, separators=(',', ':')))
raise SystemExit(0 if success else 1)
