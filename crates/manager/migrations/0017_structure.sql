-- A finished download without a checksum, checked by its own structure: the format, bytes
-- downloaded again because they were damaged, and whether it is still damaged (as JSON).
ALTER TABLE tasks ADD COLUMN structure TEXT;
