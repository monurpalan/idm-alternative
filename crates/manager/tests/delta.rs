//! Delta downloads through the list: a file whose older copy is in its folder is built from
//! it when the server has a `.zsync`, and a `.zsync` link downloads the file it describes.

mod common;

use common::*;
use shardload_manager::Manager;

/// The test server's file `name` (seed 71) as an older version: some bytes changed, some put
/// in near the start, its end cut short.
fn older(size: u64) -> Vec<u8> {
    let mut data = vec![0u8; size as usize];
    shardload_testserver::content::fill(71, 0, &mut data);
    data.truncate(data.len() - 30_000);
    data[(4 * MB) as usize..(4 * MB + 60_000) as usize].fill(9);
    data.splice(MB as usize..MB as usize, vec![3u8; 500]);
    data
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_copy_saves_downloading_what_did_not_change() {
    let base = start_server().await;
    let w = WorkDir::new("delta");
    let size = 6 * MB;
    let downloads = w.path().join("downloads");
    std::fs::create_dir_all(&downloads).expect("folder");
    std::fs::write(downloads.join("disk.iso"), older(size)).expect("older copy");
    let m = Manager::open(&w.db(), w.config(1)).await.expect("open");
    let url = format!("{base}/file/disk.iso?size=6M&seed=71");
    let t = m.add(new_task(url)).await.expect("add");
    let done = wait_done(&m, t.id).await;
    // Beside the older copy, which stays as it was.
    assert!(
        done.path
            .as_ref()
            .is_some_and(|p| p.ends_with("disk (1).iso")),
        "{:?}",
        done.path
    );
    assert_file(&done, 71, size);
    assert_eq!(
        std::fs::read(downloads.join("disk.iso")).expect("older"),
        older(size)
    );
    // What was asked for, not what the server sent (a busy machine's socket buffers hold more
    // or less of the name probe's answer): the changed blocks in ranges, the file whole only
    // by the probe.
    let asked = ranges_asked(&base).await;
    let fetched: u64 = asked
        .iter()
        .filter_map(|r| r.strip_prefix("bytes=")?.split_once('-'))
        .filter_map(|(a, b)| Some(b.parse::<u64>().ok()? + 1 - a.parse::<u64>().ok()?))
        .sum();
    assert!(fetched < MB, "{fetched} bytes asked for in {asked:?}");
    assert_eq!(
        asked.iter().filter(|r| *r == "bytes=0-").count(),
        1,
        "{asked:?}"
    );
    let note = done.error.clone().unwrap_or_default();
    assert!(note.contains("older copy disk.iso (zsync)"), "{note}");

    // A `.zsync` link: the file it describes, named by it.
    let t = m
        .add(new_task(format!(
            "{base}/file/other.bin.zsync?size=2M&seed=72"
        )))
        .await
        .expect("add");
    let done = wait_done(&m, t.id).await;
    assert_eq!(done.name.as_deref(), Some("other.bin"));
    assert_eq!(done.size, Some(2 * MB));
    assert_file(&done, 72, 2 * MB);
    m.close().await;
}
