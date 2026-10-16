//! Query-string parameters that select how the server misbehaves.

use serde::{Deserialize, Deserializer, de};

/// A byte count or byte rate written as `123`, `64K`, `10M` or `2G` (binary units).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size(pub u64);

impl std::str::FromStr for Size {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let (num, mult) = match s.chars().last() {
            Some('K' | 'k') => (&s[..s.len() - 1], 1u64 << 10),
            Some('M' | 'm') => (&s[..s.len() - 1], 1 << 20),
            Some('G' | 'g') => (&s[..s.len() - 1], 1 << 30),
            _ => (s, 1),
        };
        let n: u64 = num.parse().map_err(|_| format!("invalid size `{s}`"))?;
        n.checked_mul(mult)
            .map(Size)
            .ok_or_else(|| format!("size `{s}` overflows"))
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(de::Error::custom)
    }
}

/// One slowed connection: the `n`-th GET to this URL is served at `rate` bytes/s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlowConn {
    pub nth: u64,
    pub rate: Size,
}

impl<'de> Deserialize<'de> for SlowConn {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let (n, rate) = s
            .split_once(':')
            .ok_or_else(|| de::Error::custom("slow_conn must be `n:rate`, e.g. `3:100K`"))?;
        Ok(SlowConn {
            nth: n.parse().map_err(de::Error::custom)?,
            rate: rate.parse().map_err(de::Error::custom)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EtagMode {
    /// Same validator on every response.
    #[default]
    Static,
    /// A new ETag / Last-Modified on every response, as if the file changed.
    Rotate,
    /// No ETag header at all (Last-Modified is still sent).
    None,
}

fn flag<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    match String::deserialize(d)?.to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Ok(true),
        "0" | "false" | "off" | "no" => Ok(false),
        other => Err(de::Error::custom(format!("invalid flag `{other}`"))),
    }
}

/// Everything a test URL can ask the server to do. All fields are optional.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Scenario {
    /// File size.
    pub size: Size,
    /// Content seed; the same seed always produces the same bytes.
    pub seed: u64,
    /// Honour `Range` requests and advertise `Accept-Ranges`.
    #[serde(deserialize_with = "flag")]
    pub ranges: bool,
    /// Advertise ranges but answer every ranged request with `200` and the whole file.
    #[serde(deserialize_with = "flag")]
    pub range_as_200: bool,
    /// Per-connection rate limit in bytes/s.
    pub rate: Option<Size>,
    /// Slow down one specific connection.
    pub slow_conn: Option<SlowConn>,
    /// Stop sending (but keep the connection open) after this many bytes of a response.
    pub stall_after: Option<Size>,
    /// Reset the connection after this many bytes of a response.
    pub drop_after: Option<Size>,
    /// Reply `503` when more than this many responses are in flight for the URL.
    pub max_conns: Option<u32>,
    /// The URL starts returning `403` this many seconds after its first request.
    pub ttl: Option<u64>,
    /// How validators behave.
    pub etag: EtagMode,
    /// GET requests after the first `n` get `200 text/html` (a login / error page).
    pub html_error_after: Option<u64>,
    /// Omit `Content-Length` and use chunked encoding (implies no range support).
    #[serde(deserialize_with = "flag")]
    pub no_length: bool,
    /// Redirect this many times before serving.
    pub redirect: u32,
    /// Every redirect leads to a new URL (a `hop` parameter is added), like a CDN that
    /// signs each link it hands out: with `ttl`, each resolved link expires on its own.
    #[serde(deserialize_with = "flag")]
    pub redirect_unique: bool,
    /// Send `Content-Disposition` with this filename.
    pub cd: Option<String>,
    /// `user:pass`: requests without these Basic credentials get `401` with
    /// `WWW-Authenticate`, like a password-protected folder.
    pub auth: Option<String>,
    /// Delay before the response headers, in milliseconds.
    pub latency_ms: u64,
}

impl Default for Scenario {
    fn default() -> Self {
        Self {
            size: Size(100 << 20),
            seed: 1,
            ranges: true,
            range_as_200: false,
            rate: None,
            slow_conn: None,
            stall_after: None,
            drop_after: None,
            max_conns: None,
            ttl: None,
            etag: EtagMode::Static,
            html_error_after: None,
            no_length: false,
            redirect: 0,
            redirect_unique: false,
            cd: None,
            auth: None,
            latency_ms: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!("123".parse::<Size>(), Ok(Size(123)));
        assert_eq!("64K".parse::<Size>(), Ok(Size(64 << 10)));
        assert_eq!("10m".parse::<Size>(), Ok(Size(10 << 20)));
        assert_eq!("2G".parse::<Size>(), Ok(Size(2 << 30)));
        assert!("x".parse::<Size>().is_err());
    }
}
