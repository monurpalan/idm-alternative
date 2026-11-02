//! Running a finished download's actions in order: move, unpack, the commands, the webhooks.
//! Each one's outcome is kept with the download; one that fails does not stop the others.

use std::path::Path;
use std::sync::Arc;

use super::{Done, Limits, Plan, unpack_folder};
use crate::{Inner, Manager, Status, Task, TaskId, lock};

impl Inner {
    /// Runs what the download's own wishes and the user's rules ask for once it is done.
    pub(crate) async fn after_done(self: &Arc<Self>, id: TaskId) {
        let Ok(Some(task)) = self.db.get(id).await else {
            return;
        };
        let (rules, limits, opts) = {
            let cfg = lock(&self.config);
            (cfg.rules.clone(), cfg.after_limits, cfg.engine.clone())
        };
        let plan = Plan::of(&task, &rules);
        if task.status != Status::Done || plan.is_empty() {
            return;
        }
        let mut log = Vec::new();
        let mut task = task;
        if let Some(to) = plan.move_to.as_ref().filter(|to| **to != task.dir) {
            let manager = Manager {
                inner: Arc::clone(self),
            };
            match manager.relocate(id, to).await {
                Ok(moved) => {
                    log.push(Done::ok("move", to.display().to_string(), None));
                    task = moved;
                }
                Err(e) => log.push(Done::failed("move", e.to_string())),
            }
        }
        let Some(path) = task.path.clone() else {
            return;
        };
        if plan.unpack {
            log.push(unpack_it(&path).await);
        }
        for command in &plan.commands {
            log.push(run_command(command, &path, limits).await);
        }
        for hook in &plan.webhooks {
            log.push(call_webhook(hook, &task, &path, &opts, limits).await);
        }
        if let Err(e) = self.db.set_after_log(id, &log).await {
            tracing::warn!(id, "could not keep what happened after the download: {e}");
        }
        if let Ok(Some(task)) = self.db.get(id).await {
            self.emit(crate::Event::Updated(task));
        }
    }
}

async fn unpack_it(path: &Path) -> Done {
    let file = path.to_owned();
    let folder = unpack_folder(&file);
    let free = fs4::available_space(file.parent().unwrap_or(Path::new("."))).ok();
    let result =
        tokio::task::spawn_blocking(move || super::unpack::unpack(&file, &folder, free)).await;
    match result {
        Ok(Ok(u)) => {
            for s in &u.skipped {
                tracing::info!("left out while unpacking: {s}");
            }
            Done {
                ok: u.skipped.is_empty(),
                left_out: u.skipped.len(),
                ..Done::ok(
                    "unpack",
                    u.folder.display().to_string(),
                    i64::try_from(u.files).ok(),
                )
            }
        }
        Ok(Err(e)) => Done::failed("unpack", e),
        Err(e) => Done::failed("unpack", e.to_string()),
    }
}

/// Runs the user's command: split into words (quotes keep spaces), `{file}` and `{dir}`
/// replaced in each, no shell. Its output goes to the log.
async fn run_command(command: &str, file: &Path, limits: Limits) -> Done {
    let dir = file
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut w = words(command);
    if w.first().is_none_or(|p| p.trim().is_empty()) {
        return Done::failed("command", "the command is empty");
    }
    let program = w.remove(0);
    let fill = |a: &str| {
        a.replace("{file}", &file.to_string_lossy())
            .replace("{dir}", &dir.to_string_lossy())
    };
    let mut cmd = tokio::process::Command::new(fill(&program));
    cmd.args(w.iter().map(|a| fill(a)))
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let output = match tokio::time::timeout(limits.command, cmd.output()).await {
        Err(_) => {
            return Done::failed(
                "command",
                format!("{program}: stopped after {} s", limits.command.as_secs()),
            );
        }
        Ok(Err(e)) => return Done::failed("command", format!("could not run {program}: {e}")),
        Ok(Ok(o)) => o,
    };
    let text = |b: &[u8]| {
        String::from_utf8_lossy(&b[..b.len().min(4096)])
            .trim()
            .to_owned()
    };
    tracing::info!(
        program,
        code = ?output.status.code(),
        stdout = %text(&output.stdout),
        stderr = %text(&output.stderr),
        "ran the command for a finished download"
    );
    match output.status.code() {
        Some(code) => Done {
            ok: code == 0,
            ..Done::ok("command", program, Some(i64::from(code)))
        },
        None => Done::failed("command", format!("{program}: stopped by a signal")),
    }
}

async fn call_webhook(
    url: &str,
    task: &Task,
    file: &Path,
    opts: &shardload_engine::Options,
    limits: Limits,
) -> Done {
    let body = serde_json::json!({
        "event": "download.done",
        "id": task.id,
        "url": task.url,
        "name": task.name,
        "path": file,
        "size": task.size,
        "checksum": task.checksum,
        "finished_at": task.finished_at,
    });
    match shardload_engine::post_json(url.trim(), &body, opts, limits.webhook).await {
        Ok(status) => Done::ok("webhook", url.trim(), Some(i64::from(status))),
        Err(e) => Done::failed("webhook", format!("{}: {e}", url.trim())),
    }
}

/// A command line split into words, keeping quoted parts (`"C:\Program Files\x.exe" {file}`).
/// Backslashes stay as they are: they are Windows' path separators.
pub fn words(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in command.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            c => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_a_command_line() {
        assert_eq!(
            words(r#""C:\Program Files\AV\scan.exe" /quiet "{file}""#),
            [r"C:\Program Files\AV\scan.exe", "/quiet", "{file}"]
        );
        assert_eq!(words("  scan.exe   {file} "), ["scan.exe", "{file}"]);
        assert_eq!(words(r#"x.exe """#), ["x.exe", ""]);
        assert!(words("   ").is_empty());
    }

    #[tokio::test]
    async fn a_command_gets_the_file_as_one_word_and_no_shell() {
        let d = std::env::temp_dir().join(format!("shardload-after-cmd-{}", std::process::id()));
        std::fs::create_dir_all(&d).expect("dir");
        // A name a shell would split or act on.
        let file = d.join("a b & echo hacked.txt");
        std::fs::write(&file, b"x").expect("file");
        let limits = Limits::default();
        #[cfg(windows)]
        let (ok, fails) = (
            "fc.exe /B {file} {file}",
            "fc.exe /B {file} {dir}\\missing.txt",
        );
        #[cfg(not(windows))]
        let (ok, fails) = ("test -f {file}", "test -f {dir}/missing.txt");
        let done = run_command(ok, &file, limits).await;
        assert!(done.ok, "{done:?}");
        let done = run_command(fails, &file, limits).await;
        assert!(!done.ok && done.count.is_some_and(|c| c != 0), "{done:?}");
        let done = run_command("no-such-program-anywhere {file}", &file, limits).await;
        assert!(
            !done.ok && done.detail.contains("could not run"),
            "{done:?}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[tokio::test]
    async fn a_command_that_runs_too_long_is_stopped() {
        let limits = Limits {
            command: std::time::Duration::from_millis(300),
            ..Limits::default()
        };
        #[cfg(windows)]
        let slow = "ping.exe -n 30 127.0.0.1";
        #[cfg(not(windows))]
        let slow = "sleep 30";
        let started = std::time::Instant::now();
        let done = run_command(slow, Path::new("."), limits).await;
        assert!(!done.ok && done.detail.contains("stopped"), "{done:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }
}
