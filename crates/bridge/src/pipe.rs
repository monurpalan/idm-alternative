//! The local channel between the app and its clients (the native host, `shardload-mcp`): a named
//! pipe on Windows, a Unix socket in the data folder elsewhere. Both carry the same frames,
//! and every connection must present the app's token first.

use std::future::Future;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::{Error, Reply, Request, Result, read_json, same_token, write_json};

#[cfg(windows)]
pub use windows::{Conn, connect, serve};

#[cfg(unix)]
pub use unix::{Conn, connect, serve};

async fn answer<S, F, Fut>(mut conn: S, token: &str, handler: &F) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: Fn(Request) -> Fut,
    Fut: Future<Output = Reply>,
{
    match read_json::<Request, _>(&mut conn).await? {
        Some(Request::Hello { token: given }) if same_token(&given, token) => {
            write_json(&mut conn, &Reply::ok()).await?;
        }
        _ => {
            write_json(&mut conn, &Reply::error("refused")).await.ok();
            return Err(Error::Refused);
        }
    }
    while let Some(request) = read_json::<Request, _>(&mut conn).await? {
        let reply = handler(request).await;
        write_json(&mut conn, &reply).await?;
    }
    Ok(())
}

/// Says `hello` with `token` on a fresh connection.
async fn hello<S: AsyncRead + AsyncWrite + Unpin>(mut conn: S, token: &str) -> Result<S> {
    write_json(
        &mut conn,
        &Request::Hello {
            token: token.to_owned(),
        },
    )
    .await?;
    match read_json::<Reply, _>(&mut conn).await? {
        Some(r) if r.ok => Ok(conn),
        _ => Err(Error::Refused),
    }
}

fn spawn_answer<S, F, Fut>(conn: S, token: &Arc<str>, handler: &Arc<F>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Reply> + Send,
{
    let handler = Arc::clone(handler);
    let token = Arc::clone(token);
    tokio::spawn(async move {
        if let Err(e) = answer(conn, &token, &*handler).await {
            tracing::debug!("bridge connection ended: {e}");
        }
    });
}

#[cfg(windows)]
mod windows {
    use std::future::Future;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, ServerOptions};

    use crate::{Reply, Request, Result};

    /// A client's connection to the app.
    pub type Conn = NamedPipeClient;

    /// `ERROR_PIPE_BUSY`: every instance of the pipe is in use; another appears shortly.
    const PIPE_BUSY: i32 = 231;

    /// Answers connections on the pipe `name` until an error stops it (dropping the future
    /// stops it too). Each connection must say `hello` with `token` first; then every
    /// request gets the handler's reply.
    ///
    /// The first instance is created with `first_pipe_instance`, so if another process
    /// already owns the name this fails instead of sharing it, and remote clients are
    /// refused.
    pub async fn serve<F, Fut>(name: &str, token: String, handler: F) -> Result<()>
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Reply> + Send,
    {
        let handler = Arc::new(handler);
        let token: Arc<str> = token.into();
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(name)?;
        loop {
            server.connect().await?;
            let connected = server;
            // The next client gets a fresh instance.
            server = ServerOptions::new()
                .reject_remote_clients(true)
                .create(name)?;
            super::spawn_answer(connected, &token, &handler);
        }
    }

    /// Opens the pipe `name` and presents `token`. Fails at once (`NotFound`) when no app is
    /// listening; waits while all instances are busy.
    pub async fn connect(name: &str, token: &str) -> Result<Conn> {
        let client = loop {
            match ClientOptions::new().open(name) {
                Ok(c) => break c,
                Err(e) if e.raw_os_error() == Some(PIPE_BUSY) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => return Err(e.into()),
            }
        };
        super::hello(client, token).await
    }
}

#[cfg(unix)]
mod unix {
    use std::future::Future;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    use tokio::net::{UnixListener, UnixStream};

    use crate::{Reply, Request, Result};

    /// A client's connection to the app.
    pub type Conn = UnixStream;

    /// Answers connections on the socket at `name` (a path in the data folder) until an
    /// error stops it. A socket left by an app that is gone is replaced; the socket is
    /// readable by this user only, and each connection must say `hello` with `token` first.
    pub async fn serve<F, Fut>(name: &str, token: String, handler: F) -> Result<()>
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Reply> + Send,
    {
        let handler = Arc::new(handler);
        let token: Arc<str> = token.into();
        if UnixStream::connect(name).await.is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "another instance answers on the socket",
            )
            .into());
        }
        std::fs::remove_file(name).ok();
        let listener = UnixListener::bind(name)?;
        std::fs::set_permissions(name, std::fs::Permissions::from_mode(0o600))?;
        loop {
            let (conn, _) = listener.accept().await?;
            super::spawn_answer(conn, &token, &handler);
        }
    }

    /// Connects to the socket at `name` and presents `token`. Fails at once when no app is
    /// listening.
    pub async fn connect(name: &str, token: &str) -> Result<Conn> {
        let conn = UnixStream::connect(name).await?;
        super::hello(conn, token).await
    }
}
