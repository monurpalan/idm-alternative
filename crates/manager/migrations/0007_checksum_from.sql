-- Where the checksum came from when the user gave none (e.g. `SHA256SUMS`, `GitHub release`);
-- NULL for one the user gave.
ALTER TABLE tasks ADD COLUMN checksum_from TEXT;
