//! The skeleton first, against the test server sending slowly: what an archive, a video (its
//! index at the start or the end) or a document holds is known in the first seconds, while
//! the download goes on to the right file.

use std::time::{Duration, Instant};

use shardload_engine::skeleton::Skeleton;
use shardload_manager::{Config, Manager, NewTask, Status};
use shardload_testserver::samples::sample;

async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, shardload_testserver::router()).await });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_skeleton_comes_first() {
    let server = serve().await;
    let dir = std::env::temp_dir()
        .join("shardload-manager-tests")
        .join(format!("skeleton-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("workdir");
    let m = Manager::open(
        &dir.join("list.db"),
        Config {
            dir: dir.join("downloads"),
            max_active: 4,
            connections: 2,
            engine: shardload_engine::Options {
                find_checksum: false,
                ..shardload_engine::Options::default()
            },
            ..Config::default()
        },
    )
    .await
    .expect("open");
    let cases = [
        ("zip", "a.zip", "256K"),
        ("mp4", "start.mp4", "96K"),
        ("mp4end", "end.mp4", "96K"),
        ("pdf", "doc.pdf", "12K"),
    ];
    let mut ids = Vec::new();
    for (kind, name, rate) in cases {
        let t = m
            .add(NewTask {
                url: format!("{server}/sample/{name}?kind={kind}&rate={rate}"),
                start: true,
                ..NewTask::default()
            })
            .await
            .expect("add");
        ids.push((kind, t.id, Instant::now()));
    }
    let mut early = Vec::new();
    for (kind, id, added) in &ids {
        let deadline = Instant::now() + Duration::from_secs(60);
        let task = loop {
            let t = m.get(*id).await.expect("get");
            if t.skeleton.is_some() || t.status == Status::Done {
                break t;
            }
            assert!(Instant::now() < deadline, "{kind}: {t:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let size = sample(kind, 1).expect(kind).len() as u64;
        assert!(
            task.status == Status::Downloading && task.received < size / 2,
            "{kind}: the skeleton came only at {} of {size} bytes ({:?})",
            task.received,
            task.status
        );
        early.push((
            kind,
            added.elapsed().as_secs_f64(),
            task.skeleton.clone().expect("skeleton"),
        ));
    }
    for (kind, secs, skeleton) in &early {
        println!("{kind}: {skeleton:?} after {secs:.1} s");
        match (**kind, skeleton) {
            ("zip", Skeleton::Archive { files, names, .. }) => {
                assert_eq!(*files, 7);
                assert!(names.iter().any(|n| n == "data/big.bin"), "{names:?}");
            }
            (
                "mp4" | "mp4end",
                Skeleton::Video {
                    duration,
                    width,
                    height,
                },
            ) => {
                assert_eq!((*duration, *width, *height), (90.0, 1280, 720));
            }
            ("pdf", Skeleton::Document { pages }) => assert_eq!(*pages, 8),
            other => panic!("{other:?}"),
        }
    }
    // The downloads are not hurt: each ends with the right bytes.
    for (kind, id, _) in &ids {
        let deadline = Instant::now() + Duration::from_secs(90);
        let t = loop {
            let t = m.get(*id).await.expect("get");
            if matches!(t.status, Status::Done | Status::Error) {
                break t;
            }
            assert!(Instant::now() < deadline, "{kind}: {t:?}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(t.status, Status::Done, "{kind}: {t:?}");
        let got = std::fs::read(t.path.expect("path")).expect("file");
        assert!(
            got == sample(kind, 1).expect(kind),
            "{kind}: the file is not right"
        );
    }
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}
