-- What kind of failure `error` describes, so the UI can offer the matching fix (a login
-- prompt, "refresh link"…): auth, expired, changed, disk, checksum, or NULL.
ALTER TABLE tasks ADD COLUMN error_kind TEXT;
