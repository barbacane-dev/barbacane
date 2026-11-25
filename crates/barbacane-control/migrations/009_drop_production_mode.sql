-- Compilations carry their own production flag; projects have none.
ALTER TABLE projects DROP COLUMN production_mode;
