-- When the user wants a download finished by (Unix milliseconds).
ALTER TABLE tasks ADD COLUMN deadline INTEGER;
