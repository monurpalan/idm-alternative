//! A file its server no longer has: said so (`Missing`), and its archived copy added from the
//! Wayback Machine at the user's word.

mod common;

use common::*;
use shardload_manager::{ErrorKind, Manager, NewTask, Status};

#[tokio::test(flavor = "multi_thread")]
async fn a_file_the_server_no_longer_has_is_missing() {
    let base = start_server().await;
    let w = WorkDir::new("missing");
    let m = Manager::open(&w.db(), w.config(1)).await.expect("open");
    for url in [
        format!("{base}/no-such-route/gone.iso"),
        format!("{base}/file/gone.iso?status=410"),
    ] {
        let t = m.add(new_task(url.clone())).await.expect("add");
        let t = wait_for(&m, t.id, 30, |t| t.status == Status::Error).await;
        assert_eq!(t.error_kind, Some(ErrorKind::Missing), "{url}: {t:?}");
    }
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "network: the Wayback Machine"]
async fn the_archived_copy_is_added_from_the_wayback_machine() {
    let w = WorkDir::new("wayback");
    let m = Manager::open(&w.db(), w.config(1)).await.expect("open");
    let t = m
        .add(NewTask {
            url: "http://www.example.com/".into(),
            ..NewTask::default()
        })
        .await
        .expect("add");
    let copy = m
        .add_archived_copy(t.id)
        .await
        .expect("ask")
        .expect("archived");
    let added = m.get(copy.id).await.expect("added");
    assert!(
        added.url.starts_with("https://web.archive.org/web/")
            && added.url.ends_with("id_/http://www.example.com/"),
        "{}",
        added.url
    );
    m.close().await;
}
