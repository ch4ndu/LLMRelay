-- body stays the original guidance. Its one delivery reservation records the exact
-- text the provider is asked to submit, which native submit matching then requires.
-- Rows written before this schema were pasted as their body and keep NULL here.
ALTER TABLE guidance_messages ADD COLUMN submitted_text TEXT;
ALTER TABLE guidance_messages ADD COLUMN submitted_digest TEXT;

CREATE TRIGGER guidance_submitted_form_paired BEFORE INSERT ON guidance_messages
WHEN (NEW.submitted_text IS NULL) != (NEW.submitted_digest IS NULL)
BEGIN
    SELECT RAISE(ABORT, 'guidance submitted text and digest are recorded together');
END;

CREATE TRIGGER guidance_submitted_form_once
BEFORE UPDATE OF submitted_text, submitted_digest ON guidance_messages
WHEN (NEW.submitted_text IS NULL) != (NEW.submitted_digest IS NULL)
  OR (OLD.submitted_text IS NOT NULL
      AND (NEW.submitted_text IS NOT OLD.submitted_text
        OR NEW.submitted_digest IS NOT OLD.submitted_digest))
BEGIN
    SELECT RAISE(ABORT, 'guidance submitted form is recorded once with its digest');
END;
