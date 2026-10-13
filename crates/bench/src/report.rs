use std::fmt::Write;

use crate::config::{Client, Scenario};
use crate::run::{RunResult, Status};

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    Some(if v.len().is_multiple_of(2) {
        (v[m - 1] + v[m]) / 2.0
    } else {
        v[m]
    })
}

fn fmt(v: Option<f64>, decimals: usize) -> String {
    v.map_or_else(|| "–".into(), |x| format!("{x:.decimals$}"))
}

/// One table per scenario; numbers are medians over the successful runs.
pub fn markdown(
    scenarios: &[&Scenario],
    clients: &[&Client],
    results: &[RunResult],
    repeat: u32,
) -> String {
    let mut md = String::new();
    let _ = writeln!(
        md,
        "# Benchmark — {}\n\nLocal test server, {repeat} run(s) per cell, medians of successful runs. \
         *Tail* = time spent on the last 10 % of bytes. *Overhead* = bytes sent beyond file size.\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    );
    for s in scenarios {
        let _ = writeln!(
            md,
            "## {} · {}\n\n{}\n\n`size={}&seed={}{}{}`\n",
            s.name,
            human(s.size.0),
            s.description,
            s.size.0,
            s.seed,
            if s.query.is_empty() { "" } else { "&" },
            s.query
        );
        md.push_str(
            "| Client | OK | Time (s) | MiB/s | Tail (s) | Peak conns | GETs | Overhead % | CPU % | RAM (MiB) |\n\
             |---|---|---:|---:|---:|---:|---:|---:|---:|---:|\n",
        );
        for c in clients {
            let runs: Vec<&RunResult> = results
                .iter()
                .filter(|r| r.scenario == s.name && r.client == c.name)
                .collect();
            let ok: Vec<&&RunResult> = runs.iter().filter(|r| r.status == Status::Ok).collect();
            let col = |f: &dyn Fn(&RunResult) -> Option<f64>| {
                median(ok.iter().filter_map(|r| f(r)).collect())
            };
            let mut status = format!("{}/{}", ok.len(), runs.len());
            if let Some(bad) = runs.iter().find(|r| r.status != Status::Ok) {
                let _ = write!(
                    status,
                    " ({:?}: {})",
                    bad.status,
                    bad.detail.replace('|', "/")
                );
            }
            let _ = writeln!(
                md,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                c.name,
                status,
                fmt(col(&|r| Some(r.secs)), 2),
                fmt(col(&|r| Some(r.mib_s)), 1),
                fmt(col(&|r| r.tail_secs), 2),
                fmt(col(&|r| Some(f64::from(r.peak_conns))), 0),
                fmt(col(&|r| Some(r.gets as f64)), 0),
                fmt(col(&|r| Some(r.overhead_pct)), 1),
                fmt(col(&|r| r.cpu_avg_pct.map(f64::from)), 0),
                fmt(col(&|r| r.peak_rss_mib), 0),
            );
        }
        md.push('\n');
    }
    md
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 && b % (1 << 30) == 0 => format!("{} GiB", b >> 30),
        b if b >= 1 << 20 => format!("{} MiB", b >> 20),
        b => format!("{b} B"),
    }
}
