-- When the download's link was last given (Unix ms): set when the link is refreshed, so a
-- link's age is measured from then. NULL means since `added_at`.
ALTER TABLE tasks ADD COLUMN link_at INTEGER;
