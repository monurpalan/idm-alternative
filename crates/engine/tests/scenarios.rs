//! The engine against every hostile server behaviour the test server can produce.
//!
//! Each test starts its own server in-process, downloads into its own directory, and checks
//! the result byte-for-byte against the generated content.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::*;
use shardload_engine::{CancellationToken, Error, Options, Outcome, Progress, download};

/// Downloads `url`, also saying how many connections failed and were opened again on the way.
async fn counting_reconnects(url: String, out: &Path, o: &Options) -> (Outcome, u64) {
    let (tx, mut rx) = tokio::sync::watch::channel(Progress::default());
    let most = tokio::spawn(async move {
        let mut most = 0;
        while rx.changed().await.is_ok() {
            most = most.max(rx.borrow().health.reconnects);
        }
        most
    });
    let req = request(url, out);
    let token = CancellationToken::new();
    let done = tokio::time::timeout(LIMIT, download(&req, o, Some(&tx), &token))
        .await
        .expect("download hung")
        .expect("download");
    drop(tx);
    (done, most.await.expect("progress watcher"))
}

/// Downloads `query` (appended to the URL) and checks the file.
async fn ok(name: &str, size: u64, query: &str, connections: usize) -> Outcome {
    let base = start().await;
    let dir = workdir(name);
    let out = dir.join("f.bin");
    let url = format!("{base}/file/f.bin?size={size}&seed=7{query}");
    let done = get(&request(url, &out), &opts(connections))
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(done.size, size);
    assert_verified(&out, 7, size);
    done
}

#[tokio::test(flavor = "multi_thread")]
async fn clean() {
    ok("clean", 64 * MB, "", 16).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tiny_and_empty_files() {
    ok("one-byte", 1, "", 16).await;
    ok("below-min-piece", 100 << 10, "", 16).await;
    ok("empty", 0, "", 16).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_ask_for_no_more_than_the_largest_allowed() {
    // A server that serves large requests slowly (YouTube): each connection's range comes
    // in pieces of 1 MiB at most, one after another.
    let base = start().await;
    let dir = workdir("max-request");
    let out = dir.join("f.bin");
    let size = 12 * MB;
    let url = format!("{base}/file/f.bin?size={size}&seed=7");
    let o = Options {
        max_request: Some(MB),
        ..opts(4)
    };
    get(&request(url, &out), &o).await.expect("download");
    assert_verified(&out, 7, size);
    let ranges = ranges_asked(&base).await;
    assert!(ranges.len() >= 12, "{ranges:?}");
    for r in &ranges {
        let (start, end) = r
            .strip_prefix("bytes=")
            .and_then(|r| r.split_once('-'))
            .expect("a range");
        if end.is_empty() {
            assert_eq!(
                start, "0",
                "only the first request is open: it learns the size"
            );
            continue;
        }
        let len = end.parse::<u64>().expect("end") - start.parse::<u64>().expect("start") + 1;
        assert!(len <= MB, "{r}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn per_connection_limit_uses_parallelism() {
    // 8 connections at 2 MiB/s: about 1 s; one connection would need 8 s.
    let done = ok("per-conn", 16 * MB, "&rate=2M", 8).await;
    assert!(done.elapsed < Duration::from_secs(5), "{:?}", done.elapsed);
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_connection_does_not_hold_the_tail() {
    // The third connection crawls at 32 KiB/s; it must not keep a large share of the file.
    let done = ok("slow-tail", 32 * MB, "&rate=4M&slow_conn=3:32K", 8).await;
    assert!(done.elapsed < Duration::from_secs(6), "{:?}", done.elapsed);
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_cap_is_respected() {
    ok("conn-cap", 16 * MB, "&rate=4M&max_conns=3", 16).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn forbidden_extra_connections_are_dropped() {
    // Connections beyond 3 get 403 rather than 503: still a limit, not an expired link.
    ok(
        "conn-ban",
        16 * MB,
        "&rate=4M&max_conns=3&refuse_status=403",
        16,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dropped_connections_are_resumed() {
    ok("flaky", 16 * MB, "&rate=16M&drop_after=1M", 8).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stalled_connection_is_replaced() {
    let base = start().await;
    let dir = workdir("stall");
    let out = dir.join("f.bin");
    let url = format!("{base}/file/f.bin?size={}&seed=7&stall_after=2M", 8 * MB);
    let o = Options {
        read_timeout: Duration::from_millis(500),
        ..opts(4)
    };
    get(&request(url, &out), &o).await.expect("download");
    assert_verified(&out, 7, 8 * MB);
}

/// The first request is never answered: the download fails as stalled once the wait for an
/// answer (three read timeouts) is over, to be tried again later, instead of waiting forever.
#[tokio::test(flavor = "multi_thread")]
async fn unanswered_first_request_stalls() {
    let base = start().await;
    let dir = workdir("hang-probe");
    let out = dir.join("f.bin");
    let url = format!("{base}/file/f.bin?size={}&seed=7&hang_get=1", 4 * MB);
    let o = Options {
        read_timeout: Duration::from_millis(500),
        ..opts(4)
    };
    let (req, token) = (request(url, &out), CancellationToken::new());
    let started = Instant::now();
    let run = download(&req, &o, None, &token);
    let err = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("waited for an answer that never comes")
        .expect_err("must fail");
    assert!(matches!(err, Error::Stalled(_)), "{err}");
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(1500), "{waited:?}");
}

/// A server that takes longer than the read timeout to answer each request, but less than
/// the wait for an answer, is waited for: no request fails, the first or a later one.
#[tokio::test(flavor = "multi_thread")]
async fn late_answers_are_waited_for() {
    let base = start().await;
    let dir = workdir("late-answer");
    let out = dir.join("f.bin");
    let size = 2 * MB;
    let url = format!("{base}/file/f.bin?size={size}&seed=7&latency_ms=2000");
    let o = Options {
        read_timeout: Duration::from_secs(1),
        max_request: Some(MB / 2),
        ..opts(2)
    };
    let (_, reconnects) = counting_reconnects(url, &out, &o).await;
    assert_eq!(reconnects, 0);
    assert!(
        ranges_asked(&base).await.len() >= 3,
        "requests after the first"
    );
    assert_verified(&out, 7, size);
}

/// A request in the middle of the download is never answered: that connection gives up
/// after the read timeout and its range is downloaded again.
#[tokio::test(flavor = "multi_thread")]
async fn unanswered_range_request_is_retried() {
    let base = start().await;
    let dir = workdir("hang-range");
    let out = dir.join("f.bin");
    let size = 8 * MB;
    let url = format!("{base}/file/f.bin?size={size}&seed=7&rate=4M&hang_get=2");
    let o = Options {
        read_timeout: Duration::from_millis(500),
        ..opts(4)
    };
    let (req, token) = (request(url, &out), CancellationToken::new());
    let run = download(&req, &o, None, &token);
    tokio::time::timeout(Duration::from_secs(20), run)
        .await
        .expect("waited for an answer that never comes")
        .expect("download");
    assert_verified(&out, 7, size);
}

/// Answers that come a little late and a body that trickles in small pieces are not mistaken
/// for a stalled server: the download finishes with no connection failed on the way.
#[tokio::test(flavor = "multi_thread")]
async fn slow_but_live_server_is_not_cut_off() {
    let base = start().await;
    let dir = workdir("slow-live");
    let out = dir.join("f.bin");
    let size = MB;
    // Answers after 0.3 s (the wait for one is 3 s); pieces of about 13 KiB every 50 ms.
    let url = format!("{base}/file/f.bin?size={size}&seed=7&latency_ms=300&rate=256K");
    let o = Options {
        read_timeout: Duration::from_secs(1),
        ..opts(2)
    };
    let (_, reconnects) = counting_reconnects(url, &out, &o).await;
    assert_eq!(reconnects, 0);
    assert_verified(&out, 7, size);
}

/// One connection gets its headers and then nothing, while the others download: it is
/// replaced after a few seconds, long before the read timeout.
#[tokio::test(flavor = "multi_thread")]
async fn silent_connection_is_replaced_before_the_read_timeout() {
    let base = start().await;
    let dir = workdir("silent");
    let out = dir.join("f.bin");
    let url = format!(
        "{base}/file/f.bin?size={}&seed=7&rate=2M&slow_conn=2:0",
        16 * MB
    );
    let o = Options {
        read_timeout: Duration::from_secs(60),
        ..opts(4)
    };
    let started = std::time::Instant::now();
    get(&request(url, &out), &o).await.expect("download");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    assert_verified(&out, 7, 16 * MB);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_range_support_uses_one_connection() {
    ok("no-ranges", 8 * MB, "&ranges=off", 16).await;
}

/// Without ranges every attempt starts from the first byte, so one that delivers part of the
/// file before the server cuts it is no progress: a server that always cuts gives up after
/// `max_failures` attempts instead of being asked again forever.
#[tokio::test(flavor = "multi_thread")]
async fn no_range_server_that_always_drops_gives_up() {
    let base = start().await;
    let dir = workdir("no-ranges-drop");
    let out = dir.join("f.bin");
    let url = format!(
        "{base}/file/f.bin?size={}&seed=7&ranges=off&drop_after=256K",
        2 * MB
    );
    let o = Options {
        max_failures: 2,
        ..opts(4)
    };
    let (req, token) = (request(url, &out), CancellationToken::new());
    let run = download(&req, &o, None, &token);
    let err = tokio::time::timeout(Duration::from_secs(30), run)
        .await
        .expect("kept retrying")
        .expect_err("must fail");
    assert!(
        matches!(err, Error::TooManyFailures { attempts: 3, .. }),
        "{err}"
    );
    // The probe and one request per retry: max_failures + 1 attempts in all.
    assert_eq!(ranges_asked(&base).await.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn fake_range_support_falls_back_to_one_connection() {
    ok("fake-ranges", 8 * MB, "&range_as_200=1&rate=16M", 16).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_length() {
    ok("no-length", 8 * MB, "&no_length=1", 16).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn follows_redirects() {
    ok("redirect", 8 * MB, "&redirect=3&rate=8M", 8).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_file_is_detected_and_discarded() {
    let base = start().await;
    let dir = workdir("etag-rotate");
    let out = dir.join("f.bin");
    let url = format!("{base}/file/f.bin?size={}&etag=rotate&rate=4M", 16 * MB);
    let err = get(&request(url, &out), &opts(8))
        .await
        .expect_err("must fail");
    assert!(matches!(err, Error::FileChanged), "{err}");
    assert!(!out.exists());
    assert!(!PathBuf::from(format!("{}.shardload-part", out.display())).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn html_error_page_is_not_written_as_data() {
    let base = start().await;
    let dir = workdir("html");
    let out = dir.join("f.bin");
    let url = format!(
        "{base}/file/f.bin?size={}&html_error_after=1&rate=4M",
        16 * MB
    );
    let err = get(&request(url, &out), &opts(8))
        .await
        .expect_err("must fail");
    assert!(matches!(err, Error::HtmlResponse), "{err}");
    assert!(!out.exists());
}

/// Downloads at a limited rate and interrupts the download after `after`, either cleanly
/// (`cancel`) or by dropping it mid-flight like a killed process would.
/// Bytes the state file next to `out` records as written, 0 while there is none.
fn saved(out: &Path) -> u64 {
    let Ok(text) = std::fs::read_to_string(format!("{}.shardload-state", out.display())) else {
        return 0;
    };
    let Ok(state) = serde_json::from_str::<serde_json::Value>(&text) else {
        return 0;
    };
    state["segments"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| s["written"].as_u64().unwrap_or(0) - s["start"].as_u64().unwrap_or(0))
        .sum()
}

/// Runs the download until its state file records at least `saved_at_least` bytes, then
/// cancels it or drops it like a killed process. Waiting for the checkpoint instead of a fixed
/// time keeps a slow CI machine from interrupting before anything was saved.
async fn interrupt(url: &str, out: &Path, saved_at_least: u64, cancel: bool) -> u64 {
    interrupt_with(url, out, saved_at_least, cancel, 4).await
}

/// [`interrupt`] with this many connections.
async fn interrupt_with(
    url: &str,
    out: &Path,
    saved_at_least: u64,
    cancel: bool,
    connections: usize,
) -> u64 {
    let o = Options {
        checkpoint_every: Duration::from_millis(200),
        ..opts(connections)
    };
    let token = CancellationToken::new();
    let req = request(url.to_owned(), out);
    let run = download(&req, &o, None, &token);
    tokio::pin!(run);
    let checkpointed = async {
        while saved(out) < saved_at_least {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::select! {
        r = &mut run => panic!("finished before the interruption: {r:?}"),
        () = checkpointed => {}
        () = tokio::time::sleep(LIMIT) => panic!("nothing checkpointed within {LIMIT:?}"),
    }
    if cancel {
        token.cancel();
        let r = tokio::time::timeout(LIMIT, run).await.expect("cancel hung");
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    }
    // Otherwise `run` is dropped here without a final checkpoint.
    saved(out)
}

async fn interrupted_then_resumed(name: &str, cancel: bool) {
    let base = start().await;
    let dir = workdir(name);
    let out = dir.join("f.bin");
    let size = 16 * MB;
    // 4 connections at 1 MiB/s: about 4 s in total.
    let url = format!("{base}/file/f.bin?size={size}&seed=3&rate=1M");
    let saved = interrupt(&url, &out, 10 * MB, cancel).await;
    assert!(PathBuf::from(format!("{}.shardload-state", out.display())).exists());

    let started = Instant::now();
    let done = get(&request(url, &out), &opts(4)).await.expect("resume");
    if cancel {
        // Stopped cleanly, the data flushed: all of it counts.
        assert!(
            done.resumed_from >= saved,
            "{} of {saved}",
            done.resumed_from
        );
    } else {
        // Killed: the last seconds of each part may never have reached the disk, and are
        // downloaded again (at most 2 s of each part at its speed: about 8 MiB here).
        assert!(
            done.resumed_from < saved,
            "resumed from {} of {saved}",
            done.resumed_from
        );
    }
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_verified(&out, 3, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_then_resume() {
    interrupted_then_resumed("cancel-resume", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn killed_then_resume() {
    interrupted_then_resumed("kill-resume", false).await;
}

/// A power loss: the state file counts bytes that never reached the disk (they read as zeros).
/// Resuming downloads each part's last seconds again, so the file still comes out right.
#[tokio::test(flavor = "multi_thread")]
async fn bytes_lost_with_the_power_are_downloaded_again() {
    use std::io::{Seek, SeekFrom, Write};
    let base = start().await;
    let dir = workdir("power-loss");
    let out = dir.join("f.bin");
    let size = 16 * MB;
    let url = format!("{base}/file/f.bin?size={size}&seed=5&rate=1M");
    interrupt(&url, &out, 10 * MB, false).await;
    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{}.shardload-state", out.display())).expect("state"),
    )
    .expect("json");
    assert_eq!(
        state["clean"], false,
        "a checkpoint while running is not clean"
    );
    // The last 512 KiB each part counts were lost.
    let mut part = std::fs::OpenOptions::new()
        .write(true)
        .open(format!("{}.shardload-part", out.display()))
        .expect("part");
    for s in state["segments"].as_array().expect("segments") {
        let (start, written) = (
            s["start"].as_u64().expect("start"),
            s["written"].as_u64().expect("written"),
        );
        let lost = (written - start).min(512 << 10);
        part.seek(SeekFrom::Start(written - lost)).expect("seek");
        part.write_all(&vec![0; lost as usize]).expect("zero");
    }
    drop(part);
    get(&request(url, &out), &opts(4)).await.expect("resume");
    assert_verified(&out, 5, size);
}

/// The file on the server got shorter than what an interrupted download already has, so
/// asking for the rest is refused (416): the download starts over, from the same address and
/// from a new link alike, like any other change of the file.
#[tokio::test(flavor = "multi_thread")]
async fn resume_past_the_end_of_a_shrunk_file_starts_over() {
    let base = start().await;
    for same_address in [true, false] {
        let dir = workdir(&format!("shrunk-{same_address}"));
        let out = dir.join("f.bin");
        // Without an ETag the validator sent along (Last-Modified) still matches, so the
        // server answers the range itself instead of sending the whole new file.
        let long = format!("{base}/file/f.bin?size={}&seed=3&etag=none&rate=1M", 8 * MB);
        interrupt_with(&long, &out, 2 * MB, true, 1).await;
        let short = format!("{base}/file/f.bin?size={MB}&seed=3&etag=none");
        if same_address {
            // The address the download used now serves the shorter file.
            point_state_at(&out, &short);
        }
        let done = get(&request(short, &out), &opts(4))
            .await
            .unwrap_or_else(|e| panic!("same address {same_address}: {e}"));
        assert_eq!((done.size, done.resumed_from), (MB, 0));
        assert_verified(&out, 3, MB);
    }
}

/// Makes the interrupted download of `out` look as if it had used `url` all along.
fn point_state_at(out: &Path, url: &str) {
    let path = format!("{}.shardload-state", out.display());
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("state")).expect("json");
    state["url"] = url.into();
    std::fs::write(&path, state.to_string()).expect("write state");
}

/// A server refuses the rest of an interrupted download (416) while saying the file still has
/// the size the download knows, like an edge cache that refuses ranges for a while: nothing
/// says the file changed, so the partial download is kept for a later attempt, from the same
/// address and from a new link alike.
#[tokio::test(flavor = "multi_thread")]
async fn refused_resume_of_an_unchanged_file_keeps_the_partial_download() {
    let base = start().await;
    for same_address in [true, false] {
        let dir = workdir(&format!("refused-{same_address}"));
        let out = dir.join("f.bin");
        // Its own URLs each time: the server counts the GETs per URL.
        let query = format!("size={}&seed=3&case={same_address}", 8 * MB);
        interrupt_with(
            &format!("{base}/file/f.bin?{query}&rate=1M"),
            &out,
            2 * MB,
            true,
            1,
        )
        .await;
        let refusing = format!("{base}/file/f.bin?{query}&range_416_get=1");
        if same_address {
            point_state_at(&out, &refusing);
        }
        let err = get(&request(refusing.clone(), &out), &opts(4))
            .await
            .expect_err("refused");
        assert!(matches!(err, Error::Status(416)), "{same_address}: {err}");
        for kept in [".shardload-part", ".shardload-state"] {
            assert!(PathBuf::from(format!("{}{kept}", out.display())).exists());
        }
        // The next attempt is not refused, and goes on from there.
        let done = get(&request(refusing, &out), &opts(4))
            .await
            .expect("resume");
        assert!(done.resumed_from >= 2 * MB, "{}", done.resumed_from);
        assert_verified(&out, 3, 8 * MB);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn completed_download_is_not_fetched_again() {
    let base = start().await;
    let dir = workdir("again");
    let out = dir.join("f.bin");
    let url = format!("{base}/file/f.bin?size={}&seed=7", 4 * MB);
    get(&request(url.clone(), &out), &opts(4))
        .await
        .expect("first");
    // The output exists and no state is left: a new run downloads afresh and still verifies.
    get(&request(url, &out), &opts(4)).await.expect("second");
    assert_verified(&out, 7, 4 * MB);
}

#[tokio::test(flavor = "multi_thread")]
async fn what_the_server_allows_is_learned() {
    let base = start().await;
    let dir = workdir("learned");
    for (query, max, ranges) in [
        ("&rate=4M&max_conns=3", Some(3), false),
        ("&rate=4M&max_conns=3&refuse_status=403", Some(3), false),
        ("&range_as_200=1&rate=16M", None, true),
        ("&rate=8M", None, false),
    ] {
        let out = dir.join("f.bin");
        let url = format!("{base}/file/f.bin?size={}&seed=7{query}", 16 * MB);
        let o = opts(16);
        get(&request(url, &out), &o)
            .await
            .unwrap_or_else(|e| panic!("{query}: {e}"));
        let learned = o.learn.get();
        // A refused connection may come while fewer were running, so the limit learned
        // can be below the server's.
        match max {
            Some(max) => assert!(
                learned
                    .max_connections
                    .is_some_and(|m| (1..=max).contains(&m)),
                "{query}: {learned:?}"
            ),
            None => assert_eq!(learned.max_connections, None, "{query}"),
        }
        assert_eq!(learned.no_ranges, ranges, "{query}");
        if query == "&rate=8M" {
            assert!(learned.peak_connections > 3, "{learned:?}");
        }
        std::fs::remove_file(&out).ok();
    }
}
