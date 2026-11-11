-- Resource limits a project sets for a plugin, replacing the plugin's own
-- key by key (memory_mib, fuel, timeout_ms). NULL sets none.
ALTER TABLE project_plugin_configs ADD COLUMN limits JSONB;
