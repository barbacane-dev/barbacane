-- M10: Add Projects as core organizing entity
-- Projects group specs, plugin configurations, compilations, and artifacts together.

-- Projects table: the central organizing entity
CREATE TABLE projects (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name            TEXT NOT NULL UNIQUE,
    description     TEXT,
    production_mode BOOLEAN NOT NULL DEFAULT true,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_projects_created_at ON projects(created_at DESC);

-- Project plugin configurations: per-project plugin settings
-- Plugins remain global (registry), but each project configures which plugins to use
CREATE TABLE project_plugin_configs (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id      UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    plugin_name     TEXT NOT NULL,
    plugin_version  TEXT NOT NULL,
    enabled         BOOLEAN NOT NULL DEFAULT true,
    priority        INTEGER NOT NULL DEFAULT 0,
    config          JSONB NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (plugin_name, plugin_version) REFERENCES plugins(name, version) ON DELETE RESTRICT,
    UNIQUE(project_id, plugin_name)
);

CREATE INDEX idx_project_plugin_configs_project ON project_plugin_configs(project_id);

-- Create default project for existing data migration
INSERT INTO projects (id, name, description) VALUES
    ('00000000-0000-0000-0000-000000000001', 'Default Project', 'Auto-migrated project containing pre-existing specs');

-- Link specs to projects
ALTER TABLE specs ADD COLUMN project_id UUID REFERENCES projects(id) ON DELETE CASCADE;

-- Migrate existing specs to default project
UPDATE specs SET project_id = '00000000-0000-0000-0000-000000000001' WHERE project_id IS NULL;

-- Make project_id required
ALTER TABLE specs ALTER COLUMN project_id SET NOT NULL;

-- Update unique constraint: name is now unique per project (not globally)
ALTER TABLE specs DROP CONSTRAINT IF EXISTS specs_name_key;
ALTER TABLE specs ADD CONSTRAINT specs_project_name_unique UNIQUE(project_id, name);

CREATE INDEX idx_specs_project_id ON specs(project_id);

-- Link compilations to projects
ALTER TABLE compilations ADD COLUMN project_id UUID REFERENCES projects(id) ON DELETE CASCADE;

-- Migrate existing compilations: inherit project_id from their spec
UPDATE compilations c SET project_id = s.project_id FROM specs s WHERE c.spec_id = s.id AND c.project_id IS NULL;

CREATE INDEX idx_compilations_project_id ON compilations(project_id);

-- Link artifacts to projects (SET NULL to preserve artifacts when project deleted)
ALTER TABLE artifacts ADD COLUMN project_id UUID REFERENCES projects(id) ON DELETE SET NULL;

-- Migrate existing artifacts: inherit project_id from their compilation
UPDATE artifacts a SET project_id = c.project_id
FROM compilations c
WHERE a.id = c.artifact_id AND c.project_id IS NOT NULL AND a.project_id IS NULL;

CREATE INDEX idx_artifacts_project_id ON artifacts(project_id);
