//! A synthetic DASH stream for testing the stream downloader.
//!
//! `/dash/manifest.mpd?mode=<number|timeline|list|base>&segments=N&seg_size=S&init_size=I`
//! lists representations `v360`, `v720` (and `a` with `audio=1`). Whatever the addressing
//! mode, a representation's init segment followed by its media segments is the
//! deterministic content of [`crate::hls::track_seed`]`(seed, rep)` and length
//! `init_size + segments * seg_size`, which [`crate::verify_file`] checks. In `base` mode the
//! representation is that whole file at `/file/…`.
//!
//! With `live=1` (`number` or `timeline` mode) the manifest is a live stream's (`dynamic`): a
//! segment of `seg_ms` comes out every `seg_ms` from the first request for the manifest (per
//! `seed`), the newest `window` are listed (`timeline`) or kept (`timeShiftBufferDepth`), and
//! with `end_after=N` the manifest turns `static` once N are out. Segment `n`'s content is
//! still at `init_size + (n - 1) * seg_size`, so a recording from the first segment on is
//! the same deterministic content.

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
    /// Add a Turkish WebVTT text representation (`s`) as one file.
    pub subs: u8,
    /// A live stream (see the module docs).
    pub live: u8,
    pub seg_ms: u64,
    pub window: u64,
    pub end_after: Option<u64>,
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
            subs: 0,
            live: 0,
            seg_ms: 1000,
            window: 6,
            end_after: None,
        }
    }
}

impl Params {
    /// The length of a representation's whole content.
    pub fn total(&self) -> u64 {
        self.init_size.0 + self.segments * self.seg_size.0
    }
}

/// A live stream's clock: when it started (wall clock, Unix ms) and how many segments are
/// out by now (one starts coming out as soon as its time begins, like the live HLS).
fn live_clock(app: &AppState, p: &Params) -> (u64, u64) {
    let started = *app
        .url(&format!("live-clock:dash:{}", p.seed))
        .first_seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(std::time::Instant::now);
    let elapsed = started.elapsed().as_millis() as u64;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let out = elapsed / p.seg_ms.max(1) + 1;
    (now - elapsed, p.end_after.map_or(out, |e| out.min(e)))
}

/// Unix milliseconds as `2026-10-31T12:00:00.123Z`.
fn iso_time(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60,
        ms % 1000
    )
}

pub(crate) async fn manifest(
    State(app): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<Params>,
) -> Response {
    let q = uri.query().unwrap_or_default().replace('&', "&amp;");
    // A live stream: where it is, and whether it is over.
    let live = (p.live == 1).then(|| live_clock(&app, &p));
    let ended = live.is_some_and(|(_, out)| p.end_after.is_some_and(|e| out >= e));
    let n = match live {
        Some((_, out)) => out,
        None => p.segments,
    };
    let reps = |kind: &str, ids: &[(&str, u32)]| -> String {
        let mut out = format!("<AdaptationSet contentType=\"{kind}\" mimeType=\"{kind}/mp4\">");
        match p.mode.as_str() {
            "number" if live.is_some() => out.push_str(&format!(
                "<SegmentTemplate timescale=\"1000\" duration=\"{}\" startNumber=\"1\" initialization=\"$RepresentationID$/init.mp4?{q}\" media=\"$RepresentationID$/$Number$.m4s?{q}\"/>",
                p.seg_ms
            )),
            "timeline" if live.is_some() => {
                let first = if ended { 0 } else { n.saturating_sub(p.window.max(1)) };
                out.push_str(&format!(
                    "<SegmentTemplate timescale=\"1000\" startNumber=\"{}\" initialization=\"$RepresentationID$/init.mp4?{q}\" media=\"$RepresentationID$/t$Time$.m4s?{q}\"><SegmentTimeline><S t=\"{}\" d=\"{}\" r=\"{}\"/></SegmentTimeline></SegmentTemplate>",
                    first + 1,
                    first * p.seg_ms,
                    p.seg_ms,
                    n - first - 1
                ));
            }
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
    let mut body = match live {
        Some((start, _)) if !ended => format!(
            "<?xml version=\"1.0\"?><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"dynamic\" availabilityStartTime=\"{}\" minimumUpdatePeriod=\"PT{:.3}S\" timeShiftBufferDepth=\"PT{:.3}S\"><Period start=\"PT0S\">",
            iso_time(start),
            p.seg_ms as f64 / 1000.0,
            (p.window.max(1) * p.seg_ms) as f64 / 1000.0
        ),
        Some(_) => format!(
            "<?xml version=\"1.0\"?><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" mediaPresentationDuration=\"PT{:.3}S\"><Period>",
            (n * p.seg_ms) as f64 / 1000.0
        ),
        None => format!(
            "<?xml version=\"1.0\"?><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" mediaPresentationDuration=\"PT{}S\"><Period>",
            n * SEGMENT_SECONDS
        ),
    };
    body.push_str(&reps("video", &[("v360", 360), ("v720", 720)]));
    if p.audio == 1 {
        body.push_str(&reps("audio", &[("a", 0)]));
    }
    if p.subs == 1 {
        body.push_str(&format!(
            "<AdaptationSet contentType=\"text\" mimeType=\"text/vtt\" lang=\"tr\"><Representation id=\"s\" bandwidth=\"256\"><BaseURL>s/whole.vtt?{q}</BaseURL></Representation></AdaptationSet>",
        ));
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
    if file == "whole.vtt" {
        let cues: String = (0..p.segments as u32)
            .map(|i| {
                crate::hls::subtitle_segment(i)
                    .split_once(
                        "

",
                    )
                    .map(|(_, cue)| {
                        format!(
                            "{cue}
"
                        )
                    })
                    .unwrap_or_default()
            })
            .collect();
        return (
            [(header::CONTENT_TYPE, "text/vtt")],
            format!(
                "WEBVTT

{cues}"
            ),
        )
            .into_response();
    }
    let (offset, len) = if file == "init.mp4" {
        (0, p.init_size.0)
    } else {
        let stem = file.strip_suffix(".m4s").unwrap_or("");
        let per_segment = match p.live {
            1 => p.seg_ms.max(1),
            _ => SEGMENT_SECONDS * 1000,
        };
        let number = match stem.strip_prefix('t') {
            Some(t) => t.parse::<u64>().ok().map(|t| t / per_segment + 1),
            None => stem.parse::<u64>().ok(),
        };
        // A live stream's segments exist once they are out.
        let last = match p.live {
            1 => live_clock(&app, &p).1,
            _ => p.segments,
        };
        match number {
            Some(n) if (1..=last).contains(&n) => {
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

#[cfg(test)]
mod tests {
    #[test]
    fn iso_time() {
        assert_eq!(super::iso_time(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            super::iso_time(1_793_448_000_123),
            "2026-10-31T12:00:00.123Z"
        );
        assert_eq!(super::iso_time(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }
}
