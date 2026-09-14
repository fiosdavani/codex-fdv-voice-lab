-- Receipts outlive queue removal. Never cascade these rows from queued_items.
CREATE TABLE voice_admission_receipts (
    thread_id TEXT NOT NULL,
    origin_id TEXT NOT NULL CHECK (length(origin_id) > 0),
    voice_session_generation INTEGER NOT NULL CHECK (voice_session_generation >= 1),
    handoff_id TEXT,
    item_id TEXT,
    queued_item_id TEXT NOT NULL UNIQUE,
    client_id TEXT NOT NULL CHECK (client_id = origin_id),
    payload_json TEXT NOT NULL,
    input_digest TEXT,
    admission_result TEXT NOT NULL CHECK (
        admission_result IN ('Queued', 'Claimed', 'Started', 'Ambiguous', 'Rejected', 'Cancelled')
    ),
    attempt_id TEXT,
    turn_id TEXT,
    reason TEXT,
    PRIMARY KEY (thread_id, origin_id),
    CHECK (admission_result != 'Started' OR (turn_id IS NOT NULL AND length(turn_id) > 0)),
    CHECK (admission_result NOT IN ('Claimed', 'Ambiguous') OR attempt_id IS NOT NULL)
);
