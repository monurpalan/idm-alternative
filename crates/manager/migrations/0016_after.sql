-- What a download asked to have done once finished (unpack, move), as JSON, and how that and
-- the user's rules for finished downloads went.
ALTER TABLE tasks ADD COLUMN after TEXT;
ALTER TABLE tasks ADD COLUMN after_log TEXT;
