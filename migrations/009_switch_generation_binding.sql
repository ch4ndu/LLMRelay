ALTER TABLE switch_intents ADD COLUMN new_generation_id TEXT REFERENCES role_generations(id);
ALTER TABLE sessions ADD COLUMN resume_count INTEGER NOT NULL DEFAULT 0;

CREATE INDEX switch_intents_new_generation_idx
ON switch_intents(new_generation_id)
WHERE new_generation_id IS NOT NULL;
