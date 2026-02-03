-- Milestone 12: Data plane connection feature
-- Adds tables for tracking connected data planes and API keys

-- Data plane connections
CREATE TABLE data_planes (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id      UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name            TEXT,
    artifact_id     UUID REFERENCES artifacts(id) ON DELETE SET NULL,
    status          TEXT NOT NULL DEFAULT 'offline' CHECK (status IN ('online', 'offline', 'deploying')),
    last_seen       TIMESTAMPTZ,
    connected_at    TIMESTAMPTZ,
    metadata        JSONB DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_data_planes_project ON data_planes(project_id);
CREATE INDEX idx_data_planes_status ON data_planes(status);

-- API keys for data plane authentication
CREATE TABLE api_keys (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id      UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    key_hash        TEXT NOT NULL,
    key_prefix      TEXT NOT NULL,
    scopes          TEXT[] NOT NULL DEFAULT ARRAY['data-plane:connect'],
    expires_at      TIMESTAMPTZ,
    last_used_at    TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revoked_at      TIMESTAMPTZ,
    UNIQUE(project_id, name)
);

CREATE INDEX idx_api_keys_project ON api_keys(project_id);
CREATE INDEX idx_api_keys_prefix ON api_keys(key_prefix);
