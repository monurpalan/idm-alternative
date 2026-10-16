//! What a stream offers: the video qualities, audio tracks and subtitles in an HLS master
//! playlist or a DASH manifest, so the user can pick before downloading.

mod dash;
mod dash_segments;
mod hls;

use serde::Serialize;

pub use dash::parse_mpd;
pub use dash_segments::{DashTrack, Piece, dash_track};
pub use hls::parse_m3u8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Hls,
    Dash,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Streams {
    pub format: Format,
    /// Best first.
    pub video: Vec<Variant>,
    pub audio: Vec<Track>,
    pub subtitles: Vec<Track>,
    /// Seconds, when the playlist says (not for live streams).
    pub duration: Option<f64>,
    /// Still being broadcast: no end yet.
    pub live: bool,
    /// How the media is encrypted, if it is: `AES-128` and `SAMPLE-AES` (HLS) can be
    /// decrypted with keys the playlist names; `DRM` cannot be downloaded usefully.
    pub encryption: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Variant {
    /// The variant's playlist (HLS) or the representation id (DASH).
    pub id: String,
    /// Bits per second.
    pub bandwidth: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codecs: Option<String>,
    pub frame_rate: Option<f64>,
    /// HLS: the group of audio tracks that goes with this variant.
    pub audio_group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Track {
    /// The track's playlist (HLS) or representation id (DASH); `None` when the audio is
    /// muxed into the video.
    pub id: Option<String>,
    pub name: Option<String>,
    pub language: Option<String>,
    pub group: Option<String>,
    pub default: bool,
    pub bandwidth: Option<u64>,
    pub codecs: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Reads `text` (from `url`) as whatever playlist it is.
pub fn parse(url: &str, text: &str) -> Result<Streams, Error> {
    let start = text.trim_start_matches('\u{feff}').trim_start();
    if start.starts_with("#EXTM3U") {
        parse_m3u8(url, text)
    } else if start.starts_with('<') {
        parse_mpd(url, text)
    } else {
        Err(Error("not an HLS playlist or a DASH manifest".into()))
    }
}

/// `relative` resolved against `base`; as it is when either does not parse.
pub(crate) fn resolve(base: &str, relative: &str) -> String {
    url::Url::parse(base)
        .and_then(|b| b.join(relative))
        .map_or_else(|_| relative.to_owned(), |u| u.to_string())
}

/// Highest resolution first, then highest bandwidth.
pub(crate) fn sort_best_first(v: &mut [Variant]) {
    v.sort_by(|a, b| {
        let px = |x: &Variant| u64::from(x.width.unwrap_or(0)) * u64::from(x.height.unwrap_or(0));
        px(b).cmp(&px(a)).then(b.bandwidth.cmp(&a.bandwidth))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_format() {
        assert_eq!(
            parse("https://x/a.m3u8", "\u{feff}#EXTM3U\n#EXTINF:4,\ns.ts\n")
                .expect("hls")
                .format,
            Format::Hls
        );
        assert!(parse("https://x/a", "<html></html>").is_err());
        assert!(parse("https://x/a", "hello").is_err());
    }

    #[test]
    fn resolves_relative_uris() {
        assert_eq!(
            resolve("https://cdn.x/v/master.m3u8?t=1", "720/index.m3u8"),
            "https://cdn.x/v/720/index.m3u8"
        );
        assert_eq!(
            resolve("https://cdn.x/v/m.m3u8", "/abs.m3u8"),
            "https://cdn.x/abs.m3u8"
        );
        assert_eq!(
            resolve("https://cdn.x/v/m.m3u8", "https://o/x.m3u8"),
            "https://o/x.m3u8"
        );
    }
}
