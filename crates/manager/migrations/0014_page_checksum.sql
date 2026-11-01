-- A checksum shown on the page the download was linked from (`algorithm:hex`): checked when
-- the publisher posts none.
ALTER TABLE tasks ADD COLUMN page_checksum TEXT;
