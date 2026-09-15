import hashlib,json,os,sys
from pathlib import Path

r=Path(__file__).resolve().parent
def load(name):
    p=r/'receipts'/name
    return json.loads(p.read_text()) if p.exists() else {}
msix=load('MSIX-DIGEST-RECEIPT.json'); ex=load('target-extraction.json')
anchor=load('TARGET-ANCHOR-CONTEXTS.json'); classes=load('TARGET-CLASS-EQUIVALENTS.json')
bundles=ex.get('bundles',{})
ok=msix.get('MSIX_DIGEST_GATE')=='PASS' and ex.get('TARGET_BUILD_BYTES')=='PASS'
rec={
 'ACTION_RUN_URL':'https://github.com/'+os.environ['GITHUB_REPOSITORY']+'/actions/runs/'+os.environ['GITHUB_RUN_ID'],
 'ACTION_RUN_ATTEMPT':os.environ['GITHUB_RUN_ATTEMPT'], 'COMMIT':os.environ['GITHUB_SHA'],
 'MSIX_BYTES':msix.get('MSIX_BYTES'), 'MSIX_SHA1':msix.get('MSIX_SHA1'), 'MSIX_SHA256':msix.get('MSIX_SHA256'),
 'MSIX_DIGEST_GATE':msix.get('MSIX_DIGEST_GATE','NOT_EXECUTED'),
 'PACKAGE_IDENTITY':ex.get('identity'),
 'RENDERER_SHA256':bundles.get('webview/assets/app-initial-d9bed9d614d8.js',{}).get('sha256'),
 'MAIN_SHA256':bundles.get('.vite/build/main-D8abTQQE.js',{}).get('sha256'),
 'ASAR_SHA256':ex.get('asar',{}).get('sha256'),
 'TARGET_BUILD_BYTES':'PASS' if ok else 'NC',
 'EXACT_BUILD_MATCH':'PENDING_STRUCTURAL_REVIEW' if ok else 'NC',
 'ANCHOR_ANALYSIS_PRESENT':bool(anchor), 'CLASS_ANALYSIS_PRESENT':bool(classes),
 'PATCH_ANCHORS_MATCHED':anchor.get('PATCH_ANCHORS_MATCHED','NC/22'),
 'PATCH_ANCHORS_MATCHED_BASIS':anchor.get('PATCH_ANCHORS_MATCHED_BASIS'),
 'STRUCTURAL_ANCHORS_CONFIRMED':anchor.get('STRUCTURAL_ANCHORS_CONFIRMED','NC/22'),
 'SIX_EQUIVALENTS':{k:{'value':v.get('value'),'status':v.get('status'),'candidate_count':v.get('candidate_count')} for k,v in classes.get('SIX_EQUIVALENTS',{}).items()},
 'OWNER_PATCH':'FROZEN_SOL_PASS', 'P1':'NO', 'P2':'NO', 'WINDOWS_CANARY':'NO','VOICE':'NO','ELEVENLABS':'NO','PRODUCTION':'NO','egressAuthorized':False,
 'EXTRACTION_REASON':ex.get('reason'), 'REMAINING_NC':['independent structural review of22anchors/sixequivalents','publisher signature not validated','P1/P2 not executed'],
 'TOOLS_SHA256':{p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(r.glob('*.py'))},
 'ANALYSIS_JSON_SHA256':{p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted((r/'receipts').glob('TARGET-*.json'))}
}
with (r/'receipts/TARGET-P0-RECEIPT.json').open('x') as f:
 json.dump(rec,f,indent=2);f.write('\n')
for name in ('TARGET-P0-RECEIPT.json','TARGET-ANCHOR-CONTEXTS.json','TARGET-CLASS-EQUIVALENTS.json'):
 p=r/'receipts'/name
 if p.exists():
  obj=json.loads(p.read_text());s=json.dumps(obj,separators=(',',':'),ensure_ascii=True)
  if len(s)>220000:raise RuntimeError('OUTPUT_CAP')
  print('FDV_JSON_BEGIN '+name); print(s); print('FDV_JSON_END '+name)
sys.exit(0 if ok and anchor and classes else 1)
