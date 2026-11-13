ALTER TABLE work_item ADD COLUMN redo_of TEXT REFERENCES work_item(id);
ALTER TABLE work_item ADD COLUMN redo_reason TEXT CHECK (redo_reason IS NULL OR length(redo_reason) BETWEEN 1 AND 500);
ALTER TABLE work_item ADD COLUMN redo_note TEXT CHECK (redo_note IS NULL OR length(redo_note) <= 2000);
CREATE UNIQUE INDEX work_item_one_redo ON work_item (redo_of) WHERE redo_of IS NOT NULL;
