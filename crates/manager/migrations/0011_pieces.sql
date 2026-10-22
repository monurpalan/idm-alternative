-- Hashes of the file's pieces from its metalink, JSON `{"length": n, "hashes": [...]}`:
-- a corrupt piece is downloaded again alone. NULL for none.
ALTER TABLE tasks ADD COLUMN pieces TEXT;
