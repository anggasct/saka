ALTER TABLE spec_draft ADD COLUMN deployment_id TEXT REFERENCES deployment(id);

CREATE UNIQUE INDEX spec_draft_one_live_per_deployment ON spec_draft (deployment_id)
  WHERE deployment_id IS NOT NULL AND state NOT IN ('succeeded','cancelled');
