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
        // A connection kept from an earlier message may have closed (the app restarted):
        // writing to it fails, so the app never saw the frame, and it goes again on a fresh
        // connection. Once the frame was written the app may have acted on it, so only a
        // request that changes nothing is sent again.
        let repeatable = changes_nothing(frame);
        let mut attempt = 0;
        loop {
            attempt += 1;
            let conn = match self.conn.as_mut() {
                Some(c) => c,
                None if launch => self.conn.insert(open(&self.data, &self.pipe).await?),
                None => self.conn.insert(
                    try_connect(&self.data, &self.pipe)
                        .await
                        .map_err(|_| NOT_RUNNING.to_owned())?,
                ),
            };
            let failed = match exchange(conn, frame).await {
                Ok(bytes) => return Ok(bytes),
                Err(failed) => failed,
            };
            self.conn = None;
            let delivered = matches!(failed, Failed::AfterWriting(_));
            if attempt == 2 || (delivered && !repeatable) {
                return Err(failed.into_message());
            }
        }
    }
}

/// How an exchange on a connection failed: before the whole frame was written (the app
/// cannot have acted on it), or after (it may have).
enum Failed {
    Writing(String),
    AfterWriting(String),
}

impl Failed {
    fn into_message(self) -> String {
        match self {
            Self::Writing(e) | Self::AfterWriting(e) => e,
        }
    }
}

async fn exchange(conn: &mut Conn, frame: &[u8]) -> std::result::Result<Vec<u8>, Failed> {
    write_frame(conn, frame)
        .await
        .map_err(|e| Failed::Writing(e.to_string()))?;
    match read_frame(conn).await {
        Ok(Some(bytes)) => Ok(bytes),
        Ok(None) => Err(Failed::AfterWriting(
            "the app closed the connection before answering".into(),
        )),
        Err(e) => Err(Failed::AfterWriting(e.to_string())),
    }
}

/// Whether the app only answers this request, so that it may be sent again without the
/// app doing something twice. A frame that does not parse is not.
fn changes_nothing(frame: &[u8]) -> bool {
    use Request as R;
    matches!(
        serde_json::from_slice::<Request>(frame),
        Ok(R::Ping
            // Says which extension this is; saying it again records the same.
            | R::Extension(_)
            | R::MediaQualities(_)
            | R::Tasks
            | R::Grab { .. }
            | R::InspectVideo { .. }
            | R::InspectTorrent { .. }
            | R::InspectArchive { .. }
            | R::Wait { .. }
            | R::Sites
            | R::DiskSpace
            | R::Network
            | R::Deadlines
            | R::FileOrigin { .. })
    )
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncRead, AsyncWrite};

    use super::*;
    use crate::{Reply, Request, read_json, write_json};

    const DOWNLOAD: &[u8] = br#"{"type":"download","url":"https://example.com/a.zip"}"#;
    const PING: &[u8] = br#"{"type":"ping"}"#;

    /// A data folder of its own: the pipe name and token follow from it.
    struct Profile(PathBuf);

    impl Profile {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join("shardload-client-tests")
                .join(format!("{name}-{}", std::process::id()));
            std::fs::remove_dir_all(&dir).ok();
            std::fs::create_dir_all(&dir).expect("create");
            Self(dir)
        }
    }

    impl Drop for Profile {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[cfg(windows)]
    struct Listener {
        name: String,
        next: tokio::net::windows::named_pipe::NamedPipeServer,
    }

    #[cfg(windows)]
    impl Listener {
        fn bind(name: &str) -> Self {
            let next = tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true)
                .create(name)
                .expect("pipe");
            Self {
                name: name.to_owned(),
                next,
            }
        }

        async fn accept(&mut self) -> tokio::net::windows::named_pipe::NamedPipeServer {
            self.next.connect().await.expect("connect");
            let fresh = tokio::net::windows::named_pipe::ServerOptions::new()
                .create(&self.name)
                .expect("pipe");
            std::mem::replace(&mut self.next, fresh)
        }
    }

    #[cfg(unix)]
    struct Listener(tokio::net::UnixListener);

    #[cfg(unix)]
    impl Listener {
        fn bind(name: &str) -> Self {
            std::fs::remove_file(name).ok();
            Self(tokio::net::UnixListener::bind(name).expect("socket"))
        }

        async fn accept(&mut self) -> tokio::net::UnixStream {
            self.0.accept().await.expect("accept").0
        }
    }

    /// Answers `hello`, then counts each request; the first connection is dropped as soon as
    /// its first request arrived (the app went away before answering), later ones are answered.
    async fn talk<S: AsyncRead + AsyncWrite + Unpin>(
        mut conn: S,
        token: &str,
        drop_first: bool,
        received: &AtomicUsize,
    ) {
        match read_json::<Request, _>(&mut conn).await {
            Ok(Some(Request::Hello { token: t })) if t == token => {}
            _ => return,
        }
        write_json(&mut conn, &Reply::ok()).await.ok();
        while let Ok(Some(_)) = read_frame(&mut conn).await {
            received.fetch_add(1, Ordering::SeqCst);
            if drop_first {
                return;
            }
            write_json(&mut conn, &Reply::ok()).await.ok();
        }
    }

    /// Stands in for an app that loses the first connection after its first request.
    fn flaky_app(dir: &Path) -> Arc<AtomicUsize> {
        let token = crate::new_token(dir).expect("token");
        let mut listener = Listener::bind(&crate::pipe_name(dir));
        let received = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&received);
        tokio::spawn(async move {
            let mut first = true;
            loop {
                let conn = listener.accept().await;
                let (token, count) = (token.clone(), Arc::clone(&count));
                let drop_first = std::mem::take(&mut first);
                tokio::spawn(async move { talk(conn, &token, drop_first, &count).await });
            }
        });
        received
    }

    async fn in_time<T>(work: impl Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(20), work)
            .await
            .expect("in time")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_download_the_app_may_have_received_is_not_sent_again() {
        let p = Profile::new("once");
        let received = flaky_app(&p.0);
        let mut client = Client::new(p.0.clone());
        let sent = in_time(client.send_if_running(DOWNLOAD)).await;
        assert!(sent.is_err(), "{sent:?}");
        assert_eq!(received.load(Ordering::SeqCst), 1, "sent once");
        // The next request goes out on a fresh connection.
        in_time(client.send_if_running(PING)).await.expect("ping");
        assert_eq!(received.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_request_that_changes_nothing_is_asked_again() {
        let p = Profile::new("ask-again");
        let received = flaky_app(&p.0);
        let mut client = Client::new(p.0.clone());
        in_time(client.send_if_running(PING)).await.expect("ping");
        assert_eq!(received.load(Ordering::SeqCst), 2);
    }

    /// The real server in a runtime of its own, which ends (closing every connection) when
    /// dropped: an app that quits.
    fn app_in_own_runtime(dir: &Path, downloads: &Arc<AtomicUsize>) -> tokio::runtime::Runtime {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        serve_counting(&rt, dir, downloads);
        rt
    }

    /// Waits until the server started for `dir` has taken the pipe and accepts its token.
    async fn answering(dir: &Path) {
        let pipe = crate::pipe_name(dir);
        let token = crate::read_token(dir).expect("token");
        in_time(async {
            while crate::connect(&pipe, &token).await.is_err() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
    }

    fn serve_counting(rt: &tokio::runtime::Runtime, dir: &Path, downloads: &Arc<AtomicUsize>) {
        let token = crate::new_token(dir).expect("token");
        let pipe = crate::pipe_name(dir);
        let count = Arc::clone(downloads);
        rt.spawn(async move {
            crate::serve(&pipe, token, move |request| {
                if matches!(request, Request::Download(_)) {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                async { Reply::ok() }
            })
            .await
            .ok();
        });
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_kept_connection_to_an_app_that_restarted_is_replaced() {
        let p = Profile::new("restart");
        let downloads = Arc::new(AtomicUsize::new(0));
        let before = app_in_own_runtime(&p.0, &downloads);
        answering(&p.0).await;
        let mut client = Client::new(p.0.clone());
        in_time(client.send_if_running(PING)).await.expect("ping");
        tokio::task::spawn_blocking(move || drop(before))
            .await
            .expect("quit");
        let after = app_in_own_runtime(&p.0, &downloads);
        answering(&p.0).await;
        let reply = in_time(client.send_if_running(DOWNLOAD)).await;
        assert!(reply.is_ok(), "{reply:?}");
        assert_eq!(downloads.load(Ordering::SeqCst), 1);
        tokio::task::spawn_blocking(move || drop(after))
            .await
            .expect("quit");
    }
}
