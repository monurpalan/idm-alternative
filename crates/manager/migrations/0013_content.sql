-- What a finished file's first bytes say it is (`kind` or `kind:ext`, see
-- `shardload_engine::sniff::Content::label`); NULL until checked.
ALTER TABLE tasks ADD COLUMN content TEXT;
-- Whether the file name was given (by the user, or the browser), not chosen from the server's
-- answer: only a chosen name is fixed from the content. Lists from before say given.
ALTER TABLE tasks ADD COLUMN name_given INTEGER NOT NULL DEFAULT 1;
-- The title of the page the download came from, for naming a file whose name says nothing.
ALTER TABLE tasks ADD COLUMN page_title TEXT;
