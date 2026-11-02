CREATE TABLE webhook_delivery (
  id              TEXT    PRIMARY KEY,
  integration_id  TEXT    NOT NULL,
  delivery_id     TEXT    NOT NULL CHECK (length(delivery_id) BETWEEN 1 AND 64),
  event           TEXT    NOT NULL CHECK (length(event) BETWEEN 1 AND 64),
  action          TEXT,
  repo_id         TEXT    REFERENCES repo(id),
  outcome         TEXT    NOT NULL CHECK (outcome IN ('refreshed','ignored')),
  change_requests INTEGER NOT NULL CHECK (change_requests >= 0),
  received_at     INTEGER NOT NULL,
  UNIQUE (integration_id, delivery_id),
  CHECK ((outcome = 'refreshed') = (change_requests > 0))
) STRICT;
CREATE INDEX webhook_delivery_received_idx ON webhook_delivery (received_at);
CREATE INDEX change_request_head_idx ON change_request (repo_id, head_sha);
