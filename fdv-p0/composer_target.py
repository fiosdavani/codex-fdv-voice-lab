"""P0 only: locate composer anchor in the two already verified static bundles."""
from pathlib import Path
import json,re
from analyze_target import scopes,TARGETS,sha
r=Path(__file__).resolve().parent
path,digest=TARGETS['renderer'];raw=(r/'target'/path).read_bytes()
if sha(raw)!=digest:raise RuntimeError('TARGET_HASH_DIVERGENCE_STOP')
idx=scopes(raw);funcs=idx['functions']
def take(a,b):
 while a<len(raw) and raw[a]&0xc0==0x80:a+=1
 while b<len(raw) and raw[b]&0xc0==0x80:b-=1
 s=raw[a:b];return {'start':a,'end':b,'sha256':sha(s),'text':s.decode()}
out={'scope':'P0_COMPOSER_ANCHOR_ONLY','renderer_sha256':sha(raw),'signals':{},'asset_references':[],'P1':'NO','P2':'NO'}
owner=raw[8739907:8751215]
out['owner_claim_preparation']=[]
for m in re.finditer(rb'F\.status',owner):
 p=8739907+m.start();out['owner_claim_preparation'].append(take(max(8739907,p-500),min(8751215,p+2800)))
terms=('isComposerInputVisible','getDictationSurroundingText','shouldHandleDictation','hasFocusedComposer','hasActiveApprovalSurface','stop-realtime-session','data-codex-composer','composerController','dictation send failed','isComposerInputEnabled','composerDisabled','composerReadiness')
for term in terms:
 hits=list(re.finditer(re.escape(term.encode()),raw));contexts=[]
 for hit in hits[:4]:
  p=hit.start();item={'context':take(max(0,p-500),min(len(raw),p+1100))}
  fs=sorted((f for f in funcs if f['start']<=p<f['end']),key=lambda f:f['end']-f['start'])
  if fs:
   f=fs[0];item['function']=f;item['declaration']=take(f['start'],min(f['end'],f['start']+1600))
  contexts.append(item)
 out['signals'][term]={'total':len(hits),'hits':contexts}
for m in re.finditer(rb'[A-Za-z0-9_./-]*(?:composer|local-conversation-thread|src-)[A-Za-z0-9_./-]*\.js',raw):
 v=m.group().decode()
 if v not in out['asset_references']:out['asset_references'].append(v)
b=(json.dumps(out,ensure_ascii=False,separators=(',',':'))+'\n').encode()
if len(b)>120000:raise RuntimeError('CONTEXT_CAP')
with (r/'receipts/TARGET-COMPOSER-CONTEXTS.json').open('xb') as f:f.write(b)
print('FDV_JSON_BEGIN TARGET-COMPOSER-CONTEXTS.json');print(json.dumps(out,ensure_ascii=True,separators=(',',':')));print('FDV_JSON_END TARGET-COMPOSER-CONTEXTS.json');print(json.dumps({'bytes':len(b),'sha256':sha(b)}))
