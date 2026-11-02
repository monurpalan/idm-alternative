-- What an archive, a video or a document holds, read from its skeleton while it downloads
-- (JSON; see shardload_engine::skeleton).
ALTER TABLE tasks ADD COLUMN skeleton TEXT;
