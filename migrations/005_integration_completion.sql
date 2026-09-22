ALTER TABLE tasks ADD COLUMN legacy_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE import_records ADD COLUMN preview_json TEXT NOT NULL DEFAULT '{}';
