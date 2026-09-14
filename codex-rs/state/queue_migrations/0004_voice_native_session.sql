-- Preserve pre-session receipts without inventing a native session identity.
-- These NULL rows cannot be claimed, reconciled, or emitted as typed receipts.
ALTER TABLE voice_admission_receipts
ADD COLUMN native_session_id TEXT CHECK (
    native_session_id IS NULL OR length(trim(native_session_id)) > 0
);

CREATE TRIGGER voice_native_session_required_on_insert
BEFORE INSERT ON voice_admission_receipts
WHEN NEW.native_session_id IS NULL OR length(trim(NEW.native_session_id)) = 0
BEGIN
    SELECT RAISE(ABORT, 'native session id is required');
END;

-- NULL from the old schema also stays immutable: an operator cannot silently
-- upgrade an old receipt into playback eligibility by attaching today's session.
CREATE TRIGGER voice_native_session_immutable
BEFORE UPDATE OF native_session_id ON voice_admission_receipts
WHEN NEW.native_session_id IS NOT OLD.native_session_id
BEGIN
    SELECT RAISE(ABORT, 'native session binding is immutable');
END;
