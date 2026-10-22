//! Downloading HLS streams from the test server's synthetic stream.

mod common;

use std::time::Duration;

use shardload_engine::{CancellationToken, Error, Options, StreamRequest, download_hls};
use shardload_testserver::hls::track_seed;
use shardload_testserver::{Verdict, verify_file};

use common::{LIMIT, bytes_sent, start, workdir};

const SEG: u64 = 256 << 10;

fn request(url: String, out: &std::path::Path) -> StreamRequest {
    StreamRequest {
        video: url,
        audio: None,
        output: out.to_owned(),
        headers: Vec::new(),
        ffmpeg: None,
        clip: None,
    }
}

fn opts(connections: usize) -> Options {
    Options {
        connections,
        ..Options::default()
    }
}

async fn get(
    req: &StreamRequest,
    opts: &Options,
) -> shardload_engine::Result<shardload_engine::Outcome> {
    tokio::time::timeout(
        LIMIT,
        download_hls(req, opts, None, &CancellationToken::new()),
    )
    .await
    .expect("download hung")
}

#[tokio::test(flavor = "multi_thread")]
async fn takes_the_best_variant_and_joins_its_segments() {
    let base = start().await;
    let dir = workdir("hls-plain");
    let out = dir.join("show.ts");
    let url = format!("{base}/hls/master.m3u8?segments=10&seed=3&variants=360,720");
    let done = get(&request(url, &out), &opts(4)).await.expect("download");
    assert_eq!(done.size, 10 * SEG);
    assert_eq!(
        verify_file(&out, track_seed(3, "v720"), 10 * SEG).expect("verify"),
        Verdict::Ok
    );
    assert!(
        !dir.join("show.ts.shardload-hls").exists(),
        "segments are cleaned up"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn decrypts_aes_128() {
    let base = start().await;
    let dir = workdir("hls-aes");
    let out = dir.join("secret.ts");
    let url = format!("{base}/hls/v360.m3u8?segments=6&seed=4&key=1");
    get(&request(url, &out), &opts(3)).await.expect("download");
    assert_eq!(
        verify_file(&out, track_seed(4, "v360"), 6 * SEG).expect("verify"),
        Verdict::Ok
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_a_segment_that_fails_once() {
    let base = start().await;
    let dir = workdir("hls-retry");
    let out = dir.join("r.ts");
    let url = format!("{base}/hls/v360.m3u8?segments=5&seed=5&fail_once=2");
    get(&request(url, &out), &opts(2)).await.expect("download");
    assert_eq!(
        verify_file(&out, track_seed(5, "v360"), 5 * SEG).expect("verify"),
        Verdict::Ok
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn continues_with_the_missing_segments() {
    let base = start().await;
    let dir = workdir("hls-resume");
    let out = dir.join("long.ts");
    // 20 segments, each taking a second at 256 KiB/s, four at a time: about 5 s.
    let url = format!("{base}/hls/v360.m3u8?segments=20&seed=6&rate=256K");
    let req = request(url, &out);
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2600)).await;
        stop.cancel();
    });
    let first = download_hls(&req, &opts(4), None, &cancel).await;
    assert!(matches!(first, Err(Error::Cancelled)), "{first:?}");
    assert!(
        dir.join("long.ts.shardload-hls").exists(),
        "finished segments are kept"
    );
    let before = bytes_sent(&base).await;

    let done = get(&req, &opts(4)).await.expect("resume");
    assert!(
        done.resumed_from >= 4 * SEG,
        "resumed from {}",
        done.resumed_from
    );
    assert_eq!(
        verify_file(&out, track_seed(6, "v360"), 20 * SEG).expect("verify"),
        Verdict::Ok
    );
    // Nothing already on disk is fetched again: only the missing segments and the playlist.
    let again = bytes_sent(&base).await - before;
    assert!(
        again <= 20 * SEG - done.resumed_from + (16 << 10),
        "fetched {again} bytes, {} were on disk",
        done.resumed_from
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn separate_audio_needs_ffmpeg() {
    let base = start().await;
    let dir = workdir("hls-audio");
    let out = dir.join("a.ts");
    let url = format!("{base}/hls/master.m3u8?segments=3&audio=1");
    match get(&request(url, &out), &opts(2)).await {
        Err(Error::State(m)) => assert!(m.contains("ffmpeg"), "{m}"),
        other => panic!("expected a clear error, got {other:?}"),
    }
}

/// Whether ffmpeg (and ffprobe) can be run; the real-media tests skip without (CI).
fn have_ffmpeg() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok()
}

/// `seconds` of test picture and tone made by ffmpeg into an HLS stream in `src`: video and
/// audio as separate renditions, a segment (and a keyframe) every 2 s.
fn make_stream(src: &std::path::Path, seconds: u32) {
    std::fs::create_dir_all(src).expect("dir");
    let made = std::process::Command::new("ffmpeg")
        .current_dir(src)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!("testsrc=duration={seconds}:size=320x180:rate=25"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={seconds}"))
        .args([
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "50",
            "-c:a",
            "aac",
            "-f",
            "hls",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-var_stream_map",
            "v:0,agroup:aud a:0,agroup:aud,default:yes",
            "-master_pl_name",
            "master.m3u8",
            "-hls_segment_filename",
            "s%v_%03d.ts",
            "p%v.m3u8",
        ])
        .status()
        .expect("run ffmpeg");
    assert!(made.success(), "ffmpeg could not make the stream");
}

/// Serves the folder; returns its address and the names of the segments asked for.
async fn serve_folder(
    root: std::path::PathBuf,
) -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = asked.clone();
    let app = axum::Router::new().route(
        "/{file}",
        axum::routing::get(
            move |axum::extract::Path(file): axum::extract::Path<String>| {
                let root = root.clone();
                let log = log.clone();
                async move {
                    if file.ends_with(".ts") {
                        log.lock().expect("log").push(file.clone());
                    }
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
    (addr, asked)
}

/// The streams' kinds and the length of a media file, from ffprobe.
fn probe(file: &std::path::Path) -> (String, f64) {
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type:format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .expect("ffprobe");
    let report = String::from_utf8_lossy(&probe.stdout).into_owned();
    let seconds = report
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .next()
        .expect("duration");
    (report, seconds)
}

/// Real media: a short clip made by ffmpeg, cut into an HLS stream with separate audio, then
/// downloaded and put back together. Skipped where ffmpeg is not installed (CI).
#[tokio::test(flavor = "multi_thread")]
async fn joins_real_video_and_audio_with_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found: skipped");
        return;
    }
    let dir = workdir("hls-ffmpeg");
    let src = dir.join("src");
    make_stream(&src, 8);
    let (addr, _) = serve_folder(src).await;

    let out = dir.join("clip.mp4");
    let req = StreamRequest {
        ffmpeg: Some("ffmpeg".into()),
        ..request(format!("http://{addr}/master.m3u8"), &out)
    };
    get(&req, &opts(4)).await.expect("download");
    let (report, seconds) = probe(&out);
    assert!(
        report.contains("video") && report.contains("audio"),
        "{report}"
    );
    assert!((7.5..=8.6).contains(&seconds), "{seconds} s");
}

/// A clip of a real stream: only the segments it falls in are fetched, and the file is cut
/// to it, by copying or, exactly, by re-encoding.
#[tokio::test(flavor = "multi_thread")]
async fn a_clip_fetches_only_its_segments_and_is_cut() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found: skipped");
        return;
    }
    let dir = workdir("hls-clip");
    let src = dir.join("src");
    make_stream(&src, 30);
    let (addr, asked) = serve_folder(src).await;

    for (start, end, exact, segments) in [(10.0, 16.0, false, 3), (11.5, 14.5, true, 2)] {
        asked.lock().expect("log").clear();
        let out = dir.join(&format!("clip-{start}.mp4"));
        let req = StreamRequest {
            ffmpeg: Some("ffmpeg".into()),
            clip: Some(shardload_engine::Clip { start, end, exact }),
            ..request(format!("http://{addr}/master.m3u8"), &out)
        };
        get(&req, &opts(4)).await.expect("download");
        let asked = asked.lock().expect("log").clone();
        // Video and audio, about `segments` of each of the 15: the audio's segments do not
        // end exactly where the video's do, so one more of them may touch the clip.
        assert!(
            (2 * segments..=2 * segments + 2).contains(&asked.len()),
            "{asked:?}"
        );
        let (report, seconds) = probe(&out);
        assert!(
            report.contains("video") && report.contains("audio"),
            "{report}"
        );
        let want = end - start;
        assert!(
            (seconds - want).abs() < 0.3,
            "{start}-{end}: {seconds} s, wanted {want}"
        );
    }

    // Without ffmpeg nothing can be cut.
    let req = StreamRequest {
        clip: Some(shardload_engine::Clip {
            start: 1.0,
            end: 2.0,
            exact: false,
        }),
        ..request(format!("http://{addr}/p0.m3u8"), &dir.join("no.mp4"))
    };
    match get(&req, &opts(4)).await {
        Err(Error::State(m)) => assert!(m.contains("ffmpeg"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn joins_hls_subtitle_segments() {
    let base = start().await;
    let dir = workdir("hls-subs");
    let out = dir.join("film.tr.vtt");
    let source = shardload_engine::SubtitleSource::Hls {
        playlist: format!("{base}/hls/subs.m3u8?segments=5&subs=1"),
    };
    shardload_engine::download_subtitles(&source, &out, &[], &Options::default())
        .await
        .expect("subtitles");
    let text = std::fs::read_to_string(&out).expect("read");
    assert_eq!(text.matches("WEBVTT").count(), 1, "{text}");
    assert_eq!(text.matches(" --> ").count(), 5);
    assert!(text.find("Altyazı 0").expect("0") < text.find("Altyazı 4").expect("4"));
    assert!(text.contains("00:00:16.000 --> 00:00:18.000"));
}
