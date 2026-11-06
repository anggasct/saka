ALTER TABLE spec_draft ADD COLUMN target_kind TEXT NOT NULL DEFAULT 'new' CHECK (target_kind IN ('new','revise'));
