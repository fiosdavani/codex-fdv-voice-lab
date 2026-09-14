// Reads synthetic eligibility; no subprocess player, network or audio is used.
import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
import { createNativeVoiceBoundary } from './native-boundary/index.mjs';
import { createFakeAdapter } from './native-boundary/fake-adapter.mjs';

const input=JSON.parse(readFileSync(process.argv[2],'utf8'));
assert.equal(input.scope,'E2E_FAKE_ONLY_NO_AUDIO');
assert.equal(input.producer.journal_status_counts.ELIGIBLE_FAKE_ONLY,1);
assert.equal(input.producer.egress_authorized,false);
const r=input.receipt;
const scope={threadId:r.thread_id,nativeSessionId:'FAKE_SESSION',voiceGeneration:r.voice_session_generation,ownerId:'FAKE_OWNER'};
const adapter=createFakeAdapter();
const b=createNativeVoiceBoundary({adapter});
assert.equal((await b.beginSession(scope)).status,'READY_MUTED');
assert.equal((await b.playNativeOutput(scope)).status,'PLAYED_MUTED');
const job={scope,jobGeneration:0};
assert.equal(b.gateVaiJob(job).allowed,true);
const jobId=JSON.stringify([r.thread_id,r.turn_id,input.final_agent_item_id]);
adapter.registerFakePlayer(jobId,scope,0);
const onset={scope,eventId:'FAKE_SPEECH_ONSET',origin:'FAKE_FIXTURE'};
const one=await b.speechOnset(onset);
assert.deepEqual(await b.speechOnset(onset),one);
assert.equal(adapter.playerHandles.get(jobId).stopped,true);
assert.equal(b.gateVaiJob(job).allowed,false); // delayed TTS/master callback
assert.equal((await b.closeVoice(scope)).status,'CLOSED');
const successor={...scope,voiceGeneration:scope.voiceGeneration+1,nativeSessionId:'FAKE_SUCCESSOR'};
assert.equal((await b.beginSession(successor)).status,'READY_MUTED');
const calls=adapter.calls.length;
assert.equal((await b.closeVoice(scope)).status,'IGNORED_STALE_OR_UNKNOWN');
assert.equal(adapter.calls.length,calls);
assert.equal(b.gateVaiJob({scope:successor,jobGeneration:0}).allowed,true);
assert.equal((await b.closeVoice(successor)).status,'CLOSED');
console.log(JSON.stringify({result:'PASS',fake_player_only:true,
 mute_before_play:true,duplicate_onset_deduplicated:true,late_callback_rejected:true,
 old_close_did_not_touch_successor:true,core_subscription_preserved:true,
 events:b.events(),calls:adapter.calls.map(c=>({method:c.method,scope:c.request.scope}))}));
