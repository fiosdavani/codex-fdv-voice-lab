import hashlib, json, sys
from pathlib import Path

root = Path(__file__).resolve().parent
path = root/'incoming/OpenAI.Codex_26.908.4834.0_x64__2p2nqsd0c76g0.msix'
expected = (774918112, '5678e4c10eefd675055cb5178990beafbcafcff8', 'ae01e44b60f3c42fada0cf7deb220104bb8e9111ee79cda16bd57ce7e59c72b0')
sha1, sha256 = hashlib.sha1(), hashlib.sha256()
with path.open('rb') as f:
    for chunk in iter(lambda: f.read(1024*1024), b''):
        sha1.update(chunk); sha256.update(chunk)
actual = (path.stat().st_size, sha1.hexdigest(), sha256.hexdigest())
receipt = dict(zip(('MSIX_BYTES','MSIX_SHA1','MSIX_SHA256'), actual))
receipt.update({'EXPECTED_BYTES':expected[0], 'EXPECTED_SHA1':expected[1], 'EXPECTED_SHA256':expected[2], 'MSIX_DIGEST_GATE':'PASS' if actual==expected else 'FAIL_STOP'})
with (root/'receipts/MSIX-DIGEST-RECEIPT.json').open('x') as f:
    json.dump(receipt,f,indent=2); f.write('\n')
print(json.dumps(receipt))
sys.exit(0 if actual==expected else 70)
