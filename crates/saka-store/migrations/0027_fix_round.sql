ALTER TABLE stage_run ADD COLUMN fix TEXT CHECK (fix IS NULL OR json_valid(fix));
