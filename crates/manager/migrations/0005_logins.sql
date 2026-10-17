-- Logins remembered per site (host, with the port when it is not the default). The password
-- is encrypted like the request headers (see secret.rs).
CREATE TABLE logins (
    site     TEXT PRIMARY KEY,
    user     TEXT NOT NULL,
    password TEXT NOT NULL,
    saved_at INTEGER NOT NULL
);
