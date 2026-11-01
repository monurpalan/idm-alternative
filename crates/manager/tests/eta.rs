//! Time left against the test server's jumpy speed: the range covers when the download really
//! ends, and the shown value moves less than time left from the speed of the moment did.

use std::time::{Duration, Instant};

use shardload_manager::{Config, Event, Manager, NewTask, Status};

#[tokio::test(flavor = "multi_thread")]
async fn time_left_on_a_jumpy_line() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, shardload_testserver::router()).await });
    let dir = std::env::temp_dir()
        .join("shardload-manager-tests")
        .join(format!("eta-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("workdir");
    let m = Manager::open(
        &dir.join("list.db"),
        Config {
            dir: dir.join("downloads"),
            connections: 1,
            engine: shardload_engine::Options {
                find_checksum: false,
                ..shardload_engine::Options::default()
            },
            ..Config::default()
        },
    )
    .await
    .expect("open");
    let mut events = m.subscribe();
    // Between 0.5 and 2 MiB/s, a second and a half each: about 8 s for 10 MiB.
    let size = 10u64 << 20;
    let t = m
        .add(NewTask {
            url: format!("http://{addr}/file/wave.bin?size=10M&wave=512K:2M:1500"),
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");

    let started = Instant::now();
    let mut seen = Vec::new(); // (when, [soonest, latest], time left from the speed now)
    let deadline = started + Duration::from_secs(60);
    loop {
        match tokio::time::timeout(Duration::from_secs(1), events.recv()).await {
            Ok(Ok(Event::Progress(p))) if p.id == t.id && p.received < size => {
                let naive = (p.speed > 0.0).then(|| (size - p.received) as f64 / p.speed);
                if let (Some(eta), Some(naive)) = (p.eta, naive) {
                    seen.push((started.elapsed().as_secs_f64(), eta, naive));
                }
            }
            _ => {}
        }
        if m.get(t.id).await.expect("get").status == Status::Done {
            break;
        }
        assert!(Instant::now() < deadline, "too slow");
    }
    let end = started.elapsed().as_secs_f64();
    assert!(seen.len() > 10, "{} estimates", seen.len());

    let covered = seen
        .iter()
        .filter(|(at, [lo, hi], _)| *lo - 1.0 <= end - at && end - at <= *hi + 1.0)
        .count() as f64
        / seen.len() as f64;
    let jumps = |xs: Vec<f64>| xs.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>();
    let ours = jumps(
        seen.iter()
            .map(|(_, [lo, hi], _)| (lo + hi) / 2.0)
            .collect(),
    );
    let before = jumps(seen.iter().map(|(_, _, naive)| *naive).collect());
    println!(
        "{} estimates over {end:.1} s: range covered the end {:.0}% of the time; \
         total movement {ours:.1} s, from the speed of the moment {before:.1} s",
        seen.len(),
        covered * 100.0
    );
    assert!(covered > 0.8, "covered {covered}");
    assert!(ours < before, "{ours} vs {before}");
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}
