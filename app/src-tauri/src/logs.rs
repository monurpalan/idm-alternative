//! The log file and crash log.

use super::*;

/// Passed when Windows starts the app at sign-in: stay in the tray.
pub(crate) const HIDDEN_ARG: &str = "--hidden";

/// Keeps panics in `crash.log` in the data folder: a release build has no console, and a
/// browser-started app has nowhere else to say why it stopped.
pub(crate) fn log_panics() {
    let Some(dir) = shardload_bridge::data_dir() else {
        return;
    };
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;
        let when = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let backtrace = std::backtrace::Backtrace::force_capture();
        if std::fs::create_dir_all(&dir).is_ok()
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("crash.log"))
        {
            let _ = writeln!(
                f,
                "[{when}] {info}
{backtrace}"
            );
        }
        default(info);
    }));
}

/// The app's log: `<data>/logs/shardload.log.<date>`, a file a day, the last week kept, and stderr
/// for a console. `SHARDLOAD_LOG` sets the level (default `info`). The guard writes out what is
/// still buffered when the app ends.
pub(crate) fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_env("SHARDLOAD_LOG")
            .unwrap_or_else(|_| "info".into())
    };
    let console = tracing_subscriber::fmt::layer().with_filter(filter());
    let file = shardload_bridge::data_dir().and_then(|data| {
        tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("shardload")
            .filename_suffix("log")
            .max_log_files(7)
            .build(data.join(LOG_DIR))
            .ok()
    });
    match file {
        Some(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let file = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter());
            tracing_subscriber::registry()
                .with(console)
                .with(file)
                .init();
            Some(guard)
        }
        None => {
            tracing_subscriber::registry().with(console).init();
            None
        }
    }
}

/// Inside the data folder.
pub(crate) const LOG_DIR: &str = "logs";

/// Opens the folder with the app's logs (and `crash.log` beside it, one level up).
#[tauri::command]
pub(crate) fn open_logs(handle: AppHandle, app: State<'_, App>) -> CmdResult<()> {
    let dir = app.data.join(LOG_DIR);
    std::fs::create_dir_all(&dir).map_err(msg)?;
    handle
        .opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(msg)
}
