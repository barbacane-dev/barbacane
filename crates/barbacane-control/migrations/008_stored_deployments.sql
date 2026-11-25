-- The artifact a project's data planes run: the one its last deploy named,
-- sent to every data plane that registers without it.
ALTER TABLE projects ADD COLUMN deployed_artifact_id UUID REFERENCES artifacts(id) ON DELETE SET NULL;
ALTER TABLE projects ADD COLUMN deployed_at TIMESTAMPTZ;

-- Why a data plane did not load the last artifact it was sent; cleared when
-- it loads one.
ALTER TABLE data_planes ADD COLUMN deploy_error TEXT;
