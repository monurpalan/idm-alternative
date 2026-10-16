//! Subtitles of a stream, saved as a WebVTT (or TTML) file next to the video: an HLS
//! subtitle playlist's segments joined into one file, or a DASH text representation.

use std::path::Path;

use crate::download::{Options, http_config};
use crate::error::{Error, Result};
use crate::hls::{Playlist, parse_playlist};
use crate::remote::Http;

#[derive(Debug, Clone)]
pub enum SubtitleSource {
    /// An HLS subtitle rendition's playlist.
    Hls { playlist: String },
    /// A text representation of a DASH manifest.
    Dash {
        manifest: String,
        representation: String,
    },
}

const MAX_TEXT: usize = 16 << 20;

/// Downloads the subtitles to `output` and returns its size.
pub async fn download_subtitles(
    source: &SubtitleSource,
    output: &Path,
    headers: &[(String, String)],
    opts: &Options,
) -> Result<u64> {
    let http = Http::new(&http_config(headers, opts))?;
    let text = match source {
        SubtitleSource::Hls { playlist } => {
            let (list, url) = http.text(playlist, MAX_TEXT).await?;
            let Playlist::Media(m) = parse_playlist(&url, &list)? else {
                return Err(Error::State("a subtitle playlist lists variants".into()));
            };
            let mut parts = Vec::new();
            for seg in &m.segments {
                parts.push(fetch_text(&http, &seg.url, seg.range).await?);
            }
            merge_vtt(&parts)?
        }
        SubtitleSource::Dash {
            manifest,
            representation,
        } => {
            let (mpd, url) = http.text(manifest, MAX_TEXT).await?;
            let track = idma_media::dash_track(&url, &mpd, representation)
                .map_err(|e| Error::State(e.0))?;
            if track.single {
                // A whole WebVTT or TTML file: kept as it is.
                fetch_text(&http, &track.segments[0].url, None).await?
            } else {
                let mut parts = Vec::new();
                for seg in &track.segments {
                    parts.push(fetch_text(&http, &seg.url, seg.range).await?);
                }
                merge_vtt(&parts)?
            }
        }
    };
    tokio::fs::write(output, &text).await?;
    Ok(text.len() as u64)
}

async fn fetch_text(http: &Http, url: &str, range: Option<(u64, u64)>) -> Result<String> {
    let resp = http.segment(url, range, 0).await?;
    let bytes = resp.bytes().await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Joins WebVTT segments into one file: one `WEBVTT` header, then every segment's cues in
/// order. Segment headers (and their `X-TIMESTAMP-MAP`) are dropped; cue times are kept as
/// the segments give them.
pub(crate) fn merge_vtt(parts: &[String]) -> Result<String> {
    let mut out = String::from("WEBVTT\n\n");
    for part in parts {
        let text = part.trim_start_matches('\u{feff}').replace("\r\n", "\n");
        if !text.trim_start().starts_with("WEBVTT") {
            return Err(Error::State(
                "these subtitles are not WebVTT (subtitles inside MP4 segments are not supported)"
                    .into(),
            ));
        }
        // The header block ends at the first blank line.
        let body = text.split_once("\n\n").map_or("", |(_, rest)| rest).trim();
        if !body.is_empty() {
            out.push_str(body);
            out.push_str("\n\n");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging() {
        let parts = vec![
            "\u{feff}WEBVTT\r\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000\r\n\r\n00:00:00.000 --> 00:00:02.000\r\nBir\r\n".to_owned(),
            "WEBVTT\n\n".to_owned(),
            "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000\n\n1\n00:00:04.000 --> 00:00:06.000\nİki\n\n00:00:06.000 --> 00:00:07.000\nÜç\n".to_owned(),
        ];
        let merged = merge_vtt(&parts).expect("merge");
        assert_eq!(merged.matches("WEBVTT").count(), 1);
        assert!(!merged.contains("X-TIMESTAMP-MAP"));
        assert_eq!(merged.matches(" --> ").count(), 3);
        assert!(merged.find("Bir").expect("1") < merged.find("Üç").expect("3"));
        assert!(merge_vtt(&["not vtt".to_owned()]).is_err());
    }
}
