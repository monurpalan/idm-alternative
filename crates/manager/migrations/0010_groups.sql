-- Downloads added together (a playlist, a channel, a batch, the files of a site): each is a
-- download of its own, and the list can fold them into one row.
CREATE TABLE groups (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    name     TEXT    NOT NULL,
    added_at INTEGER NOT NULL  -- Unix milliseconds
);

ALTER TABLE tasks ADD COLUMN group_id INTEGER REFERENCES groups (id) ON DELETE SET NULL;
