ALTER TABLE project ADD COLUMN hook_skip TEXT NOT NULL DEFAULT '[]'
  CHECK (json_valid(hook_skip) AND json_type(hook_skip) = 'array');
