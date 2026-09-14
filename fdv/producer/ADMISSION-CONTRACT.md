# Producer admission gate — offline candidate

This directory evolves a COPY of the previously reviewed producer. The original
package and its real 93-job journal are unchanged. This delta does not reopen
either source; the exact historical freeze SHA remains unchanged. `final_producer_base.py` is the
preserved starting source; it is not the active gate implementation.

## Result and scope

- Admission suite: **PASS, 239 assertions / 19 scenarios**.
- Existing SQLite regression: **PASS, 70 assertions / 14 scenarios**, including
  four real benign Linux subprocesses and cache-spill rollback recovery.
- Windows, real Voice, TTS, audio, network and Codex RPC executions: **zero**.
- The receipt and authorization fixtures implement the proposed contract. These
  tests do not prove that an installed runtime emits these receipts.

`ELIGIBLE_FAKE_ONLY` is a re-evaluated job state, not an emitted playback request
or a playback claim. `egress_authorized` is always false. Repeated snapshots
retain one job identity; a downstream consumer must use its own dedup/claim and
must recheck current authorization at the play boundary. This producer does not
make SQLite, a JavaScript consumer and a renderer one atomic transaction.

## Entry points

`admission_gate.evaluate_final_admission(record, receipts, authorization)` returns
`status`, `reason`, `eligible_fake_only`, receipt/authorization hashes and explicit
scope limits. Supply records produced by `final_producer.read_snapshot`; arbitrary
caller-created dictionaries are not independently authenticated SQL evidence.

The integrated SQLite entry point is:

```python
poll_once(source_path, thread_id, journal_path,
          admission_receipts=[receipt], authorization=authorization)
```

CLI inputs `--admission-receipts` and `--authorization` are optional. Without a
matching pair, capture continues in HOLD. They are explicit local contract
snapshots supplied by a trusted caller; merely finding JSON on disk does not
authenticate a real origin, owner or live session.

## Shared receipt contract

```json
{
  "schema": "fdv.voice.admission.v1",
  "receipt_id": "opaque-queue-id",
  "thread_id": "thread-id",
  "native_session_id": "native-session-id",
  "voice_session_generation": 1,
  "origin_id": "opaque-origin-id",
  "handoff_id": null,
  "item_id": null,
  "client_id": "opaque-origin-id",
  "queued_item_id": "opaque-queue-id",
  "turn_id": "admitted-turn-id",
  "admission_result": "Started",
  "attempt_id": null,
  "input_digest": null
}
```

The receipt snapshot contains the latest record for a queue identity, not all
prior state transitions. Identical duplicates are idempotent; conflicting
records for one queue ID HOLD. `receipt_id` must equal `queued_item_id`, and
`client_id` must equal opaque `origin_id`. No generation or origin is extracted
by regular expression from an identifier. The provider `item_id` above is NOT
the backend `final_agent_item_id`; nullable provider identifiers are not used as
substitutes for the backend join.

Only `Started` can satisfy admission. `Queued`, `Claimed`, `Ambiguous`, `Rejected`,
`Cancelled`, unknown results and `Steered` cannot. Absence of a matching first
user item never authorizes a replay or requeue.

## Authorization snapshot

```json
{
  "schema": "fdv.voice.authorization.v1",
  "state": "Active",
  "job_generation": 0,
  "thread_id": "thread-id",
  "native_session_id": "native-session-id",
  "voice_session_generation": 1,
  "origin_id": "opaque-origin-id",
  "client_id": "opaque-origin-id",
  "queued_item_id": "opaque-queue-id",
  "input_digest": null
}
```

This is a per-job snapshot of the caller's CURRENT session authorization.
Voice generation must be a JavaScript-safe integer at least one; job generation
must be a JavaScript-safe integer at least zero. Booleans/stringified numbers
are rejected. Native session ID is required; absence is not a wildcard. Thread,
native session, voice generation, origin, client, queue ID and input digest must
match the receipt exactly. Inactive or absent authorization causes HOLD even
after a previous eligible poll.

`input_digest` is required but nullable. Present null matches only present null;
strings are compared opaquely and exactly. Missing is not null. The producer
does not recompute it from the projected user content, whose serialization has
not been established as the queue's payload encoding.
`source_payload_digest_verified=False` remains explicit even on eligibility.
An explicit null digest does not introduce an additional artificial blocker.

## SQL proof and job generation

The final source still comes from all completed turn pointers, joined by exact
thread/turn/item, without a global cursor or `LIMIT 1`. Final JSON ID, type,
phase, delivery and questions validation remain in place. A receipt cannot
override a frontend item, wrong item ID, missing projection, asynchronous item
or nonfinal phase.

The first user is joined by `thread_turns.first_user_item_id` in the SAME thread
and turn. Its JSON must have `type=userMessage`, matching `id`, and exact camelCase
`clientId`. That value must equal the receipt's `client_id`. Snake-case aliases,
duplicate JSON keys, absent/null/invalid IDs and mismatched pointers HOLD. The
measured real projection uses `clientId`; the observation is documented separately.
The presence of a client ID alone never establishes a Voice origin.

`admission_audit` persists the first positive native session, voice generation,
job generation and receipt hash in the same candidate journal transaction as
the job decision. A later authorization
voice generation cannot rebind that old job. A matching receipt/authorization
rewritten for another native session is also held. Advancing authorization
job_generation after speech onset cannot refresh an old final; a changed committed receipt cannot
silently replace its first proof. Current authorization is checked on every poll.
Revocation concurrent with a later renderer operation still requires the
renderer/consumer's own immediate authorization recheck.

## Combined-gate evidence, never isolated producer authorization

```python
read_playback_evidence(source_path, thread_id, journal_path,
                      (thread_id, turn_id, final_agent_item_id),
                      admission_receipts=[receipt], authorization=authorization)
```

This additional entry point is read-only. It requires an already committed,
owned v3 candidate journal job and its immutable first-generation audit. It
rereads current source rows; source disappearance/change or a persistent journal
HOLD cannot be bypassed by supplying a plausible receipt. It does not create a
job, update eligibility or repair a journal. `build_playback_evidence` is the pure
validator used after that reader enriches a record; arbitrary caller dictionaries
remain trusted-input contracts, not authenticated evidence.

Returned schema `fdv.voice.playback.evidence.v1` contains:

- `identity`: thread/native session/voice generation/origin/client/queue/turn/
  final item/first user item/job generation.
- `source`: final pointer and final item, exact first user evidence, source
  version hash and text hash. The final text remains intact for future TTS.
- `journal`: eligible fake status, original source version and text hash,
  immutable native/voice/job generations and original receipt hash.
- Full matched `admission_receipt` and `authorization` snapshots, their exact
  canonical SHA256s, `provenance_only=true`, `egress_authorized=false`.

Canonical hashing uses UTF-8 `json.dumps(ensure_ascii=False, sort_keys=True,
separators=(',', ':'))`. Packets must remain private when they contain real final
text. The packaged fixture contains exclusively synthetic text. A detached JSON
copy prevents later caller mutation from changing a captured packet; it does
not turn the packet into current authority.

The root-owned `authorizeVaiPlayback` must call a trusted evidence reader at the
play boundary, revalidate all packet identities/hashes, and combine them with
actual current native session, owner, generation and mute fences. The real
authorization producer must retain the job generation assigned to each origin;
it cannot stamp the newest boundary generation onto an old final that arrives
after an onset. Freezing generation at first eligibility alone does not prove
that upstream origin-to-generation assignment. That live assignment is **NC**. Neither a
producer packet nor a boundary-only success authorizes playback. The trusted
live authorization source, native adapter and cross-process transport remain
**NC**, not proved by these offline tests. These APIs do not promise atomicity
across SQLite, JavaScript and a future player. No TTS/audio is invoked here.

## Historical freeze and source changes

`historical_freeze.json` contains only 93 historical thread/turn/item identities
and existing hashes, copied by read-only SQL from the frozen journal. No original
final text is included. Its exact SHA256 is fixed in `admission_gate.py`; a changed
or missing ledger is a STOP, not a fallback to eligibility.

The freeze is by **thread and turn**, so all 93 historical turns remain
`HOLD_HISTORICAL` even if their final pointer changes or a fabricated matching
Started receipt is supplied. A new 94th or later item with no receipt/authorization
also remains HOLD; being absent from the frozen set does not authorize it.

Source conflicts retain the original job text and override receipt eligibility.
Source disappearance removes eligibility from both jobs and the audit record.
The final text, including literal `[COMPLETE]`, is never stripped or normalized.
This is not proof of semantic envelope interpretation.

The delta candidate journal schema is v3. It does not migrate the original
private v1 journal or the previous candidate v2 journal. Old journals fail the
binding check rather than silently gaining playback authorization. The previous journal recovery guards remain candidate-file-only and
cooperative under the same UID; they are not protection against a malicious
same-UID writer, general power-failure recovery or a production authority layer.

## Reproduction

```sh
python3 -B fdv/producer/test_admission.py
python3 -B fdv/producer/test_producer.py
```

Run from the candidate repository root. Each creates a fresh directory only
under `fdv/producer`, with real SQLite files containing synthetic bodies.
Tests do not modify the frozen real journal or the approved package.

The fake end-to-end queue/renderer test is owned by the root integration task;
these producer receipts do not claim that integration has already passed.
