//! Choosing the file name from the server's answer before downloading.

mod common;

use idma_engine::{Options, probe};

use common::{LIMIT, MB, start};

async fn ask(url: String) -> idma_engine::Probe {
    tokio::time::timeout(LIMIT, probe(&url, &[], &Options::default()))
        .await
        .expect("probe hung")
        .expect("probe")
}

#[tokio::test]
async fn name_from_content_disposition() {
    let base = start().await;
    let p = ask(format!(
        "{base}/file/x.bin?size={MB}&cd=%C3%A7ay%20bardak.zip"
    ))
    .await;
    assert_eq!(p.name, "çay bardak.zip");
    assert_eq!(p.size, Some(MB));
    assert!(p.resumable);
}

#[tokio::test]
async fn hostile_names_are_made_safe() {
    let base = start().await;
    for (cd, want) in [
        ("..%2F..%2Fevil.exe", "evil.exe"),
        ("..%5C..%5Cevil.exe", "evil.exe"),
        ("CON.txt", "_CON.txt"),
        ("a%3Fb%2A.zip", "a_b_.zip"),
        ("trailing.%20.", "trailing"),
    ] {
        let p = ask(format!("{base}/file/x.bin?size=1000&cd={cd}")).await;
        assert_eq!(p.name, want, "cd={cd}");
    }
}

#[tokio::test]
async fn name_from_url_after_redirects() {
    let base = start().await;
    let p = ask(format!("{base}/file/%C5%9Fey.iso?size=1000&redirect=2")).await;
    assert_eq!(p.name, "şey.iso");
    assert!(p.url.contains("redirect=0"), "{}", p.url);
}

#[tokio::test]
async fn no_ranges_is_not_resumable() {
    let base = start().await;
    let p = ask(format!("{base}/file/a.bin?size=1000&ranges=off")).await;
    assert_eq!(p.name, "a.bin");
    assert!(!p.resumable);
}
