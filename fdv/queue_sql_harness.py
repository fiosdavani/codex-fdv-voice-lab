#!/usr/bin/env python3
"""Execute candidate Rust SQL with real SQLite; Core is explicitly FAKE.

No network, canonical DB, Voice or audio. This does NOT execute Rust or prove
Core's runtime. Transactions/bindings reproduce queued_voice.rs and are checked
against literal SQL extracted from that file rather than a second queue schema.
"""
from pathlib import Path
import hashlib
import json
import os
import re
import sqlite3
import subprocess
import sys
import uuid

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent
RUST = REPO / 'codex-rs/state/src/runtime/queued_voice.rs'
SQL = {}
for name, raw, quoted in re.findall(r'const (\w+): &str\s*=\s*(?:r#"(.*?)"#|"(.*?)");', RUST.read_text(), re.S):
    SQL[name] = raw if raw else quoted
THREAD = '00000000-0000-7000-8000-000000000001'

def digest(path): return hashlib.sha256(Path(path).read_bytes()).hexdigest()
def encode(value): return json.dumps(value, sort_keys=True, separators=(',', ':'))
def origin(key='origin-A'):
    return dict(voice_session_generation=1, origin_id=key, handoff_id='handoff-'+key, item_id='item-'+key)
def payload(o, text='same legitimate words'):
    return encode({'UserInput': {'client_id': o['origin_id'], 'content': [{'type':'text','text':text,'text_elements':[]}]}})

class Queue:
    def __init__(self, root):
        self.root = Path(root)
        self.db = self.root / 'queue.sqlite'
        new = not self.db.exists()
        self.c = sqlite3.connect(self.db, isolation_level=None, timeout=10)
        self.c.row_factory = sqlite3.Row
        self.c.execute('PRAGMA synchronous=FULL')
        if new:
            for path in sorted((REPO/'codex-rs/state/queue_migrations').glob('*.sql')):
                self.c.executescript(path.read_text())
    def close(self): self.c.close()
    def get(self, o):
        row=self.c.execute(SQL['GET_VOICE_RECEIPT_SQL'],(THREAD,o['origin_id'])).fetchone()
        return dict(row) if row else None
    def enqueue(self, o, text='same legitimate words', crash=False):
        p=payload(o,text); q=str(uuid.uuid4())
        self.c.execute('BEGIN IMMEDIATE')
        try:
            inserted=self.c.execute(SQL['ENQUEUE_VOICE_RECEIPT_SQL'],(THREAD,o['origin_id'],o['voice_session_generation'],o['handoff_id'],o['item_id'],q,o['origin_id'],p)).rowcount
            row=self.get(o)
            if any(row[k]!=o[k] for k in ('origin_id','voice_session_generation','handoff_id','item_id')) or row['payload_json']!=p:
                raise ValueError('ORIGIN_CONFLICT')
            if inserted:
                n=self.c.execute(SQL['ENQUEUE_VOICE_ITEM_SQL'],(q,THREAD,p,THREAD,1,1,THREAD,100)).rowcount
                if n!=1: raise ValueError('QUEUE_FULL')
            if crash: os._exit(81)
            self.c.commit(); return self.get(o)
        except BaseException:
            self.c.rollback(); raise
    def claim(self, o):
        row=self.get(o)
        result=self.c.execute(SQL['CLAIM_VOICE_SQL'],(str(uuid.uuid4()),THREAD,row['queued_item_id'])).fetchone()
        return dict(result) if result else None
    def finish(self, claim, state, turn=None):
        self.c.execute('BEGIN IMMEDIATE')
        try:
            attempt=None if state=='Queued' else claim['attempt_id']
            row=self.c.execute(SQL['FINISH_VOICE_CLAIM_SQL'],(state,turn,None,attempt,THREAD,claim['queued_item_id'],claim['attempt_id'])).fetchone()
            if row is None: raise ValueError('CLAIM_MISMATCH')
            if state in ('Started','Rejected'):
                self.c.execute(SQL['REMOVE_VOICE_ITEM_SQL'],(THREAD,claim['queued_item_id']))
            self.c.commit()
            return dict(row)
        except BaseException:
            self.c.rollback(); raise
    def reconcile(self, o, core):
        # Positive persisted evidence is mandatory in this harness. Zero or
        # multiple hits cannot turn an ambiguous dispatch into a fresh attempt.
        turns=core.turns_for_client(o['origin_id'])
        if len(turns)!=1: return None
        turn=turns[0]
        self.c.execute('BEGIN IMMEDIATE')
        try:
            row=self.c.execute(SQL['RECONCILE_VOICE_STARTED_SQL'],(turn,THREAD,o['origin_id'],o['origin_id'],turn)).fetchone()
            if row is None: raise ValueError('RECONCILE_REJECTED')
            self.c.execute(SQL['REMOVE_VOICE_ITEM_SQL'],(THREAD,row['queued_item_id']))
            self.c.commit(); return dict(row)
        except BaseException:
            self.c.rollback(); raise
    def dispatch(self, o, core, crash_after_started=False):
        claim=self.claim(o)
        if claim is None: return 'BLOCKED_OR_ALREADY_ADMITTED'
        turn=core.start_if_idle(claim)
        if turn is None:
            self.finish(claim,'Queued'); return 'BUSY_QUEUED'
        if crash_after_started: os._exit(84)
        self.finish(claim,'Started',turn); return turn
    def public_receipt(self,o):
        row=self.get(o)
        return {k:v for k,v in row.items() if k!='payload_json'} | {
            'schema':'fdv.voice.admission.v1','receipt_id':row['queued_item_id']}

class FakeCore:
    """Two real SQLite tables with a fake StartIfIdle producer, not Codex."""
    def __init__(self, root):
        self.path=Path(root)/'FAKE-CORE.sqlite'
        new=not self.path.exists()
        self.c=sqlite3.connect(self.path,isolation_level=None,timeout=10)
        if new:
            schema=json.loads((ROOT/'producer/source-schema.json').read_text())
            for t in schema['tables']:self.c.execute(t['sql'])
    def close(self):self.c.close()
    def start_if_idle(self,claim):
        self.c.execute('BEGIN IMMEDIATE')
        if self.c.execute("SELECT 1 FROM thread_turns WHERE status='inProgress'").fetchone():
            self.c.rollback(); return None
        turn=str(uuid.uuid4()); item=str(uuid.uuid4())
        user={'id':item,'type':'userMessage','clientId':claim['client_id'],'content':[]}
        self.c.execute('INSERT INTO thread_turns(thread_id,turn_id,rollout_ordinal,status,first_user_item_id) VALUES(?,?,1,?,?)',(THREAD,turn,'inProgress',item))
        self.c.execute('INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,created_at_ms,item_json,item_type) VALUES(?,?,?,1,1,?,?)',(THREAD,turn,item,encode(user),'userMessage'))
        self.c.commit();return turn
    def turns_for_client(self,client):
        return [r[0] for r in self.c.execute('SELECT turn_id,item_json FROM thread_items WHERE thread_id=? AND item_type=?',(THREAD,'userMessage')) if json.loads(r[1]).get('clientId')==client]
    def count(self):return self.c.execute('SELECT count(*) FROM thread_turns').fetchone()[0]
    def complete(self,turn,text='FAKE backend final; no Voice or provider involved.'):
        item=str(uuid.uuid4())
        final={'id':item,'type':'agentMessage','text':text,'phase':'final_answer','delivery':None}
        self.c.execute('BEGIN IMMEDIATE')
        self.c.execute('INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,created_at_ms,item_json,item_type) VALUES(?,?,?,2,2,?,?)',(THREAD,turn,item,encode(final),'agentMessage'))
        self.c.execute("UPDATE thread_turns SET status='completed',final_agent_item_id=?,completed_at=2 WHERE thread_id=? AND turn_id=?",(item,THREAD,turn))
        self.c.commit();return item

def child(mode,root):
    q=Queue(root); core=FakeCore(root);o=origin()
    if mode=='before_enqueue_commit':q.enqueue(o,crash=True)
    q.enqueue(o)
    if mode=='after_enqueue':os._exit(82)
    if mode=='after_claim':q.claim(o);os._exit(83)
    if mode=='after_started':q.dispatch(o,core,crash_after_started=True)
    if mode=='concurrent_enqueue':q.close();core.close();return
    raise ValueError(mode)

def run():
    base=ROOT/('queue-tests-'+uuid.uuid4().hex);base.mkdir()
    cases=[];checks=0;processes=0
    def check(condition):
        nonlocal checks
        checks+=1
        if not condition:raise AssertionError('assertion '+str(checks))
    def bench(name):
        d=base/name;d.mkdir();return Queue(d),FakeCore(d)
    def done(name,before):cases.append({'name':name,'assertions':checks-before,'result':'PASS'})
    before=checks;q,c=bench('dedup')
    one=q.enqueue(origin()); two=q.enqueue(origin());check(one==two)
    check(q.c.execute('SELECT count(*) FROM queued_items').fetchone()[0]==1)
    turn=q.dispatch(origin(),c);check(c.count()==1);check(q.get(origin())['turn_id']==turn)
    c.complete(turn);q.enqueue(origin());check(q.dispatch(origin(),c)=='BLOCKED_OR_ALREADY_ADMITTED');check(c.count()==1)
    try:q.enqueue(origin(),'changed input');check(False)
    except ValueError:check(True)
    q.close();c.close();done('duplicate_origin_one_durable_admission_and_one_fake_started',before)

    before=checks;q,c=bench('busy')
    a=origin('A');b=origin('B');q.enqueue(a);q.enqueue(b)
    t1=q.dispatch(a,c);check(q.dispatch(b,c)=='BUSY_QUEUED');check(c.count()==1)
    check(q.get(b)['admission_result']=='Queued');check(q.get(b)['attempt_id'] is None)
    c.complete(t1);t2=q.dispatch(b,c);check(t1!=t2);check(c.count()==2)
    check(c.turns_for_client('A')==[t1]);check(c.turns_for_client('B')==[t2])
    check(q.c.execute('SELECT count(*) FROM queued_items').fetchone()[0]==0)
    q.close();c.close();done('same_text_different_origin_busy_then_separate_turns_never_steer',before)

    for mode,rc in [('before_enqueue_commit',81),('after_enqueue',82),('after_claim',83),('after_started',84)]:
        before=checks;q,c=bench(mode);d=q.root;q.close();c.close()
        result=subprocess.run([sys.executable,'-B',__file__,'--child',mode,str(d)],capture_output=True,text=True);processes+=1
        check(result.returncode==rc)
        q=Queue(d);c=FakeCore(d);o=origin()
        if mode=='before_enqueue_commit':
            check(q.get(o) is None);check(q.c.execute('SELECT count(*) FROM queued_items').fetchone()[0]==0)
            q.enqueue(o);q.dispatch(o,c);check(c.count()==1)
        elif mode=='after_enqueue':
            check(q.get(o)['admission_result']=='Queued');q.enqueue(o);q.dispatch(o,c);check(c.count()==1)
        else:
            check(q.get(o)['admission_result']=='Claimed');check(q.dispatch(o,c)=='BLOCKED_OR_ALREADY_ADMITTED')
            count=c.count();q.enqueue(o);check(c.count()==count)
            evidence=q.reconcile(o,c)
            if mode=='after_claim':
                check(evidence is None);check(count==0);check(q.get(o)['admission_result']=='Claimed')
            else:
                check(evidence['admission_result']=='Started');check(count==1)
                c.complete(evidence['turn_id']);check(q.dispatch(o,c)=='BLOCKED_OR_ALREADY_ADMITTED');check(c.count()==1)
        q.close();c.close();done(mode,before)

    before=checks;q,c=bench('claim_blocks_followers');a=origin('A');b=origin('B');q.enqueue(a);q.enqueue(b)
    claim=q.claim(a);check(claim is not None);check(q.claim(b) is None)
    q.finish(claim,'Ambiguous');check(q.claim(b) is None);check(q.claim(a) is None)
    q.close();c.close();done('ambiguous_claim_blocks_automatic_replay_and_following_voice',before)

    before=checks;q,c=bench('concurrent');d=q.root;q.close();c.close()
    children=[subprocess.Popen([sys.executable,'-B',__file__,'--child','concurrent_enqueue',str(d)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True) for _ in range(2)];processes+=2
    for p in children:
        out,err=p.communicate(timeout=15);check(p.returncode==0)
    q=Queue(d);c=FakeCore(d);check(q.c.execute('SELECT count(*) FROM voice_admission_receipts').fetchone()[0]==1)
    check(q.c.execute('SELECT count(*) FROM queued_items').fetchone()[0]==1)
    q.dispatch(origin(),c);check(c.count()==1);q.close();c.close();done('two_real_processes_same_origin_unique_sql_admission',before)

    # The integrated sample is generated through the same SQL and FAKE Core,
    # then consumed by the evolved real producer. No hand-built Started receipt.
    before=checks;q,c=bench('e2e');o=origin('E2E_ORIGIN');q.enqueue(o);turn=q.dispatch(o,c);final=c.complete(turn)
    receipt=q.public_receipt(o);check(receipt['client_id']==o['origin_id'])
    check(c.turns_for_client(o['origin_id'])==[turn])
    (base/'E2E-ADMISSION.json').write_text(json.dumps({'scope':'FAKE_CORE_REAL_CANDIDATE_SQL','receipt':receipt,'source_path':str(c.path),'final_agent_item_id':final},indent=2)+'\n')
    q.close();c.close();done('client_id_preserved_into_fake_user_item_and_receipt',before)
    result={'schema':'fdv.queue.sql.tests.v1','result':'PASS','assertions':checks,'scenarios':len(cases),'cases':cases,'real_subprocesses':processes,'sqlite_version':sqlite3.sqlite_version,'sql_extracted_from_rust':True,'rust_execution':False,'core':'FAKE_EXPLICIT','voice_windows_network_audio':False,'source_hashes':{'queued_voice.rs':digest(RUST),'harness':digest(__file__)},'e2e_admission_path':str(base/'E2E-ADMISSION.json')}
    (base/'RECEIPT.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({'receipt':str(base/'RECEIPT.json'),**{k:v for k,v in result.items() if k not in ('cases','source_hashes')}},indent=2))

if __name__=='__main__':
    if len(sys.argv)>1 and sys.argv[1]=='--child':child(sys.argv[2],sys.argv[3])
    else:run()
