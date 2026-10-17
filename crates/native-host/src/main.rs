//! Native messaging host. The browser starts it and talks to it over stdin/stdout; it passes
//! each message to the app over the named pipe and writes the app's reply back. When the app
//! is not running it starts it (in the tray) and waits for it.
//!
//! Errors go back to the extension as `{"ok": false, "error": ...}`, and to stderr, which the
//! browser keeps in its log.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use idma_bridge::{Reply, Request, connect, read_frame, write_frame};
use tokio::net::windows::named_pipe::NamedPipeClient;

/// How long to wait for a freshly started app to open its pipe.
const STARTUP_WAIT: Duration = Duration::from_secs(15);

type Result<T> = std::result::Result<T, String>;

fn main() -> ExitCode {
    // The uninstaller runs `idma-host --unregister` to take our keys out of the browsers.
    // Browsers start the host with the extension's origin or id as the argument instead.
    if std::env::args().nth(1).as_deref() == Some("--unregister") {
        let Some(data) = idma_bridge::data_dir() else {
            eprintln!("idma-host: APPDATA is not set");
            return ExitCode::FAILURE;
        };
        return match idma_bridge::unregister(&data) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("idma-host: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("idma-host: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("idma-host: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let data = idma_bridge::data_dir().ok_or("APPDATA is not set")?;
    let pipe = idma_bridge::pipe_name(&data);
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut app: Option<NamedPipeClient> = None;
    // One frame in, one frame out, until the browser closes stdin.
    while let Some(frame) = read_frame(&mut stdin).await.map_err(|e| e.to_string())? {
        let reply = match forward(&data, &pipe, &mut app, &frame).await {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("idma-host: {e}");
                serde_json::to_vec(&Reply::error(e)).map_err(|e| e.to_string())?
            }
        };
        write_frame(&mut stdout, &reply)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Sends one message to the app and returns its reply, connecting (and starting the app)
/// when needed.
async fn forward(
    data: &Path,
    pipe: &str,
    app: &mut Option<NamedPipeClient>,
    frame: &[u8],
) -> Result<Vec<u8>> {
    let request: Request =
        serde_json::from_slice(frame).map_err(|e| format!("bad message: {e}"))?;
    if matches!(request, Request::Hello { .. }) {
        // Only the host knows the token; the extension never sends one.
        return Err("unexpected hello".into());
    }
    // A connection kept from an earlier message may have closed (the app restarted): one
    // retry with a fresh connection.
    for attempt in 0..2 {
        if app.is_none() {
            *app = Some(open(data, pipe).await?);
        }
        let Some(conn) = app.as_mut() else {
            unreachable!()
        };
        let sent = write_frame(conn, frame).await;
        let reply = match sent {
            Ok(()) => read_frame(conn).await,
            Err(e) => Err(e),
        };
        match reply {
            Ok(Some(bytes)) => return Ok(bytes),
            Ok(None) | Err(_) if attempt == 0 => *app = None,
            Ok(None) => return Err("the app closed the connection".into()),
            Err(e) => return Err(e.to_string()),
        }
    }
    unreachable!()
}

/// Connects to the running app, or starts it and waits until it listens.
async fn open(data: &Path, pipe: &str) -> Result<NamedPipeClient> {
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

async fn try_connect(data: &Path, pipe: &str) -> Result<NamedPipeClient> {
    // Read on every attempt: a freshly started app writes a new token.
    let token = idma_bridge::read_token(data).map_err(|e| e.to_string())?;
    connect(pipe, &token).await.map_err(|e| e.to_string())
}

/// The app is installed next to the host; `IDMA_APP` overrides that (tests, development).
fn app_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("IDMA_APP") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(exe.with_file_name("idm-alternative.exe"))
}

/// Makes the browser's pipes (our stdin, stdout, stderr) non-inheritable. Windows hands
/// every inheritable handle to a child process; the app would keep the browser's end of
/// our stdout open long after we exit, and the browser would never see it close.
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
            // Not the host's stdin/stdout: the browser waits for those to close, and the app
            // outlives the host.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    // The browser may run its hosts in a job that is closed with them; the app must outlive
    // the host. Breaking away is not always allowed, so fall back to a plain detached start.
    start(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| start(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP))
        .map(drop)
        .map_err(|e| format!("could not start the app ({}): {e}", path.display()))
}
