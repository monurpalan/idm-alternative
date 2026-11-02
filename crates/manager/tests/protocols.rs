//! FTP and SFTP downloads through the download list: named and sized like any other, a saved
//! login used when the server asks for one, SFTP host keys recorded in the configured file.

mod common;

use common::*;
use shardload_manager::{Config, Manager, Status};
use shardload_testserver::{ftp, sftp};

fn config(w: &WorkDir) -> Config {
    Config {
        engine: shardload_engine::Options {
            known_hosts: Some(w.path().join("known_hosts")),
            ..w.config(2).engine
        },
        ..w.config(2)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ftp_and_sftp_downloads_with_a_saved_login() {
    let w = WorkDir::new("protocols");
    let root = w.path().join("served");
    ftp::file(&root, "pub/report.pdf", 5 * MB, 51).expect("file");
    let ftp_port = ftp::serve(root.clone(), false).await.expect("ftp");
    let sftp = sftp::serve(root).await.expect("sftp");
    let m = Manager::open(&w.db(), config(&w)).await.expect("open");

    // Anonymous FTP: named after the file, its size known before it starts.
    let t = m
        .add(new_task(format!(
            "ftp://127.0.0.1:{ftp_port}/pub/report.pdf"
        )))
        .await
        .expect("add");
    let done = wait_done(&m, t.id).await;
    assert_eq!(done.size, Some(5 * MB));
    assert!(
        done.path
            .as_ref()
            .is_some_and(|p| p.ends_with("report.pdf"))
    );
    assert_file(&done, 51, 5 * MB);

    // SFTP asks for a login: refused without one, then the saved one is used.
    let url = format!("sftp://127.0.0.1:{}/pub/report.pdf", sftp.port);
    let t = m.add(new_task(url.clone())).await.expect("add");
    let failed = wait_for(&m, t.id, 30, |t| t.status == Status::Error).await;
    assert!(failed.error.is_some(), "{failed:?}");
    let site = shardload_manager::site_of(&url).expect("site");
    m.save_login(&site, ftp::USER, ftp::PASSWORD)
        .await
        .expect("save");
    m.start(t.id).await.expect("start");
    let done = wait_done(&m, t.id).await;
    assert_file(&done, 51, 5 * MB);
    let known = std::fs::read_to_string(w.path().join("known_hosts")).expect("known hosts");
    assert!(
        known.contains(&format!("[127.0.0.1]:{}", sftp.port)),
        "{known}"
    );
    m.close().await;
}
