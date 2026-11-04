ALTER TABLE environment ADD COLUMN docs_json TEXT CHECK (docs_json IS NULL OR json_valid(docs_json));
