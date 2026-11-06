ALTER TABLE project_repo ADD COLUMN autonomy TEXT CHECK (autonomy IS NULL OR autonomy IN ('strict','standard','autonomous'));
ALTER TABLE project DROP COLUMN autonomy;
