//! Native messaging host. The browser starts it and talks to it over stdin/stdout; it passes
//! each message to the app over the named pipe and writes the app's reply back. When the app
//! is not running it starts it (in the tray) and waits for it.
//!
//! Errors go back to the extension as `{"ok": false, "error": ...}`, and to stderr, which the
//! browser keeps in its log.

use std::process::ExitCode;

use idma_bridge::{Client, Reply, Request, read_frame, write_frame};

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
    let mut app = Client::new(data);
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    // One frame in, one frame out, until the browser closes stdin.
    while let Some(frame) = read_frame(&mut stdin).await.map_err(|e| e.to_string())? {
        let reply = match forward(&mut app, &frame).await {
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

/// Sends one of the extension's messages to the app and returns its reply.
async fn forward(app: &mut Client, frame: &[u8]) -> Result<Vec<u8>> {
    let request: Request =
        serde_json::from_slice(frame).map_err(|e| format!("bad message: {e}"))?;
    if !request.from_browser() {
        // Hello carries the token, which only the host knows; listing, adding without
        // asking and removing are for the user's own tools.
        return Err("not accepted from the browser".into());
    }
    app.send(frame).await
}
