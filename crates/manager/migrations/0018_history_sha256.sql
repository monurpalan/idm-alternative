-- Every finished file's SHA-256 (`sha256:<hex>`), for finding the same content under another
-- address before it is downloaded again.
ALTER TABLE history ADD COLUMN sha256 TEXT;
CREATE INDEX history_sha256 ON history (sha256);
