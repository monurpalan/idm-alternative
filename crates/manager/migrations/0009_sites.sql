-- What downloads learned about each site (host, with the port when it is not the default),
-- so the next download from it starts right: forgotten 30 days after it was last seen.
CREATE TABLE sites (
    site            TEXT PRIMARY KEY,
    max_connections INTEGER,           -- refused connections beyond this many
    no_ranges       INTEGER NOT NULL DEFAULT 0,
    browser         TEXT,              -- the identity it accepted after refusing ours
    link_lifetime   INTEGER,           -- seconds a link of it worked before it expired
    seen_at         INTEGER NOT NULL   -- Unix milliseconds
);
