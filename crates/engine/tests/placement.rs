//! Where a download goes: the file name from the server, files already there, free space.

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

#[tokio::test(flavor = "multi_thread")]
async fn second_download_of_the_same_file_is_refused() {
    let base = start().await;
    let dir = common::workdir("in-use");
    let out = dir.join("f.bin");
    let size = 4 * MB;
    let url = format!("{base}/file/f.bin?size={size}&seed=9&rate=2M");
    let req = common::request(url, &out);
    let opts = common::opts(2);
    let first = tokio::spawn({
        let (req, opts) = (common::request(req.url.clone(), &out), opts.clone());
        async move { common::get(&req, &opts).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    match common::get(&req, &opts).await {
        Err(idma_engine::Error::InUse(p)) => assert_eq!(p, out),
        other => panic!("expected InUse, got {other:?}"),
    }
    first.await.expect("join").expect("first download");
    common::assert_verified(&out, 9, size);
    assert!(!dir.join("f.bin.idma-lock").exists());
}

/// A file larger than the free space is refused before anything is written, instead of
/// failing halfway (the output is sparse, so space is only taken as data arrives).
#[tokio::test]
async fn file_larger_than_free_space_is_refused() {
    let base = start().await;
    let dir = common::workdir("disk-full");
    let out = dir.join("huge.bin");
    let size: u64 = 1 << 50; // 1 PiB
    let req = common::request(format!("{base}/file/huge.bin?size={size}"), &out);
    match common::get(&req, &common::opts(4)).await {
        Err(idma_engine::Error::DiskFull {
            need: Some(need),
            free: Some(free),
        }) => {
            assert_eq!(need, size);
            assert!(free < size);
        }
        other => panic!("expected DiskFull, got {other:?}"),
    }
    assert!(!dir.join("huge.bin.idma-part").exists());
    assert!(!out.exists());
}
