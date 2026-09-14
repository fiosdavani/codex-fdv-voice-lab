#!/usr/bin/env python3
"""Join real candidate SQL receipts, producer and Node boundary; no real Core/audio."""
from pathlib import Path
import hashlib
import json
import subprocess
import sys
import uuid

ROOT=Path(__file__).resolve().parent
sys.path.insert(0,str(ROOT/'producer'))
from final_producer import poll_once

admission_path=Path(sys.argv[1]).resolve(strict=True)
if not admission_path.is_relative_to(ROOT) or admission_path.name!='E2E-ADMISSION.json':
    raise SystemExit('LOCAL_SYNTHETIC_RECEIPT_REQUIRED')
admission=json.loads(admission_path.read_text())
if admission['scope']!='FAKE_CORE_REAL_CANDIDATE_SQL':raise SystemExit('FAKE_SCOPE_REQUIRED')
r=admission['receipt']
authorization={k:r[k] for k in ('thread_id','voice_session_generation','origin_id','client_id','queued_item_id','input_digest')}
authorization.update(schema='fdv.voice.authorization.v1',state='Active')
out=ROOT/'producer'/('e2e-'+uuid.uuid4().hex);out.mkdir()
capture=poll_once(Path(admission['source_path']),r['thread_id'],out/'journal.sqlite',admission_receipts=[r],authorization=authorization)
if capture['journal_status_counts'].get('ELIGIBLE_FAKE_ONLY')!=1 or capture['egress_authorized']:
    raise SystemExit('PRODUCER_FAKE_ELIGIBILITY_FAILED')
bridge={'scope':'E2E_FAKE_ONLY_NO_AUDIO','receipt':r,'authorization':authorization,
        'final_agent_item_id':admission['final_agent_item_id'],'producer':capture}
bridge_path=out/'BRIDGE.json';bridge_path.write_text(json.dumps(bridge,indent=2)+'\n')
result=subprocess.run(['node',str(ROOT/'e2e_playback.mjs'),str(bridge_path)],capture_output=True,text=True,timeout=10)
if result.returncode:raise SystemExit(result.stderr or result.stdout)
boundary=json.loads(result.stdout)
assert boundary['result']=='PASS' and boundary['fake_player_only']
revoked={**authorization,'state':'Closed'}
closed=poll_once(Path(admission['source_path']),r['thread_id'],out/'journal.sqlite',admission_receipts=[r],authorization=revoked)
assert closed['journal_status_counts'].get('ELIGIBLE_FAKE_ONLY',0)==0
receipt={'schema':'fdv.voice.e2e.fake.v1','result':'PASS','real_core':False,
 'rust_executed':False,'voice_windows_elevenlabs_network':False,
 'admission_source_sha256':hashlib.sha256(admission_path.read_bytes()).hexdigest(),
 'thread_id':r['thread_id'],'voice_session_generation':r['voice_session_generation'],
 'origin_id':r['origin_id'],'client_id':r['client_id'],'queued_item_id':r['queued_item_id'],
 'turn_id':r['turn_id'],'final_agent_item_id':admission['final_agent_item_id'],
 'producer':capture,'boundary':boundary,'producer_after_revocation':closed,
 'source_hashes':{str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest()
   for p in [Path(__file__),ROOT/'e2e_playback.mjs',ROOT/'producer/final_producer.py',
             ROOT/'producer/admission_gate.py',ROOT/'native-boundary/index.mjs']}}
path=out/'END_TO_END_FAKE_RECEIPT.json';path.write_text(json.dumps(receipt,indent=2)+'\n')
print(json.dumps({'result':'PASS','receipt':str(path),'sha256':hashlib.sha256(path.read_bytes()).hexdigest()},indent=2))
