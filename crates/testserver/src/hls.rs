//! A synthetic HLS stream for testing the stream downloader.
//!
//! - `/hls/master.m3u8?<params>` lists one variant per height in `variants` (and an audio
//!   rendition with `audio=1`).
//! - `/hls/v<height>.m3u8` and `/hls/a.m3u8` list `segments` segments of `seg_size` bytes.
//! - `/hls/<track>/<i>.ts` serves segment `i` of a track: deterministic bytes
//!   ([`track_seed`], offset `i * seg_size`), so the joined segments of a track equal the
//!   content [`crate::verify_file`] checks. With `key=1` segments are AES-128-CBC encrypted
//!   (PKCS#7, IV = segment number) with the key at `/hls/key`.
//!
//! All `/hls` requests count in `/_stats` under their path and query.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::Deserialize;

use crate::content;
use crate::scenario::Size;
use crate::server::AppState;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Params {
    pub segments: u32,
    pub seg_size: Size,
    pub seed: u64,
    /// Heights of the variants, e.g. `360,720`.
    pub variants: String,
    /// Encrypt segments with AES-128.
    pub key: u8,
    /// Add an audio rendition (`a`).
    pub audio: u8,
    /// Add a WebVTT subtitle rendition (`subs`), one cue per segment.
    pub subs: u8,
    /// Per-response rate for segments.
    pub rate: Option<Size>,
    /// Segment number that fails with 500 on its first request (retry tests).
    pub fail_once: Option<u32>,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            segments: 10,
            seg_size: Size(256 << 10),
            seed: 1,
            variants: "360,720".into(),
            key: 0,
            audio: 0,
            subs: 0,
            rate: None,
            fail_once: None,
        }
    }
}

/// The content seed of a track (`v720`, `a`): what the joined segments are generated from.
pub fn track_seed(seed: u64, track: &str) -> u64 {
    track.bytes().fold(seed ^ 0x9e37_79b9_7f4a_7c15, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The AES-128 key for `seed`.
pub fn key_bytes(seed: u64) -> [u8; 16] {
    let mut k = [0u8; 16];
    content::fill(seed ^ 0x5ec2_e7ee, 0, &mut k);
    k
}

fn playlist(body: String) -> Response {
    (
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        body,
    )
        .into_response()
}

fn count(app: &AppState, uri: &axum::http::Uri, bytes: u64) {
    let url = app.url(&uri.to_string());
    url.gets.fetch_add(1, Ordering::Relaxed);
    url.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
}

pub(crate) async fn master(
    State(app): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<Params>,
) -> Response {
    let query = uri.query().unwrap_or_default();
    let mut out = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    if p.audio == 1 {
        out.push_str(&format!(
            "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Main\",LANGUAGE=\"en\",DEFAULT=YES,URI=\"a.m3u8?{query}\"\n"
        ));
    }
    if p.subs == 1 {
        out.push_str(&format!(
            "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"Türkçe\",LANGUAGE=\"tr\",URI=\"subs.m3u8?{query}\"\n"
        ));
    }
    for h in p
        .variants
        .split(',')
        .filter_map(|h| h.trim().parse::<u32>().ok())
    {
        let mut audio = if p.audio == 1 { ",AUDIO=\"aud\"" } else { "" }.to_owned();
        if p.subs == 1 {
            audio.push_str(",SUBTITLES=\"subs\"");
        }
        out.push_str(&format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={},RESOLUTION={}x{h}{audio}\nv{h}.m3u8?{query}\n",
            h * 5000,
            h * 16 / 9
        ));
    }
    count(&app, &uri, out.len() as u64);
    playlist(out)
}

pub(crate) async fn media_playlist(
    State(app): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Path(name): Path<String>,
    Query(p): Query<Params>,
) -> Response {
    let Some(track) = name.strip_suffix(".m3u8") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let query = uri.query().unwrap_or_default();
    let mut out = String::from(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n",
    );
    if p.key == 1 {
        out.push_str(&format!(
            "#EXT-X-KEY:METHOD=AES-128,URI=\"key?seed={}\"\n",
            p.seed
        ));
    }
    let ext = if track == "subs" { "vtt" } else { "ts" };
    for i in 0..p.segments {
        out.push_str(&format!("#EXTINF:4.000,\n{track}/{i}.{ext}?{query}\n"));
    }
    out.push_str("#EXT-X-ENDLIST\n");
    count(&app, &uri, out.len() as u64);
    playlist(out)
}

#[derive(Deserialize)]
pub(crate) struct KeyQuery {
    seed: u64,
}

pub(crate) async fn key(
    State(app): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Query(q): Query<KeyQuery>,
) -> Response {
    count(&app, &uri, 16);
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        key_bytes(q.seed).to_vec(),
    )
        .into_response()
}

pub(crate) async fn segment(
    State(app): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Path((track, file)): Path<(String, String)>,
    Query(p): Query<Params>,
) -> Response {
    if let Some(i) = file
        .strip_suffix(".vtt")
        .and_then(|n| n.parse::<u32>().ok())
    {
        return ([(header::CONTENT_TYPE, "text/vtt")], subtitle_segment(i)).into_response();
    }
    let Some(i) = file.strip_suffix(".ts").and_then(|n| n.parse::<u32>().ok()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if i >= p.segments {
        return StatusCode::NOT_FOUND.into_response();
    }
    let state = app.url(&uri.to_string());
    if p.fail_once == Some(i) && state.gets.load(Ordering::Relaxed) == 0 {
        state.gets.fetch_add(1, Ordering::Relaxed);
        return (StatusCode::INTERNAL_SERVER_ERROR, "try again\n").into_response();
    }
    let size = p.seg_size.0 as usize;
    let mut data = vec![0u8; size];
    content::fill(
        track_seed(p.seed, &track),
        u64::from(i) * p.seg_size.0,
        &mut data,
    );
    if p.key == 1 {
        use aes::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};
        let iv = u128::from(i).to_be_bytes();
        data = cbc::Encryptor::<aes::Aes128>::new(&key_bytes(p.seed).into(), &iv.into())
            .encrypt_padded_vec::<Pkcs7>(&data);
    }
    state.gets.fetch_add(1, Ordering::Relaxed);
    let len = data.len();
    let body = match p.rate {
        None => {
            state.bytes_sent.fetch_add(len as u64, Ordering::Relaxed);
            Body::from(data)
        }
        Some(rate) => {
            // About twenty writes a second, each counted as it goes.
            let chunk = ((rate.0 / 20).max(1024)) as usize;
            let stream = futures_util::stream::unfold((data, 0usize), move |(data, at)| {
                let state = Arc::clone(&state);
                async move {
                    if at >= data.len() {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let end = (at + chunk).min(data.len());
                    let piece = Bytes::copy_from_slice(&data[at..end]);
                    state
                        .bytes_sent
                        .fetch_add(piece.len() as u64, Ordering::Relaxed);
                    Some((Ok::<_, std::io::Error>(piece), (data, end)))
                }
            });
            Body::from_stream(stream)
        }
    };
    (
        [
            (header::CONTENT_TYPE, "video/mp2t".to_owned()),
            (header::CONTENT_LENGTH, len.to_string()),
        ],
        body,
    )
        .into_response()
}

/// WebVTT segment `i`: one cue, `Altyazı i`, at seconds `4i` to `4i + 2`.
pub fn subtitle_segment(i: u32) -> String {
    let t = |s: u32| format!("00:{:02}:{:02}.000", s / 60, s % 60);
    format!(
        "WEBVTT
X-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000

{} --> {}
Altyazı {i}
",
        t(i * 4),
        t(i * 4 + 2)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_differ_per_track() {
        assert_ne!(track_seed(1, "v360"), track_seed(1, "v720"));
        assert_ne!(track_seed(1, "a"), track_seed(2, "a"));
        assert_eq!(track_seed(5, "a"), track_seed(5, "a"));
        assert_ne!(key_bytes(1), key_bytes(2));
    }
}
