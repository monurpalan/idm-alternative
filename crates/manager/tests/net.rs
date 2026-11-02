//! Network and power rules against the test server, with made-up conditions: videos wait on a
//! metered network, a speed limit on battery, a proxy on a named network, and the monthly
//! data budget.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use shardload_manager::net::{Conditions, NetRules, NetworkProxy, Sensor};
use shardload_manager::{Config, Event, Manager, NewTask, Status, Task, TaskId};

async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, shardload_testserver::router()).await });
    format!("http://{addr}")
}

struct Setup {
    m: Manager,
    now: Arc<Mutex<Conditions>>,
    /// Times the manager has read the conditions.
    reads: Arc<AtomicU64>,
    dir: PathBuf,
    config: Config,
}

impl Setup {
    /// Waits until the manager has read the conditions twice more: the first read after a
    /// change has then been acted on, however busy the machine is.
    async fn sensed(&self) {
        let until = self.reads.load(Ordering::SeqCst) + 2;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while self.reads.load(Ordering::SeqCst) < until {
            assert!(
                tokio::time::Instant::now() < deadline,
                "conditions not read"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

async fn open(name: &str, rules: NetRules) -> Setup {
    let dir = std::env::temp_dir()
        .join("shardload-manager-tests")
        .join(format!("net-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("workdir");
    let now = Arc::new(Mutex::new(Conditions::default()));
    let reads = Arc::new(AtomicU64::new(0));
    let (read, counted) = (Arc::clone(&now), Arc::clone(&reads));
    let config = Config {
        dir: dir.join("downloads"),
        connections: 2,
        max_retries: 0,
        engine: shardload_engine::Options {
            find_checksum: false,
            check_structure: false,
            ..shardload_engine::Options::default()
        },
        sensor: Some(Sensor(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            read.lock().expect("lock").clone()
        }))),
        net_rules: rules,
        sense_every: Duration::from_millis(200),
        ..Config::default()
    };
    let m = Manager::open(&dir.join("list.db"), config.clone())
        .await
        .expect("open");
    Setup {
        m,
        now,
        reads,
        dir,
        config,
    }
}

async fn add(m: &Manager, url: String) -> Task {
    m.add(NewTask {
        url,
        start: true,
        ..NewTask::default()
    })
    .await
    .expect("add")
}

async fn wait(m: &Manager, id: TaskId, what: &str, pred: impl Fn(&Task) -> bool) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let t = m.get(id).await.expect("get");
        if pred(&t) {
            return t;
        }
        assert!(tokio::time::Instant::now() < deadline, "{what}: {t:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn videos_wait_on_a_metered_network() {
    let server = serve().await;
    let s = open(
        "metered",
        NetRules {
            metered_hold: true,
            ..NetRules::default()
        },
    )
    .await;
    s.now.lock().expect("lock").metered = true;
    s.sensed().await;
    let video = add(&s.m, format!("{server}/file/film.mp4?size=8M&rate=2M")).await;
    let zip = add(&s.m, format!("{server}/file/files.zip?size=2M")).await;
    wait(&s.m, zip.id, "the zip downloads", |t| {
        t.status == Status::Done
    })
    .await;
    let v = s.m.get(video.id).await.expect("get");
    assert_eq!(v.status, Status::Queued, "the video waits");
    assert!(
        v.error.as_deref().is_some_and(|e| e.contains("metered")),
        "{v:?}"
    );

    s.now.lock().expect("lock").metered = false;
    wait(&s.m, video.id, "the video starts", |t| {
        t.status == Status::Downloading
    })
    .await;
    // Metered again while it runs: back to the queue, with what it has.
    s.now.lock().expect("lock").metered = true;
    let back = wait(&s.m, video.id, "the video goes back", |t| {
        t.status == Status::Queued
    })
    .await;
    assert!(back.received > 0 || back.error.is_some());
    s.now.lock().expect("lock").metered = false;
    wait(&s.m, video.id, "the video finishes", |t| {
        t.status == Status::Done
    })
    .await;
    s.m.close().await;
    std::fs::remove_dir_all(&s.dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_speed_limit_on_battery() {
    let server = serve().await;
    let s = open(
        "battery",
        NetRules {
            battery_limit: Some(512 << 10),
            ..NetRules::default()
        },
    )
    .await;
    s.now.lock().expect("lock").on_battery = true;
    s.sensed().await;
    let started = Instant::now();
    let t = add(&s.m, format!("{server}/file/b.bin?size=2M")).await;
    wait(&s.m, t.id, "the download", |t| t.status == Status::Done).await;
    let secs = started.elapsed().as_secs_f64();
    assert!(secs > 3.0, "2 MiB at 512 KiB/s took only {secs:.1} s");
    // On mains again: no limit.
    s.now.lock().expect("lock").on_battery = false;
    s.sensed().await;
    let started = Instant::now();
    let t = add(&s.m, format!("{server}/file/c.bin?size=2M")).await;
    wait(&s.m, t.id, "the download", |t| t.status == Status::Done).await;
    assert!(started.elapsed().as_secs_f64() < 2.0);
    s.m.close().await;
    std::fs::remove_dir_all(&s.dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proxy_on_a_named_network() {
    let server = serve().await;
    let s = open(
        "proxy",
        NetRules {
            // Nothing listens there: a download through it fails.
            proxies: vec![NetworkProxy {
                network: "Cafe".into(),
                proxy: "http://127.0.0.1:9".into(),
            }],
            ..NetRules::default()
        },
    )
    .await;
    s.now.lock().expect("lock").network = Some("cafe".into());
    s.sensed().await;
    let t = add(&s.m, format!("{server}/file/p.bin?size=1M")).await;
    let t = wait(&s.m, t.id, "it fails", |t| t.status == Status::Error).await;
    assert!(t.error.is_some());
    s.now.lock().expect("lock").network = Some("Home".into());
    s.sensed().await;
    let t = add(&s.m, format!("{server}/file/q.bin?size=1M")).await;
    wait(&s.m, t.id, "it downloads", |t| t.status == Status::Done).await;
    s.m.close().await;
    std::fs::remove_dir_all(&s.dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_monthly_budget_stops_and_says_so() {
    let server = serve().await;
    let rules = NetRules {
        monthly_budget: Some(2 << 20),
        budget_stop: true,
        ..NetRules::default()
    };
    let s = open("budget", rules.clone()).await;
    let mut events = s.m.subscribe();
    let t = add(&s.m, format!("{server}/file/big.bin?size=16M&rate=4M")).await;
    let held = wait(&s.m, t.id, "the budget stops it", |t| {
        t.status == Status::Queued && t.received > 0
    })
    .await;
    assert!(
        held.error.as_deref().is_some_and(|e| e.contains("budget")),
        "{held:?}"
    );
    let status = s.m.net_status();
    assert!(
        status.over_budget && status.usage.bytes >= 2 << 20,
        "{status:?}"
    );
    // Said once at 90% and once used up.
    let mut notices = Vec::new();
    while let Ok(e) = events.try_recv() {
        if let Event::Budget(n) = e {
            notices.push(n);
        }
    }
    assert!(notices.iter().any(|n| n.stopped), "{notices:?}");
    assert!(notices.len() <= 2, "{notices:?}");
    // A new download waits as well.
    let other = add(&s.m, format!("{server}/file/small.bin?size=64K")).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(s.m.get(other.id).await.expect("get").status, Status::Queued);
    // A larger budget: both go on.
    let mut config = s.config.clone();
    config.net_rules.monthly_budget = Some(1 << 30);
    s.m.set_config(config).await.expect("config");
    wait(&s.m, t.id, "it finishes", |t| t.status == Status::Done).await;
    wait(&s.m, other.id, "it finishes", |t| t.status == Status::Done).await;
    s.m.close().await;
    std::fs::remove_dir_all(&s.dir).ok();
}
