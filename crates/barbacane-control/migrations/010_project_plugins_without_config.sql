-- A project plugin is a registry version the project's specs may use, with
-- its limits; the specs configure it.
ALTER TABLE project_plugin_configs DROP COLUMN priority;
ALTER TABLE project_plugin_configs DROP COLUMN config;
