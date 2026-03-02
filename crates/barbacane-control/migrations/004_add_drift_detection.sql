-- ADR-0021: Config provenance and drift detection

-- Artifact hash reported by data plane in heartbeat
ALTER TABLE data_planes ADD COLUMN artifact_hash TEXT;

-- Whether the control plane detected configuration drift
ALTER TABLE data_planes ADD COLUMN drift_detected BOOLEAN NOT NULL DEFAULT false;
