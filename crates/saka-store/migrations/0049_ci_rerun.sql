ALTER TABLE change_request ADD COLUMN ci_rerun TEXT CHECK (ci_rerun IS NULL OR json_valid(ci_rerun));
ALTER TABLE gate_result ADD COLUMN passed_after_rerun TEXT CHECK (passed_after_rerun IS NULL OR json_valid(passed_after_rerun));
