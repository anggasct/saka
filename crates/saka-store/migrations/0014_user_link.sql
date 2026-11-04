CREATE TABLE user_link (
  id                 TEXT    PRIMARY KEY,
  owner              TEXT    NOT NULL,
  integration_id     TEXT    NOT NULL,
  provider_user_id   INTEGER NOT NULL CHECK (provider_user_id >= 1),
  login              TEXT    NOT NULL CHECK (length(login) BETWEEN 1 AND 39),
  display_name       TEXT    CHECK (display_name IS NULL OR length(display_name) BETWEEN 1 AND 255),
  email              TEXT    NOT NULL CHECK (length(email) BETWEEN 3 AND 254),
  state              TEXT    NOT NULL CHECK (state IN ('active','needs_relink')),
  relink_cause       TEXT    CHECK (relink_cause IS NULL OR relink_cause IN ('refresh_failed','revoked','expired')),
  access_nonce       BLOB    NOT NULL CHECK (length(access_nonce) = 24),
  access_ciphertext  BLOB    NOT NULL,
  access_expires_at  INTEGER NOT NULL,
  refresh_nonce      BLOB    NOT NULL CHECK (length(refresh_nonce) = 24),
  refresh_ciphertext BLOB    NOT NULL,
  refresh_expires_at INTEGER NOT NULL,
  linked_at          INTEGER NOT NULL,
  refreshed_at       INTEGER,
  created_at         INTEGER NOT NULL,
  updated_at         INTEGER NOT NULL,
  UNIQUE (owner, integration_id),
  CHECK ((state = 'needs_relink') = (relink_cause IS NOT NULL))
) STRICT;

CREATE TABLE user_link_authorization (
  id                TEXT    PRIMARY KEY,
  owner             TEXT    NOT NULL,
  integration_id    TEXT    NOT NULL,
  state             TEXT    NOT NULL CHECK (state IN ('pending','completed','expired','denied','failed')),
  device_nonce      BLOB    CHECK (device_nonce IS NULL OR length(device_nonce) = 24),
  device_ciphertext BLOB,
  user_code         TEXT    NOT NULL CHECK (length(user_code) BETWEEN 1 AND 32),
  verification_uri  TEXT    NOT NULL,
  interval_secs     INTEGER NOT NULL CHECK (interval_secs BETWEEN 1 AND 300),
  expires_at        INTEGER NOT NULL,
  failure           TEXT    CHECK (failure IS NULL OR json_valid(failure)),
  link_id           TEXT,
  created_at        INTEGER NOT NULL,
  updated_at        INTEGER NOT NULL,
  CHECK ((state = 'pending') = (device_ciphertext IS NOT NULL)),
  CHECK ((device_ciphertext IS NULL) = (device_nonce IS NULL)),
  CHECK ((state = 'failed') = (failure IS NOT NULL)),
  CHECK ((state = 'completed') = (link_id IS NOT NULL))
) STRICT;
CREATE UNIQUE INDEX user_link_authorization_one_pending
  ON user_link_authorization (owner, integration_id) WHERE state = 'pending';
