//! The same content under another address, found by its checksum before it is downloaded
//! again, and copied or hard-linked instead.

use std::path::PathBuf;
use std::time::Duration;

use sha2::{Digest, Sha256};
use shardload_manager::{Config, Manager, Matched, NewTask, Reuse, Status};
use shardload_testserver::{Verdict, verify_file};

const SIZE: u64 = 2 << 20;

async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, shardload_testserver::router()).await });
    format!("http://{addr}")
}

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("shardload-manager-tests")
        .join(format!("dedup-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("workdir");
    dir
}

/// `sha256:<hex>` of the test file for `seed`.
fn sha256(seed: u64) -> String {
    let mut data = vec![0u8; SIZE as usize];
    shardload_testserver::content::fill(seed, 0, &mut data);
    let hex: String = Sha256::digest(&data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_content_is_found_and_reused() {
    let server = serve().await;
    let dir = workdir("reuse");
    let m = Manager::open(
        &dir.join("list.db"),
        Config {
            dir: dir.join("downloads"),
            engine: shardload_engine::Options {
                find_checksum: false,
                ..shardload_engine::Options::default()
            },
            ..Config::default()
        },
    )
    .await
    .expect("open");
    // Downloaded once, without a checksum: its SHA-256 is worked out afterwards.
    let first = m
        .add(NewTask {
            url: format!("{server}/file/original.iso?size=2M"),
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let history = m.history().await.expect("history");
        if history.first().is_some_and(|h| h.sha256.is_some()) {
            assert_eq!(history[0].sha256.as_deref(), Some(sha256(1).as_str()));
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "never hashed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let original = m.get(first.id).await.expect("get").path.expect("path");

    // Another address and name, the same bytes.
    let other = format!("{server}/file/mirror-copy.iso?size=2M");
    let find = |checksum: Option<String>, size: u64| {
        let (m, other) = (&m, &other);
        async move {
            m.find_same_content(
                other,
                Some("mirror-copy.iso"),
                Some(size),
                checksum.as_deref(),
                &[],
                None,
            )
            .await
            .expect("find")
        }
    };
    let found = find(Some(sha256(1)), SIZE)
        .await
        .expect("found by its checksum");
    assert_eq!(found.matched, Matched::Content);
    assert!(found.on_disk && found.linkable, "{found:?}");
    assert_eq!(found.path.as_deref(), Some(original.as_path()));
    assert!(
        find(Some(sha256(2)), SIZE).await.is_none(),
        "another content"
    );
    assert!(
        find(Some(sha256(1)), SIZE + 1).await.is_none(),
        "another size"
    );
    assert!(find(None, SIZE).await.is_none(), "nothing to compare with");
    let sha1 = format!("sha1:{}", "ab".repeat(20));
    assert!(
        find(Some(sha1), SIZE).await.is_none(),
        "only SHA-256 is kept"
    );

    let history = found.history.expect("history");
    let copy = m
        .reuse(history, None, Some("mirror-copy.iso".into()), Reuse::Copy)
        .await
        .expect("copy");
    assert_eq!(copy.status, Status::Done);
    let copied = copy.path.clone().expect("path");
    assert_eq!(copied, dir.join("downloads").join("mirror-copy.iso"));
    assert_eq!(verify_file(&copied, 1, SIZE).expect("verify"), Verdict::Ok);

    let link = m
        .reuse(history, None, Some("linked.iso".into()), Reuse::Link)
        .await
        .expect("link");
    let linked = link.path.clone().expect("path");
    assert_eq!(verify_file(&linked, 1, SIZE).expect("verify"), Verdict::Ok);
    // A hard link: a change through one name shows through the other.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&linked)
        .and_then(|mut f| std::io::Write::write_all(&mut f, b"!"))
        .expect("append");
    assert_eq!(std::fs::metadata(&original).expect("meta").len(), SIZE + 1);

    // The original changed (through its link): the copy, which is still right, is offered.
    let found = find(Some(sha256(1)), SIZE).await.expect("the copy");
    assert_eq!(found.path.as_deref(), Some(copied.as_path()));
    assert!(
        m.reuse(history, None, None, Reuse::Copy).await.is_err(),
        "changed file reused"
    );
    // Gone: not offered.
    std::fs::remove_file(&copied).expect("remove");
    assert!(
        find(Some(sha256(1)), SIZE).await.is_none(),
        "deleted file offered"
    );
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}
