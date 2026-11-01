-- How fast the site's downloads went (bytes per second, averaged over the recent ones): what
-- time left is first estimated from, while a new download is just starting.
ALTER TABLE sites ADD COLUMN speed INTEGER;
