-- The address a download ended up at after redirects, learned on its first start.
ALTER TABLE tasks ADD COLUMN final_url TEXT;

-- Every finished download, kept when it leaves the list: for "you downloaded this before" and
-- "where did this file come from". `task_id` is the download while it is still on the list.
CREATE TABLE history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id     INTEGER,
    url         TEXT    NOT NULL,
    final_url   TEXT,
    page        TEXT,              -- the page it was downloaded from (referrer), if known
    name        TEXT    NOT NULL,
    path        TEXT    NOT NULL,
    size        INTEGER,
    checksum    TEXT,              -- `algorithm:hex` the file was checked against
    finished_at INTEGER NOT NULL   -- Unix milliseconds
);

CREATE INDEX history_url ON history (url);
CREATE INDEX history_final_url ON history (final_url);
CREATE INDEX history_size ON history (size);
CREATE INDEX history_task ON history (task_id);

INSERT INTO history (task_id, url, name, path, size, checksum, finished_at)
SELECT id, url, name, path, size, checksum, COALESCE(finished_at, added_at)
FROM tasks
WHERE status = 'done' AND path IS NOT NULL AND name IS NOT NULL;
