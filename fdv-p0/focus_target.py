"""Second, bounded static look at already hash-verified target artifact."""
from pathlib import Path
import hashlib,json,re
from analyze_target import scopes,TARGETS,sha

r=Path(__file__).resolve().parent
raws={};indexes={}
for role,(path,digest) in TARGETS.items():
 raw=(r/'target'/path).read_bytes()
 if sha(raw)!=digest:raise RuntimeError('TARGET_HASH_DIVERGENCE_STOP')
 raws[role]=raw;indexes[role]=scopes(raw)
old=json.loads((r/'previous-receipts/TARGET-CLASS-EQUIVALENTS.json').read_text())
out={'scope':'P0_FOCAL_STATIC_REVIEW_ONLY','source_hashes':{k:sha(v) for k,v in raws.items()},'method_contexts':[],'composer_clues':{},'P1':'NO','P2':'NO'}
def take(raw,start,end):
 while start<len(raw) and raw[start]&0xc0==0x80:start+=1
 while end<len(raw) and raw[end]&0xc0==0x80:end-=1
 b=raw[start:end]
 return {'start':start,'end':end,'sha256':sha(b),'text':b.decode()}
wanted={
 'OWNER_CLASS_EQUIVALENT':['start','stop','applyRealtimeMuteState','handleRealtimeClosed','resetRealtimeState','cleanupAttempt','cancelPreparingRuntime','publishRealtimeVoiceHostState'],
 'RUNTIME_CLASS_EQUIVALENT':['setOutputMuted','#v','v','#y','y','#b','#x','#S','dispose'],
 'SINK_CLASS_EQUIVALENT':['start','setOutputAudioMuted','stop'],
 'CLAIM_COORDINATOR_EQUIVALENT':['claim','publish','release','#p','#m','m','#h','#d'],
 'PRESENTATION_COORDINATOR_EQUIVALENT':['registerSurface','#_','getSnapshot']}
for role,names in wanted.items():
 spec=old['SIX_EQUIVALENTS'][role];raw=raws[spec['source']]
 for c in spec['candidates'][:1]:
  if sha(raw[c['scope_byte_start']:c['scope_byte_end_exclusive']])!=c['scope_sha256']:raise RuntimeError('CLASS_SCOPE_DIVERGENCE')
  for m in c['methods']:
   if m['name'] not in names:continue
   start,end=m['start'],m['end'];spans=[]
   # Full methods only when small. Larger functions are deliberate bounded excerpts.
   if end-start<=4200:spans=[take(raw,start,end)]
   else:spans=[take(raw,start,start+1800),take(raw,end-1100,end)]
   out['method_contexts'].append({'role':role,'symbol':c['symbol'],'method':m['name'],'full_method':end-start<=4200,'method_size':end-start,'contexts':spans})
raw=raws['renderer'];funcs=indexes['renderer']['functions']
for term in ('aboveComposerHeaderContent','composerModeAvailability','activeCollaborationMode','surfacePlacement','isVoiceLayoutActive','submitDisabled','inputEnabled'):
 needle=term.encode();positions=[m.start() for m in re.finditer(re.escape(needle),raw)]
 hits=[]
 for pos in positions[:6]:
  enclosing=[f for f in funcs if f['start']<=pos<f['end']]
  enclosing.sort(key=lambda f:f['end']-f['start'])
  fn=enclosing[0] if enclosing else None
  item={'context':take(raw,max(0,pos-200),min(len(raw),pos+850)),'function':fn}
  if fn is not None:item['function_declaration']=take(raw,fn['start'],min(fn['end'],fn['start']+1100))
  hits.append(item)
 out['composer_clues'][term]={'total':len(positions),'hits':hits}
data=(json.dumps(out,ensure_ascii=False,separators=(',',':'))+'\n').encode()
if len(data)>190000:raise RuntimeError('CONTEXT_CAP')
(r/'receipts').mkdir(exist_ok=True)
with (r/'receipts/TARGET-FOCAL-CONTEXTS.json').open('xb') as f:f.write(data)
print('FDV_JSON_BEGIN TARGET-FOCAL-CONTEXTS.json');print(json.dumps(out,ensure_ascii=True,separators=(',',':')));print('FDV_JSON_END TARGET-FOCAL-CONTEXTS.json')
print(json.dumps({'bytes':len(data),'sha256':sha(data)}))
