//! Downloading DASH streams from the test server's synthetic manifests, and a real one made
//! by ffmpeg.

mod common;

use shardload_engine::{CancellationToken, DashRequest, Options, download_dash};
use shardload_testserver::hls::track_seed;
use shardload_testserver::{Verdict, verify_file};

use common::{LIMIT, start, workdir};

const SEG: u64 = 128 << 10;
const INIT: u64 = 4096;

fn request(manifest: String, video: &str, out: &std::path::Path) -> DashRequest {
    DashRequest {
        manifest,
        video: video.into(),
        audio: None,
        output: out.to_owned(),
        headers: Vec::new(),
        ffmpeg: None,
        clip: None,
    }
}

async fn get(req: &DashRequest) -> shardload_engine::Result<shardload_engine::Outcome> {
    let opts = Options {
        connections: 4,
        ..Options::default()
    };
    tokio::time::timeout(
        LIMIT,
        download_dash(req, &opts, None, &CancellationToken::new()),
    )
    .await
    .expect("download hung")
}

#[tokio::test(flavor = "multi_thread")]
async fn every_addressing_mode_gives_the_whole_representation() {
    let base = start().await;
    let dir = workdir("dash-modes");
    for (mode, rep) in [
        ("number", "v720"),
        ("timeline", "v360"),
        ("list", "v720"),
        ("base", "v360"),
    ] {
        let out = dir.join(&format!("{mode}.mp4"));
        let url = format!("{base}/dash/manifest.mpd?mode={mode}&segments=6&seed=7");
        get(&request(url, rep, &out))
            .await
            .unwrap_or_else(|e| panic!("{mode}: {e}"));
        assert_eq!(
            verify_file(&out, track_seed(7, rep), INIT + 6 * SEG).expect("verify"),
            Verdict::Ok,
            "{mode}"
        );
        assert!(
            !dir.join(&format!("{mode}.mp4.shardload-hls")).exists(),
            "{mode}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn separate_audio_needs_ffmpeg_and_unknown_ids_fail() {
    let base = start().await;
    let dir = workdir("dash-errors");
    let url = format!("{base}/dash/manifest.mpd?audio=1");
    let mut req = request(url.clone(), "v360", &dir.join("a.mp4"));
    req.audio = Some("a".into());
    assert!(
        matches!(get(&req).await, Err(shardload_engine::Error::State(m)) if m.contains("ffmpeg"))
    );
    let wrong = request(url, "v1080", &dir.join("b.mp4"));
    assert!(get(&wrong).await.is_err());
}

/// Real media: ffmpeg cuts a clip into DASH (SegmentTemplate with a timeline, separate video
/// and audio), which is downloaded and joined back. Skipped without ffmpeg.
#[tokio::test(flavor = "multi_thread")]
async fn joins_real_dash_video_and_audio() {
    use std::process::Command;
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg not found: skipped");
        return;
    }
    let dir = workdir("dash-ffmpeg");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).expect("dir");
    let made = Command::new("ffmpeg")
        .current_dir(&src)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=8:size=320x180:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=550:duration=8",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            "-f",
            "dash",
            "-seg_duration",
            "2",
            "-use_timeline",
            "1",
            "-use_template",
            "1",
            "-adaptation_sets",
            "id=0,streams=v id=1,streams=a",
            "manifest.mpd",
        ])
        .status()
        .expect("run ffmpeg");
    assert!(made.success(), "ffmpeg could not make the clip");
    let manifest = std::fs::read_to_string(src.join("manifest.mpd")).expect("mpd");
    let streams = shardload_media::parse_mpd("http://x/manifest.mpd", &manifest).expect("parse");
    let (video, audio) = (
        streams.video[0].id.clone(),
        streams.audio[0].id.clone().expect("audio id"),
    );

    let root = src.clone();
    let app = axum::Router::new().route(
        "/{file}",
        axum::routing::get(
            move |axum::extract::Path(file): axum::extract::Path<String>| {
                let root = root.clone();
                async move {
                    std::fs::read(root.join(file)).map_err(|_| axum::http::StatusCode::NOT_FOUND)
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await });

    let out = dir.join("clip.mp4");
    let req = DashRequest {
        audio: Some(audio),
        ffmpeg: Some("ffmpeg".into()),
        ..request(format!("http://{addr}/manifest.mpd"), &video, &out)
    };
    get(&req).await.expect("download");
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type:format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(&out)
        .output()
        .expect("ffprobe");
    let report = String::from_utf8_lossy(&probe.stdout);
    assert!(
        report.contains("video") && report.contains("audio"),
        "{report}"
    );
    let seconds: f64 = report
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .next()
        .expect("duration");
    assert!((7.5..=8.6).contains(&seconds), "{seconds} s");
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_a_dash_text_representation() {
    let base = start().await;
    let dir = workdir("dash-subs");
    let out = dir.join("film.tr.vtt");
    let source = shardload_engine::SubtitleSource::Dash {
        manifest: format!("{base}/dash/manifest.mpd?segments=3&subs=1"),
        representation: "s".into(),
    };
    shardload_engine::download_subtitles(&source, &out, &[], &Options::default())
        .await
        .expect("subtitles");
    let text = std::fs::read_to_string(&out).expect("read");
    assert!(text.starts_with("WEBVTT"));
    assert_eq!(text.matches(" --> ").count(), 3);
    // The manifest lists it with its language.
    let (mpd, _) = shardload_engine::fetch_text(
        &format!("{base}/dash/manifest.mpd?segments=3&subs=1"),
        &[],
        &Options::default(),
        1 << 20,
    )
    .await
    .expect("mpd");
    let streams = shardload_media::parse_mpd("u", &mpd).expect("parse");
    assert_eq!(streams.subtitles[0].language.as_deref(), Some("tr"));
}
