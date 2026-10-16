//! The named pipe between the native host and the app (Windows).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};

use crate::{Error, Reply, Request, Result, read_json, same_token, write_json};

/// `ERROR_PIPE_BUSY`: every instance of the pipe is in use; another appears shortly.
const PIPE_BUSY: i32 = 231;

/// Answers connections on the pipe `name` until an error stops it (dropping the future
/// stops it too). Each connection must say `hello` with `token` first; then every request
/// gets the handler's reply.
///
/// The first instance is created with `first_pipe_instance`, so if another process already
/// owns the name this fails instead of sharing it, and remote clients are refused.
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
        let handler = Arc::clone(&handler);
        let token = Arc::clone(&token);
        tokio::spawn(async move {
            if let Err(e) = answer(connected, &token, &*handler).await {
                tracing::debug!("bridge connection ended: {e}");
            }
        });
    }
}

async fn answer<F, Fut>(mut conn: NamedPipeServer, token: &str, handler: &F) -> Result<()>
where
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

/// Opens the pipe `name` and presents `token`. Fails at once (`NotFound`) when no app is
/// listening; waits while all instances are busy.
pub async fn connect(name: &str, token: &str) -> Result<NamedPipeClient> {
    let mut client = loop {
        match ClientOptions::new().open(name) {
            Ok(c) => break c,
            Err(e) if e.raw_os_error() == Some(PIPE_BUSY) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(e.into()),
        }
    };
    write_json(
        &mut client,
        &Request::Hello {
            token: token.to_owned(),
        },
    )
    .await?;
    match read_json::<Reply, _>(&mut client).await? {
        Some(r) if r.ok => Ok(client),
        _ => Err(Error::Refused),
    }
}
