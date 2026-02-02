-- M9: Control Plane Initial Schema
-- This migration creates the core tables for specs, plugins, artifacts, and compilations.

-- Specs table: stores metadata about uploaded API specifications
CREATE TABLE specs (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name            TEXT NOT NULL UNIQUE,
    current_sha256  TEXT NOT NULL,
    spec_type       TEXT NOT NULL CHECK (spec_type IN ('openapi', 'asyncapi')),
    spec_version    TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Spec revisions: version history with content stored as bytes
CREATE TABLE spec_revisions (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    spec_id         UUID NOT NULL REFERENCES specs(id) ON DELETE CASCADE,
    revision        INTEGER NOT NULL,
    sha256          TEXT NOT NULL,
    content         BYTEA NOT NULL,
    filename        TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(spec_id, revision)
);

CREATE INDEX idx_spec_revisions_spec_id ON spec_revisions(spec_id);

-- Plugins registry: stores plugin metadata and WASM binaries
CREATE TABLE plugins (
    name            TEXT NOT NULL,
    version         TEXT NOT NULL,
    plugin_type     TEXT NOT NULL CHECK (plugin_type IN ('middleware', 'dispatcher')),
    description     TEXT,
    capabilities    JSONB NOT NULL DEFAULT '[]',
    config_schema   JSONB NOT NULL DEFAULT '{}',
    wasm_binary     BYTEA NOT NULL,
    sha256          TEXT NOT NULL,
    registered_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(name, version)
);

CREATE INDEX idx_plugins_type ON plugins(plugin_type);

-- Artifacts: compiled .bca files with manifest
CREATE TABLE artifacts (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    manifest        JSONB NOT NULL,
    data            BYTEA NOT NULL,
    sha256          TEXT NOT NULL,
    size_bytes      BIGINT NOT NULL,
    compiler_version TEXT NOT NULL,
    compiled_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Artifact-specs junction: tracks which spec revisions are in each artifact
CREATE TABLE artifact_specs (
    artifact_id     UUID NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    spec_id         UUID NOT NULL REFERENCES specs(id) ON DELETE CASCADE,
    spec_revision   INTEGER NOT NULL,
    PRIMARY KEY(artifact_id, spec_id)
);

-- Compilations: async job tracking for spec compilation
CREATE TABLE compilations (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    spec_id         UUID NOT NULL REFERENCES specs(id) ON DELETE CASCADE,
    status          TEXT NOT NULL CHECK (status IN ('pending', 'compiling', 'succeeded', 'failed')) DEFAULT 'pending',
    production      BOOLEAN NOT NULL DEFAULT true,
    additional_specs JSONB DEFAULT '[]',
    artifact_id     UUID REFERENCES artifacts(id) ON DELETE SET NULL,
    errors          JSONB DEFAULT '[]',
    warnings        JSONB DEFAULT '[]',
    started_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at    TIMESTAMPTZ
);

CREATE INDEX idx_compilations_status ON compilations(status);
CREATE INDEX idx_compilations_spec_id ON compilations(spec_id);
