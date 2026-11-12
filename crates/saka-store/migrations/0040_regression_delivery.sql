ALTER TABLE regression_check ADD COLUMN delivered_at INTEGER
  CHECK (delivered_at IS NULL OR delivered_at >= 0);

ALTER TABLE stage_run ADD COLUMN branch_reset_from TEXT;
