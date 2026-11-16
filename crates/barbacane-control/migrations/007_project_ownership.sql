-- A project's artifacts are deleted with it, as its specs and compilations are.
ALTER TABLE artifacts DROP CONSTRAINT IF EXISTS artifacts_project_id_fkey;
ALTER TABLE artifacts
    ADD CONSTRAINT artifacts_project_id_fkey
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE;

-- The default project receives the specs uploaded with POST /specs; it exists
-- even where an earlier release let it be deleted. Names are unique, so it
-- takes another one when a project has since been given its name.
INSERT INTO projects (id, name, description)
SELECT
    '00000000-0000-0000-0000-000000000001',
    CASE
        WHEN EXISTS (SELECT 1 FROM projects WHERE name = 'Default Project')
            THEN 'Default Project ' || gen_random_uuid()
        ELSE 'Default Project'
    END,
    'Holds the specs uploaded with POST /specs'
WHERE NOT EXISTS (
    SELECT 1 FROM projects WHERE id = '00000000-0000-0000-0000-000000000001'
);
