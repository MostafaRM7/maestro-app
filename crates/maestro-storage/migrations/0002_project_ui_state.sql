ALTER TABLE projects ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN last_opened_at TEXT;

CREATE TABLE window_layouts (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    window_key TEXT NOT NULL,
    layout_json TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(project_id, window_key)
);
