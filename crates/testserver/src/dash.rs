//! A synthetic DASH stream for testing the stream downloader.
//!
//! `/dash/manifest.mpd?mode=<number|timeline|list|base>&segments=N&seg_size=S&init_size=I`
//! lists representations `v360`, `v720` (and `a` with `audio=1`). Whatever the addressing
//! mode, a representation's init segment followed by its media segments is the
//! deterministic content of [`crate::hls::track_seed`]`(seed, rep)` and length
//! `init_size + segments * seg_size`, which [`crate::verify_file`] checks. In `base` mode the
//! representation is that whole file at `/file/…`.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::content;
use crate::hls::track_seed;
use crate::scenario::Size;
use crate::server::AppState;

/// Seconds per segment in the manifest.
const SEGMENT_SECONDS: u64 = 4;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Params {
    pub mode: String,
    pub segments: u64,
    pub seg_size: Size,
    pub init_size: Size,
    pub seed: u64,
    pub audio: u8,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            mode: "number".into(),
            segments: 5,
            seg_size: Size(128 << 10),
            init_size: Size(4096),
            seed: 1,
            audio: 0,
        }
    }
}

impl Params {
    /// The length of a representation's whole content.
    pub fn total(&self) -> u64 {
        self.init_size.0 + self.segments * self.seg_size.0
    }
}

pub(crate) async fn manifest(
    State(app): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<Params>,
) -> Response {
    let q = uri.query().unwrap_or_default().replace('&', "&amp;");
    let n = p.segments;
    let reps = |kind: &str, ids: &[(&str, u32)]| -> String {
        let mut out = format!("<AdaptationSet contentType=\"{kind}\" mimeType=\"{kind}/mp4\">");
        match p.mode.as_str() {
            "number" => out.push_str(&format!(
                "<SegmentTemplate timescale=\"1\" duration=\"{SEGMENT_SECONDS}\" startNumber=\"1\" initialization=\"$RepresentationID$/init.mp4?{q}\" media=\"$RepresentationID$/$Number$.m4s?{q}\"/>"
            )),
            "timeline" => out.push_str(&format!(
                "<SegmentTemplate timescale=\"1000\" initialization=\"$RepresentationID$/init.mp4?{q}\" media=\"$RepresentationID$/t$Time$.m4s?{q}\"><SegmentTimeline><S t=\"0\" d=\"{}\" r=\"{}\"/></SegmentTimeline></SegmentTemplate>",
                SEGMENT_SECONDS * 1000,
                n - 1
            )),
            _ => {}
        }
        for (id, height) in ids {
            out.push_str(&format!(
                "<Representation id=\"{id}\" bandwidth=\"{}\"{}>",
                height.max(&1) * 4000,
                if kind == "video" {
                    format!(" width=\"{}\" height=\"{height}\"", height * 16 / 9)
                } else {
                    String::new()
                }
            ));
            match p.mode.as_str() {
                "list" => {
                    out.push_str(&format!(
                        "<SegmentList><Initialization sourceURL=\"{id}/init.mp4?{q}\"/>"
                    ));
                    for i in 1..=n {
                        out.push_str(&format!("<SegmentURL media=\"{id}/{i}.m4s?{q}\"/>"));
                    }
                    out.push_str("</SegmentList>");
                }
                "base" => out.push_str(&format!(
                    "<BaseURL>/file/{id}.mp4?size={}&amp;seed={}</BaseURL><SegmentBase indexRange=\"0-100\"/>",
                    p.total(),
                    track_seed(p.seed, id)
                )),
                _ => {}
            }
            out.push_str("</Representation>");
        }
        out.push_str("</AdaptationSet>");
        out
    };
    let mut body = format!(
        "<?xml version=\"1.0\"?><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" mediaPresentationDuration=\"PT{}S\"><Period>",
        n * SEGMENT_SECONDS
    );
    body.push_str(&reps("video", &[("v360", 360), ("v720", 720)]));
    if p.audio == 1 {
        body.push_str(&reps("audio", &[("a", 0)]));
    }
    body.push_str("</Period></MPD>");
    let url = app.url(&uri.to_string());
    url.gets.fetch_add(1, Ordering::Relaxed);
    url.bytes_sent
        .fetch_add(body.len() as u64, Ordering::Relaxed);
    ([(header::CONTENT_TYPE, "application/dash+xml")], body).into_response()
}

pub(crate) async fn segment(
    State(app): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Path((rep, file)): Path<(String, String)>,
    Query(p): Query<Params>,
) -> Response {
    let (offset, len) = if file == "init.mp4" {
        (0, p.init_size.0)
    } else {
        let stem = file.strip_suffix(".m4s").unwrap_or("");
        let number = match stem.strip_prefix('t') {
            Some(t) => t
                .parse::<u64>()
                .ok()
                .map(|t| t / (SEGMENT_SECONDS * 1000) + 1),
            None => stem.parse::<u64>().ok(),
        };
        match number {
            Some(n) if (1..=p.segments).contains(&n) => {
                (p.init_size.0 + (n - 1) * p.seg_size.0, p.seg_size.0)
            }
            _ => return StatusCode::NOT_FOUND.into_response(),
        }
    };
    let mut data = vec![0u8; len as usize];
    content::fill(track_seed(p.seed, &rep), offset, &mut data);
    let url = app.url(&uri.to_string());
    url.gets.fetch_add(1, Ordering::Relaxed);
    url.bytes_sent.fetch_add(len, Ordering::Relaxed);
    ([(header::CONTENT_TYPE, "video/mp4")], data).into_response()
}
