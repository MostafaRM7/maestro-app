CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE workspace_roots (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    canonical_path TEXT NOT NULL,
    display_order INTEGER NOT NULL DEFAULT 0,
    UNIQUE(project_id, canonical_path)
);

CREATE TABLE worktrees (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    canonical_path TEXT NOT NULL,
    git_common_dir TEXT NOT NULL,
    branch_name TEXT,
    head_oid TEXT,
    last_seen_at TEXT NOT NULL,
    UNIQUE(project_id, canonical_path)
);

CREATE TABLE cli_installations (
    id TEXT PRIMARY KEY,
    agent_kind TEXT NOT NULL,
    executable_path TEXT NOT NULL,
    version TEXT,
    fingerprint TEXT NOT NULL,
    auth_state TEXT NOT NULL,
    probed_at TEXT NOT NULL,
    UNIQUE(agent_kind, executable_path)
);

CREATE TABLE capability_snapshots (
    id TEXT PRIMARY KEY,
    cli_installation_id TEXT NOT NULL REFERENCES cli_installations(id) ON DELETE CASCADE,
    capability_json TEXT NOT NULL,
    detected_at TEXT NOT NULL
);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    agent_kind TEXT NOT NULL,
    integration_mode TEXT NOT NULL,
    state TEXT NOT NULL,
    title TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX sessions_project_updated_idx ON sessions(project_id, updated_at DESC);

CREATE TABLE vendor_bindings (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    vendor_session_id TEXT NOT NULL,
    binding_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(session_id, vendor_session_id)
);

CREATE TABLE process_runs (
    id TEXT PRIMARY KEY,
    session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    pid INTEGER,
    invocation_json TEXT NOT NULL,
    channel TEXT NOT NULL,
    state TEXT NOT NULL,
    started_at TEXT NOT NULL,
    exited_at TEXT,
    exit_code INTEGER,
    recovery_json TEXT
);

CREATE TABLE turns (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    completed_at TEXT,
    UNIQUE(session_id, sequence)
);

CREATE TABLE events (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id TEXT REFERENCES process_runs(id) ON DELETE SET NULL,
    sequence INTEGER NOT NULL,
    timestamp TEXT NOT NULL,
    source TEXT NOT NULL,
    kind TEXT NOT NULL,
    visibility TEXT NOT NULL,
    vendor_event_id TEXT,
    payload_json TEXT NOT NULL,
    raw_segment_reference TEXT,
    UNIQUE(session_id, sequence)
);

CREATE INDEX events_session_sequence_idx ON events(session_id, sequence);

CREATE TABLE raw_segments (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    byte_count INTEGER NOT NULL,
    storage_path TEXT NOT NULL
);

CREATE TABLE terminal_tabs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE terminal_segments (
    id TEXT PRIMARY KEY,
    terminal_tab_id TEXT NOT NULL REFERENCES terminal_tabs(id) ON DELETE CASCADE,
    sequence_start INTEGER NOT NULL,
    sequence_end INTEGER NOT NULL,
    byte_count INTEGER NOT NULL,
    storage_path TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE permission_rules (
    id TEXT PRIMARY KEY,
    decision TEXT NOT NULL,
    scope TEXT NOT NULL,
    scope_reference TEXT,
    tool_pattern TEXT,
    command_pattern TEXT,
    path_pattern TEXT,
    security_class TEXT NOT NULL,
    expires_at TEXT,
    created_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE TABLE permission_requests (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    rule_id TEXT REFERENCES permission_rules(id) ON DELETE SET NULL,
    request_json TEXT NOT NULL,
    decision TEXT,
    requested_at TEXT NOT NULL,
    resolved_at TEXT
);

CREATE TABLE artifacts (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    path TEXT,
    kind TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE file_changes (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    diff_text TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE comparison_groups (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    prompt TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE comparison_members (
    comparison_group_id TEXT NOT NULL REFERENCES comparison_groups(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    PRIMARY KEY(comparison_group_id, session_id)
);

CREATE TABLE exports (
    id TEXT PRIMARY KEY,
    format TEXT NOT NULL,
    state TEXT NOT NULL,
    options_json TEXT NOT NULL,
    output_path TEXT,
    created_at TEXT NOT NULL,
    completed_at TEXT
);

CREATE TABLE settings (
    scope TEXT NOT NULL,
    scope_reference TEXT NOT NULL DEFAULT '',
    key TEXT NOT NULL,
    value_json TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(scope, scope_reference, key)
);
