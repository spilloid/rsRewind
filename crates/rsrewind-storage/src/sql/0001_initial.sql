-- Schema v1. Timestamps are INTEGER unix milliseconds UTC. Media paths are relative to the data
-- root with forward slashes. This file is embedded in the binary and must never change once
-- released: schema changes go in a new numbered migration.

CREATE TABLE sessions (
    id          INTEGER PRIMARY KEY,
    started_at  INTEGER NOT NULL,
    ended_at    INTEGER,
    hostname    TEXT NOT NULL,
    app_version TEXT NOT NULL
);

CREATE TABLE monitors (
    id              INTEGER PRIMARY KEY,
    device_name     TEXT NOT NULL UNIQUE,
    width           INTEGER NOT NULL,
    height          INTEGER NOT NULL,
    "left"          INTEGER NOT NULL,
    "top"           INTEGER NOT NULL,
    dpi             INTEGER NOT NULL,
    primary_monitor INTEGER NOT NULL CHECK (primary_monitor IN (0, 1)),
    last_seen_at    INTEGER NOT NULL
);

CREATE TABLE applications (
    id            INTEGER PRIMARY KEY,
    process_name  TEXT NOT NULL UNIQUE COLLATE NOCASE,
    exe_path      TEXT,
    first_seen_at INTEGER NOT NULL
);

-- NULL class_name values are distinct under UNIQUE, so the store looks windows up with `IS`
-- inside an IMMEDIATE transaction instead of relying on this constraint alone.
CREATE TABLE windows (
    id             INTEGER PRIMARY KEY,
    application_id INTEGER NOT NULL REFERENCES applications (id),
    title          TEXT NOT NULL,
    class_name     TEXT,
    first_seen_at  INTEGER NOT NULL,
    UNIQUE (application_id, title, class_name)
);

CREATE TABLE visual_states (
    id          INTEGER PRIMARY KEY,
    monitor_id  INTEGER NOT NULL REFERENCES monitors (id),
    captured_at INTEGER NOT NULL,
    media_path  TEXT NOT NULL UNIQUE,
    width       INTEGER NOT NULL,
    height      INTEGER NOT NULL,
    byte_size   INTEGER NOT NULL,
    fingerprint INTEGER,
    ocr_status  TEXT NOT NULL DEFAULT 'pending'
                CHECK (ocr_status IN ('pending', 'done', 'failed', 'skipped')),
    ocr_error   TEXT,
    ocr_engine  TEXT,
    ocr_ms      INTEGER
);

CREATE TABLE events (
    id              INTEGER PRIMARY KEY,
    session_id      INTEGER NOT NULL REFERENCES sessions (id),
    kind            TEXT NOT NULL,
    started_at      INTEGER NOT NULL,
    ended_at        INTEGER NOT NULL,
    monitor_id      INTEGER REFERENCES monitors (id),
    visual_state_id INTEGER REFERENCES visual_states (id),
    application_id  INTEGER REFERENCES applications (id),
    window_id       INTEGER REFERENCES windows (id),
    metadata_json   TEXT,
    CHECK (ended_at >= started_at)
);

CREATE TABLE ocr_blocks (
    id              INTEGER PRIMARY KEY,
    visual_state_id INTEGER NOT NULL REFERENCES visual_states (id) ON DELETE CASCADE,
    line_index      INTEGER NOT NULL,
    text            TEXT NOT NULL,
    x               REAL NOT NULL,
    y               REAL NOT NULL,
    width           REAL NOT NULL,
    height          REAL NOT NULL,
    confidence      REAL
);

-- rowid = visual_states.id. Maintained by the store (save_ocr / deletes), not by triggers, so a
-- visual state has exactly one FTS row holding all of its lines joined by '\n'.
CREATE VIRTUAL TABLE ocr_fts USING fts5 (text, tokenize = 'unicode61 remove_diacritics 2');

CREATE TABLE control (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    state        TEXT NOT NULL CHECK (state IN ('recording', 'paused', 'error', 'stopped')),
    paused_until INTEGER,
    updated_at   INTEGER NOT NULL
);

INSERT INTO control (id, state, paused_until, updated_at) VALUES (1, 'recording', NULL, 0);

-- paused_until is an addition to the contract's column list: it lets the status row carry a timed
-- pause exactly like `control` does, so CaptureState round-trips without a JSON column.
CREATE TABLE recorder_status (
    id            INTEGER PRIMARY KEY CHECK (id = 1),
    pid           INTEGER NOT NULL,
    started_at    INTEGER NOT NULL,
    heartbeat_at  INTEGER NOT NULL,
    state         TEXT NOT NULL CHECK (state IN ('recording', 'paused', 'error', 'stopped')),
    paused_until  INTEGER,
    counters_json TEXT NOT NULL
);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT
);

-- Foreign-key indexes. windows(application_id) is covered by the leading column of its UNIQUE
-- constraint. events(session_id) is covered by events_session_monitor, whose second column serves
-- record_observation's "latest observation for this monitor in this session" lookup.
CREATE INDEX visual_states_monitor ON visual_states (monitor_id);
CREATE INDEX events_session_monitor ON events (session_id, monitor_id);
CREATE INDEX events_monitor ON events (monitor_id);
CREATE INDEX events_visual_state ON events (visual_state_id);
CREATE INDEX events_application ON events (application_id);
CREATE INDEX events_window ON events (window_id);
CREATE INDEX ocr_blocks_visual_state ON ocr_blocks (visual_state_id);

CREATE INDEX events_started_at ON events (started_at);
CREATE INDEX events_kind_started_at ON events (kind, started_at);
CREATE INDEX visual_states_captured_at ON visual_states (captured_at);
CREATE INDEX visual_states_ocr_status ON visual_states (ocr_status);
