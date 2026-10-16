-- One row per download in the list. Live progress is not stored here (ADR 0001): `received`
-- is refreshed at most about once per second, for showing the list after a restart.
CREATE TABLE tasks (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    url         TEXT    NOT NULL,
    headers     TEXT    NOT NULL DEFAULT '[]', -- JSON [[name, value], ...]
    dir         TEXT    NOT NULL,
    name        TEXT,                          -- NULL until the server was asked
    path        TEXT,                          -- full output path, once chosen
    size        INTEGER,
    received    INTEGER NOT NULL DEFAULT 0,
    status      TEXT    NOT NULL,
    error       TEXT,
    category    TEXT    NOT NULL,
    connections INTEGER NOT NULL,
    speed_limit INTEGER,                       -- bytes per second, NULL = none
    checksum    TEXT,
    added_at    INTEGER NOT NULL,              -- Unix milliseconds
    finished_at INTEGER,
    queue_pos   INTEGER NOT NULL
);

CREATE INDEX tasks_queue ON tasks (status, queue_pos);
