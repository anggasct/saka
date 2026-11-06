ALTER TABLE stage_run ADD COLUMN queue_native INTEGER NOT NULL DEFAULT 0 CHECK (queue_native IN (0, 1));
