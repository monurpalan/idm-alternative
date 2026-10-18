//! Picking files out of a ZIP archive on the server.

mod common;

use shardload_engine::{
    CancellationToken, Error, Options, Progress, RemoteZip, select_zip_entries,
};
use shardload_testserver::zip::{sample, sample_entries};

use common::{LIMIT, bytes_sent, start, workdir};

async fn open(url: &str) -> shardload_engine::Result<RemoteZip> {
    let opts = Options {
        browser: None,
        ..Options::default()
    };
    tokio::time::timeout(LIMIT, RemoteZip::open(url, &[], &opts))
        .await
        .expect("open hung")
}

fn pick(zip: &RemoteZip, patterns: &[&str]) -> Vec<usize> {
    let patterns: Vec<String> = patterns.iter().map(|p| (*p).to_owned()).collect();
    let (chosen, unmatched) = select_zip_entries(zip.entries(), &patterns);
    assert!(unmatched.is_empty(), "{unmatched:?}");
    chosen
}

#[tokio::test]
async fn lists_without_downloading() {
    let base = start().await;
    for zip64 in [false, true] {
        let url = format!("{base}/zip/sample.zip?seed=3&zip64={zip64}");
        let zip = open(&url).await.expect("open");
        assert_eq!(zip.is_zip64(), zip64);
        let want = sample_entries(3);
        let names: Vec<&str> = zip.entries().iter().map(|e| e.name.as_str()).collect();
        let want_names: Vec<&str> = want.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, want_names);
        for (e, w) in zip.entries().iter().zip(&want) {
            assert_eq!(e.size, w.data.len() as u64, "{}", e.name);
            assert_eq!(e.method, w.method, "{}", e.name);
            assert_eq!(e.encrypted, w.encrypted, "{}", e.name);
        }
    }
    // The sample is a few MiB; listing it reads its last 66 KiB or so.
    let total = sample(3, false).len() as u64 + sample(3, true).len() as u64;
    assert!(bytes_sent(&base).await < total / 10);
}

#[tokio::test]
async fn extracts_chosen_entries() {
    let base = start().await;
    let dir = workdir("zip-extract");
    let mut fetched = 0;
    for zip64 in [false, true] {
        let out = dir.join(&format!("out-{zip64}"));
        let url = format!("{base}/zip/sample.zip?seed=4&zip64={zip64}");
        let zip = open(&url).await.expect("open");
        // Deflated with a data descriptor, and stored.
        let chosen = pick(&zip, &["data/big.bin", "data/raw.bin", "readme.txt"]);
        fetched += chosen
            .iter()
            .map(|&i| zip.entries()[i].compressed_size)
            .sum::<u64>();
        let (tx, rx) = tokio::sync::watch::channel(Progress::default());
        let done = zip
            .extract(&chosen, &out, false, Some(&tx), &CancellationToken::new())
            .await
            .expect("extract");
        assert_eq!(done.len(), 3);
        for d in &done {
            assert_eq!(d.error, None, "{}", d.name);
            assert!(!d.skipped);
            let want = sample_entries(4)
                .into_iter()
                .find(|e| e.name == d.name)
                .expect("known entry");
            let got = std::fs::read(d.path.as_ref().expect("path")).expect("read");
            assert!(got == want.data, "{} differs", d.name);
        }
        assert!(out.join("data").join("big.bin").is_file());
        assert!(!out.join("data").join("big.bin.shardload-part").exists());
        let p = rx.borrow().clone();
        assert_eq!(p.size, Some(p.received));
        // Again: everything is there already.
        let again = zip
            .extract(&chosen, &out, false, None, &CancellationToken::new())
            .await
            .expect("extract again");
        assert!(again.iter().all(|d| d.skipped));
    }
    // The chosen entries' data, and per archive its tail (about 66 KiB) and the headers.
    let sent = bytes_sent(&base).await;
    assert!(
        sent < fetched + 2 * (80 << 10),
        "{sent} bytes sent for {fetched} bytes of entries"
    );
    let archives = sample(4, false).len() as u64 + sample(4, true).len() as u64;
    assert!(sent < archives);
}

#[tokio::test]
async fn reports_entries_it_cannot_extract() {
    let base = start().await;
    let dir = workdir("zip-refused");
    let out = dir.join("out");
    let zip = open(&format!("{base}/zip/sample.zip?seed=5"))
        .await
        .expect("open");
    let chosen = pick(&zip, &["secret.txt", "*.lzma", "../evil.txt", "docs"]);
    let done = zip
        .extract(&chosen, &out, false, None, &CancellationToken::new())
        .await
        .expect("extract");
    let error = |name: &str| {
        done.iter()
            .find(|d| d.name == name)
            .and_then(|d| d.error.clone())
            .unwrap_or_default()
    };
    assert!(error("secret.txt").contains("encrypted"));
    assert!(error("packed.lzma").contains("lzma"));
    assert!(error("../evil.txt").contains("unsafe"));
    assert_eq!(error("docs/notes.txt"), "");
    assert!(out.join("docs").join("notes.txt").is_file());
    assert!(!dir.join("evil.txt").exists());
    assert!(!out.join("secret.txt").exists());
}

#[tokio::test]
async fn a_server_without_ranges_cannot_be_read_in_parts() {
    let base = start().await;
    let err = open(&format!("{base}/zip/sample.zip?ranges=0"))
        .await
        .err()
        .expect("refused");
    assert!(matches!(err, Error::NoRanges), "{err}");
    assert!(err.to_string().contains("download the whole archive"));
}

#[tokio::test]
async fn something_else_is_not_an_archive() {
    let base = start().await;
    let err = open(&format!("{base}/file/x.bin?size=200000"))
        .await
        .err()
        .expect("refused");
    assert!(matches!(err, Error::Archive(_)), "{err}");
}
