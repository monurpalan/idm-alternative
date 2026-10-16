use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use idma_testserver::Verdict;
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::io::AsyncReadExt;

use crate::config::{Client, Scenario};

const SAMPLE_EVERY: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Corrupt,
    WrongLength,
    ClientError,
    Timeout,
    NotFound,
}

#[derive(Debug, Serialize)]
pub struct RunResult {
    pub scenario: String,
    pub client: String,
    pub run: u32,
    pub status: Status,
    pub detail: String,
    /// Wall time from process start to exit.
    pub secs: f64,
    /// File size / wall time.
    pub mib_s: f64,
    /// Time spent on the last 10 % of the bytes (server-side view).
    pub tail_secs: Option<f64>,
    /// From the server having sent the whole file to the client exiting: assembling parts,
    /// flushing, shutting down — work after the network part is over.
    pub after_secs: Option<f64>,
    /// GET requests the server received.
    pub gets: u64,
    /// Most concurrent responses at any moment.
    pub peak_conns: u32,
    /// Bytes the server sent beyond the file size (retries, overlaps, discarded bodies).
    pub overhead_pct: f64,
    /// Average client CPU usage (100 = one core).
    pub cpu_avg_pct: Option<f32>,
    pub peak_rss_mib: Option<f64>,
}

impl RunResult {
    pub fn one_line(&self) -> String {
        match self.status {
            Status::Ok => format!(
                "ok {:.2}s {:.1} MiB/s tail {} conns {} gets {}",
                self.secs,
                self.mib_s,
                self.tail_secs.map_or("-".into(), |t| format!("{t:.2}s")),
                self.peak_conns,
                self.gets
            ),
            s => format!("{s:?}: {}", self.detail),
        }
    }
}

#[derive(Deserialize)]
struct UrlStats {
    url: String,
    gets: u64,
    peak_active: u32,
    bytes_sent: u64,
}

async fn url_stats(http: &reqwest::Client, base: &str, url_path: &str) -> Option<UrlStats> {
    let all: Vec<UrlStats> = http
        .get(format!("{base}/_stats"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    all.into_iter().find(|s| s.url == url_path)
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

pub async fn run_once(
    base: &str,
    scenario: &Scenario,
    client: &Client,
    run: u32,
    out_dir: &Path,
    timeout_secs: u64,
    reference: &mut Option<String>,
) -> RunResult {
    let size = scenario.size.0;
    let file = format!("{}-{}-{run}.bin", slug(&scenario.name), slug(&client.name));
    let path = out_dir.join(&file);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut url_path = format!("/file/{file}?size={size}&seed={}", scenario.seed);
    if !scenario.query.is_empty() {
        url_path.push('&');
        url_path.push_str(&scenario.query);
    }
    url_path.push_str(&format!("&run={unique}"));
    // A real URL gets a unique query too: no client is served from a cache warmed by the
    // previous one, and IDM does not see a duplicate download.
    let url = match &scenario.url {
        Some(u) => format!("{u}{}run={unique}", if u.contains('?') { '&' } else { '?' }),
        None => format!("{base}{url_path}"),
    };
    let local = scenario.url.is_none();

    let mut result = RunResult {
        scenario: scenario.name.clone(),
        client: client.name.clone(),
        run,
        status: Status::ClientError,
        detail: String::new(),
        secs: 0.0,
        mib_s: 0.0,
        tail_secs: None,
        after_secs: None,
        gets: 0,
        peak_conns: 0,
        overhead_pct: 0.0,
        cpu_avg_pct: None,
        peak_rss_mib: None,
    };

    cleanup(&path);
    let args: Vec<String> = client
        .args
        .iter()
        .map(|a| {
            a.replace("{url}", &url)
                .replace("{path}", &path.to_string_lossy())
                .replace("{dir}", &out_dir.to_string_lossy())
                .replace("{file}", &file)
        })
        .collect();

    let started = Instant::now();
    let mut child = match tokio::process::Command::new(&client.program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            result.status = Status::NotFound;
            result.detail = format!("{}: {e}", client.program);
            return result;
        }
    };
    let stderr_task = child.stderr.take().map(|mut s| {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).await.ok();
            String::from_utf8_lossy(&buf).into_owned()
        })
    });

    let http = reqwest::Client::new();
    let pid = child.id().map(Pid::from_u32);
    let mut sys = System::new();
    // Cumulative CPU time (ms) at the last sample; averaged over wall time at the end.
    let mut cpu_ms = 0u64;
    let mut peak_rss = 0u64;
    let mut t90 = None;
    let mut t100 = None;
    let deadline = started + Duration::from_secs(timeout_secs);
    let mut tick = tokio::time::interval(SAMPLE_EVERY);

    let exit = loop {
        tokio::select! {
            status = child.wait() => break Some(status),
            _ = tick.tick() => {
                if Instant::now() > deadline {
                    child.kill().await.ok();
                    break None;
                }
                if local
                    && t100.is_none()
                    && let Some(s) = url_stats(&http, base, &url_path).await
                {
                    if t90.is_none() && s.bytes_sent * 10 >= size * 9 {
                        t90 = Some(started.elapsed());
                    }
                    if s.bytes_sent >= size {
                        t100 = Some(started.elapsed());
                    }
                }
                if let Some(pid) = pid {
                    sys.refresh_processes_specifics(
                        ProcessesToUpdate::Some(&[pid]),
                        true,
                        ProcessRefreshKind::nothing().with_cpu().with_memory(),
                    );
                    if let Some(p) = sys.process(pid) {
                        cpu_ms = cpu_ms.max(p.accumulated_cpu_time());
                        peak_rss = peak_rss.max(p.memory());
                    }
                }
            }
        }
    };
    let elapsed = started.elapsed();
    result.secs = elapsed.as_secs_f64();
    result.mib_s = size as f64 / result.secs.max(1e-9) / (1u64 << 20) as f64;
    result.tail_secs = t90.map(|t| elapsed.saturating_sub(t).as_secs_f64());
    result.after_secs = t100.map(|t| elapsed.saturating_sub(t).as_secs_f64());
    if cpu_ms > 0 {
        result.cpu_avg_pct = Some((cpu_ms as f64 / (result.secs * 1000.0) * 100.0) as f32);
    }
    if peak_rss > 0 {
        result.peak_rss_mib = Some(peak_rss as f64 / (1u64 << 20) as f64);
    }

    if local && let Some(s) = url_stats(&http, base, &url_path).await {
        result.gets = s.gets;
        result.peak_conns = s.peak_active;
        result.overhead_pct = (s.bytes_sent as f64 / size.max(1) as f64 - 1.0) * 100.0;
    }

    let stderr = match stderr_task {
        Some(t) => t.await.unwrap_or_default(),
        None => String::new(),
    };

    match exit {
        None => {
            result.status = Status::Timeout;
            result.detail = format!("killed after {timeout_secs}s");
        }
        Some(Err(e)) => result.detail = e.to_string(),
        Some(Ok(code)) if !code.success() && !client.ignore_exit_code => {
            result.detail = format!("{code}: {}", stderr.lines().last().unwrap_or_default());
        }
        Some(Ok(_)) if !local => verify_remote(&path, size, reference, &mut result).await,
        Some(Ok(_)) => {
            let p = path.clone();
            let seed = scenario.seed;
            match tokio::task::spawn_blocking(move || idma_testserver::verify_file(&p, seed, size))
                .await
            {
                Ok(Ok(Verdict::Ok)) => result.status = Status::Ok,
                Ok(Ok(Verdict::WrongLength { expected, actual })) => {
                    result.status = Status::WrongLength;
                    result.detail = format!("{actual} bytes, expected {expected}");
                }
                Ok(Ok(Verdict::Mismatch { offset })) => {
                    result.status = Status::Corrupt;
                    result.detail = format!("first wrong byte at {offset}");
                }
                Ok(Err(e)) => result.detail = format!("output: {e}"),
                Err(e) => result.detail = e.to_string(),
            }
        }
    }

    cleanup(&path);
    result
}

/// Removes the output file and the control files some clients leave next to it.
fn cleanup(path: &Path) {
    std::fs::remove_file(path).ok();
    let mut aria2 = path.as_os_str().to_owned();
    aria2.push(".aria2");
    std::fs::remove_file(aria2).ok();
}

/// Internet runs: the size must match, and the content must hash like the first
/// successful download of the same scenario (the servers publish no checksums).
async fn verify_remote(
    path: &Path,
    size: u64,
    reference: &mut Option<String>,
    result: &mut RunResult,
) {
    let p = path.to_owned();
    let hashed = tokio::task::spawn_blocking(move || -> std::io::Result<(u64, String)> {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let mut file = std::fs::File::open(&p)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 4 << 20];
        let mut len = 0u64;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            len += n as u64;
        }
        let hex = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok((len, hex))
    })
    .await;
    match hashed {
        Ok(Ok((len, _))) if len != size => {
            result.status = Status::WrongLength;
            result.detail = format!("{len} bytes, expected {size}");
        }
        Ok(Ok((_, hash))) => match reference {
            Some(r) if *r != hash => {
                result.status = Status::Corrupt;
                result.detail = format!("sha256 {hash} differs from {r}");
            }
            Some(_) => result.status = Status::Ok,
            None => {
                *reference = Some(hash);
                result.status = Status::Ok;
            }
        },
        Ok(Err(e)) => result.detail = format!("output: {e}"),
        Err(e) => result.detail = e.to_string(),
    }
}
