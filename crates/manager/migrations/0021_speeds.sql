-- When a download first began downloading (Unix ms); NULL until then, and again when it
-- is downloaded from scratch.
ALTER TABLE tasks ADD COLUMN started_at INTEGER;

-- How fast each download went from its first byte to its end, for the graph in its
-- details: JSON (see `speeds.rs`). Apart from `tasks`, so listing the downloads does not
-- read it.
CREATE TABLE speeds (
    task_id INTEGER PRIMARY KEY REFERENCES tasks (id) ON DELETE CASCADE,
    history TEXT NOT NULL
);
