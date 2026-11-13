ALTER TABLE spec_draft_attempt ADD COLUMN code_commits TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(code_commits));
