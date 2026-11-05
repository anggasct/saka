ALTER TABLE project ADD COLUMN workflow TEXT NOT NULL
  DEFAULT '{"preset":"full_loop","spec":true,"change_request":null,"stages":[{"stage":"dev","trigger":"auto"},{"stage":"review","trigger":"auto"},{"stage":"fix","trigger":"auto"},{"stage":"merge","trigger":"manual"}]}'
  CHECK (json_valid(workflow));

ALTER TABLE project DROP COLUMN workflow_preset;
