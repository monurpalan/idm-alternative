//! HLS: downloads the segments of a stream's playlists in parallel, decrypts AES-128, joins
//! them, and (with ffmpeg) puts video and audio into one file without re-encoding.
//!
//! Segments are written to `<output>.shardload-hls/` as they complete (`.part` first, renamed when
//! whole), so a stopped download continues with the segments that are missing.

mod fetch;
mod live;
mod parse;
mod runs;

pub use fetch::*;
pub use live::*;
pub(crate) use parse::*;
use runs::{join_runs, map_file};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

use crate::download::{Options, Outcome, Phase, Progress, http_config};
use crate::error::{Error, Result};
use crate::remote::Http;
use crate::state::sibling;

/// What to download: a media playlist (or a master one, whose best variant is taken), an
/// optional separate audio playlist, and where the result goes.
#[derive(Debug, Clone)]
pub struct StreamRequest {
    pub video: String,
    pub audio: Option<String>,
    pub output: PathBuf,
    pub headers: Vec<(String, String)>,
    /// Joins the tracks into `output`; without it a single track's segments are joined as
    /// they are (`.ts`), and a separate audio track is an error.
    pub ffmpeg: Option<PathBuf>,
    /// Only this part of the stream: only the segments it falls in are downloaded, then
    /// ffmpeg (needed) cuts it out.
    pub clip: Option<Clip>,
}

/// A part of a stream to keep, in seconds from its start.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Clip {
    pub start: f64,
    pub end: f64,
    /// Cut at exactly these times, which re-encodes the video; otherwise the cut is copied
    /// from the keyframe at or before `start` (fast, no quality lost, may begin a moment
    /// early).
    #[serde(default)]
    pub exact: bool,
}

/// Playlists larger than this are not playlists.
const MAX_PLAYLIST: usize = 16 << 20;
/// Segments fetched at once when `connections` allows more.
const MAX_PARALLEL: usize = 16;

#[cfg(test)]
mod tests {
    use super::*;

    fn timed(lengths: &[f64]) -> MediaPlaylist {
        MediaPlaylist {
            init: None,
            maps: Vec::new(),
            segments: lengths
                .iter()
                .enumerate()
                .map(|(i, d)| Segment {
                    url: format!("s{i}.ts"),
                    range: None,
                    key: None,
                    duration: Some(*d),
                })
                .collect(),
            ended: true,
            ..MediaPlaylist::default()
        }
    }

    #[test]
    fn a_clip_keeps_only_the_segments_it_falls_in() {
        let p = timed(&[4.0; 15]); // 60 s
        let clip = |start, end| Clip {
            start,
            end,
            exact: false,
        };
        assert_eq!(window(&p, clip(10.0, 20.0)), (vec![2, 3, 4], 8.0));
        assert_eq!(window(&p, clip(8.0, 12.0)), (vec![2], 8.0));
        assert_eq!(window(&p, clip(0.0, 0.5)), (vec![0], 0.0));
        assert_eq!(window(&p, clip(58.0, 999.0)), (vec![14], 56.0));
        // Past the end, or lengths unknown: everything.
        assert_eq!(window(&p, clip(100.0, 110.0)).0.len(), 15);
        let mut unknown = timed(&[4.0, 4.0]);
        unknown.segments[1].duration = None;
        assert_eq!(window(&unknown, clip(5.0, 6.0)), (vec![0, 1], 0.0));
        let files = track_files(Path::new("d"), "video", &p, Some(clip(10.0, 20.0)));
        let names: Vec<String> = files.iter().map(|(_, f)| f.display().to_string()).collect();
        assert_eq!(
            names,
            [
                Path::new("d")
                    .join("video-000002.seg")
                    .display()
                    .to_string(),
                Path::new("d")
                    .join("video-000003.seg")
                    .display()
                    .to_string(),
                Path::new("d")
                    .join("video-000004.seg")
                    .display()
                    .to_string(),
            ]
        );
    }

    #[test]
    fn master_picks_the_best_variant_and_its_audio() {
        let text = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"x\",URI=\"a1.m3u8\"\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"y\",DEFAULT=YES,URI=\"a2.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=9,RESOLUTION=640x360,AUDIO=\"a\"\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=5,RESOLUTION=1920x1080,AUDIO=\"a\"\nhigh.m3u8\n";
        assert_eq!(
            parse_playlist("https://h/x/m.m3u8", text).expect("parse"),
            Playlist::Best {
                video: "https://h/x/high.m3u8".into(),
                audio: Some("https://h/x/a2.m3u8".into()),
            }
        );
    }

    #[test]
    fn an_init_section_that_changes_part_way_is_kept_with_its_first_segment() {
        let text = "#EXTM3U
#EXT-X-MAP:URI=\"a.mp4\"
#EXTINF:2,
a1.m4s
#EXTINF:2,
a2.m4s
#EXT-X-DISCONTINUITY
#EXT-X-MAP:URI=\"b.mp4\"
#EXTINF:2,
b1.m4s
#EXT-X-MAP:URI=\"b.mp4\"
#EXTINF:2,
b2.m4s
#EXT-X-ENDLIST
";
        let Playlist::Media(m) = parse_playlist("https://h/v/p.m3u8", text).expect("parse") else {
            panic!("not media");
        };
        assert_eq!(m.init.map(|i| i.url).as_deref(), Some("https://h/v/a.mp4"));
        // The same section said again is no change.
        let maps: Vec<(usize, &str)> = m.maps.iter().map(|(i, s)| (*i, s.url.as_str())).collect();
        assert_eq!(maps, [(2, "https://h/v/b.mp4")]);
        assert_eq!(m.segments.len(), 4);
    }

    #[test]
    fn media_playlist_with_keys_ranges_and_init() {
        let text = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"100@0\"\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:4,\n#EXT-X-BYTERANGE:1000@100\nall.mp4\n#EXTINF:4,\n#EXT-X-BYTERANGE:500\nall.mp4\n#EXT-X-KEY:METHOD=AES-128,URI=\"k2\",IV=0x0000000000000000000000000000ABCD\n#EXTINF:4,\nlast.ts\n#EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse_playlist("https://h/v/p.m3u8", text).expect("parse") else {
            panic!("not media");
        };
        assert_eq!(m.init.as_ref().map(|i| i.range), Some(Some((0, 100))));
        assert_eq!(m.segments.len(), 3);
        assert_eq!(m.segments[0].range, Some((100, 1100)));
        assert_eq!(
            m.segments[1].range,
            Some((1100, 1600)),
            "continues the previous range"
        );
        assert_eq!(m.segments[0].key.as_ref().map(|k| k.iv[15]), Some(7));
        assert_eq!(m.segments[1].key.as_ref().map(|k| k.iv[15]), Some(8));
        let last = m.segments[2].key.as_ref().expect("key");
        assert_eq!(last.url, "https://h/v/k2");
        assert_eq!(&last.iv[14..], &[0xAB, 0xCD]);
        assert_eq!(m.segments[2].range, None);
    }

    #[test]
    fn refuses_live_and_drm() {
        assert!(
            parse_playlist("u", "#EXTM3U\n#EXTINF:4,\na.ts\n").is_err(),
            "live"
        );
        assert!(
            parse_playlist(
                "u",
                "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\"\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n"
            )
            .is_err()
        );
        assert!(parse_playlist("u", "nope").is_err());
    }
}
