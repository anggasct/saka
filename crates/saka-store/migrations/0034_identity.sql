CREATE TABLE credential (
  id            TEXT PRIMARY KEY,
  name          TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
  passkey       TEXT NOT NULL CHECK (json_valid(passkey)),
  created_at    INTEGER NOT NULL,
  last_used_at  INTEGER,
  revoked_at    INTEGER
) STRICT;

CREATE TABLE device (
  id            TEXT PRIMARY KEY,
  name          TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
  kind          TEXT NOT NULL CHECK (kind IN ('local_cli','client')),
  token_sha256  TEXT NOT NULL UNIQUE,
  created_at    INTEGER NOT NULL,
  last_used_at  INTEGER,
  revoked_at    INTEGER
) STRICT;

CREATE UNIQUE INDEX device_one_local_cli ON device(kind) WHERE kind = 'local_cli';

CREATE TABLE login_session (
  id            TEXT PRIMARY KEY,
  credential_id TEXT NOT NULL REFERENCES credential(id),
  token_sha256  TEXT NOT NULL UNIQUE,
  created_at    INTEGER NOT NULL,
  expires_at    INTEGER NOT NULL,
  revoked_at    INTEGER
) STRICT;

CREATE INDEX login_session_credential ON login_session(credential_id);

CREATE TABLE pairing_code (
  id            TEXT PRIMARY KEY,
  purpose       TEXT NOT NULL CHECK (purpose IN ('setup','passkey','token')),
  code_sha256   TEXT NOT NULL UNIQUE,
  failed        INTEGER NOT NULL DEFAULT 0 CHECK (failed >= 0),
  created_by    TEXT NOT NULL,
  created_at    INTEGER NOT NULL,
  expires_at    INTEGER NOT NULL,
  used_at       INTEGER
) STRICT;

CREATE INDEX pairing_code_live ON pairing_code(purpose, expires_at) WHERE used_at IS NULL;
