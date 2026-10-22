//! The client end of the pipe, for programs the user's tools start (the native messaging host,
//! `shardload-mcp`): connects to the running app, or starts it in the tray and waits for it, and
//! reconnects once when the app restarted since the last message.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{Conn, Reply, Request, connect, read_frame, write_frame};

/// How long to wait for a freshly started app to open its pipe.
const STARTUP_WAIT: Duration = Duration::from_secs(15);

type Result<T> = std::result::Result<T, String>;

/// The answer of [`Client::send_if_running`] when the app is not running.
pub const NOT_RUNNING: &str = "the app is not running";

pub struct Client {
    data: PathBuf,
    pipe: String,
    conn: Option<Conn>,
}

impl Client {
    /// For the app whose data folder is `data` ([`crate::data_dir`] for the real one).
    pub fn new(data: PathBuf) -> Self {
        let pipe = crate::pipe_name(&data);
        Self {
            data,
            pipe,
            conn: None,
        }
    }

    /// Sends one request and returns the app's reply.
    pub async fn request(&mut self, request: &Request) -> Result<Reply> {
        let frame = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        let reply = self.send(&frame).await?;
        serde_json::from_slice(&reply).map_err(|e| format!("bad reply: {e}"))
    }

    /// Sends one frame as it is and returns the reply's frame.
    pub async fn send(&mut self, frame: &[u8]) -> Result<Vec<u8>> {
        self.send_with(frame, true).await
    }

    /// Like [`Client::send`], but only to an app that is running: one that is not is left
    /// alone, and the answer is an error.
    pub async fn send_if_running(&mut self, frame: &[u8]) -> Result<Vec<u8>> {
        self.send_with(frame, false).await
    }

    async fn send_with(&mut self, frame: &[u8], launch: bool) -> Result<Vec<u8>> {
        // A connection kept from an earlier message may have closed (the app restarted): one
        // retry with a fresh connection.
        for attempt in 0..2 {
            let conn = match self.conn.as_mut() {
                Some(c) => c,
                None if launch => self.conn.insert(open(&self.data, &self.pipe).await?),
                None => self.conn.insert(
                    try_connect(&self.data, &self.pipe)
                        .await
                        .map_err(|_| NOT_RUNNING.to_owned())?,
                ),
            };
            let sent = write_frame(conn, frame).await;
            let reply = match sent {
                Ok(()) => read_frame(conn).await,
                Err(e) => Err(e),
            };
            match reply {
                Ok(Some(bytes)) => return Ok(bytes),
                Ok(None) | Err(_) if attempt == 0 => self.conn = None,
                Ok(None) => return Err("the app closed the connection".into()),
                Err(e) => return Err(e.to_string()),
            }
        }
        unreachable!()
    }
}

/// Connects to the running app, or starts it and waits until it listens.
async fn open(data: &Path, pipe: &str) -> Result<Conn> {
    if let Ok(c) = try_connect(data, pipe).await {
        return Ok(c);
    }
    launch_app()?;
    let started = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        match try_connect(data, pipe).await {
            Ok(c) => return Ok(c),
            Err(e) if started.elapsed() > STARTUP_WAIT => {
                return Err(format!("the app did not start in time: {e}"));
            }
            Err(_) => {}
        }
    }
}

async fn try_connect(data: &Path, pipe: &str) -> Result<Conn> {
    // Read on every attempt: a freshly started app writes a new token.
    let token = crate::read_token(data).map_err(|e| e.to_string())?;
    connect(pipe, &token).await.map_err(|e| e.to_string())
}

/// The app is installed next to the programs that use this; `SHARDLOAD_APP` overrides that
/// (tests, development).
fn app_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("SHARDLOAD_APP") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(exe.with_file_name(if cfg!(windows) {
        "shardload.exe"
    } else {
        "shardload"
    }))
}

/// Makes our stdin, stdout and stderr non-inheritable. Windows hands every inheritable
/// handle to a child process; the app would keep the caller's end of our stdout open long
/// after we exit, and the caller (a browser, an MCP client) would never see it close.
#[cfg(windows)]
#[allow(unsafe_code)]
fn keep_std_handles() {
    use windows_sys::Win32::Foundation::{
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle has no preconditions; SetHandleInformation only changes a flag
        // on a handle this process owns, and failure (e.g. no such handle) is harmless.
        unsafe {
            let h = GetStdHandle(which);
            if !h.is_null() && h != INVALID_HANDLE_VALUE {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

#[cfg(unix)]
fn launch_app() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let path = app_path()?;
    // Its own process group, and not our stdin/stdout: the caller waits for those to close,
    // and the app outlives us.
    std::process::Command::new(&path)
        .arg("--hidden")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(drop)
        .map_err(|e| format!("could not start the app ({}): {e}", path.display()))
}

#[cfg(windows)]
fn launch_app() -> Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let path = app_path()?;
    keep_std_handles();
    let start = |flags: u32| {
        std::process::Command::new(&path)
            .arg("--hidden")
            // Not our stdin/stdout: the caller waits for those to close, and the app outlives
            // us.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    // The caller may run us in a job that is closed with us; the app must outlive it.
    // Breaking away is not always allowed, so fall back to a plain detached start.
    start(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| start(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP))
        .map(drop)
        .map_err(|e| format!("could not start the app ({}): {e}", path.display()))
}
