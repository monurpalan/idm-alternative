-- A stream download (HLS): JSON `{"audio": url or null}`; the task's url is the video
-- playlist. NULL for a plain file.
ALTER TABLE tasks ADD COLUMN stream TEXT;
