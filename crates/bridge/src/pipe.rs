//! The local channel between the app and its clients (the native host, `shardload-mcp`): a named
//! pipe on Windows, a Unix socket in the data folder elsewhere. Both carry the same frames,
//! and every connection must present the app's token first.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};

use crate::{Error, Reply, Request, Result, read_json, same_token, write_json};

#[cfg(windows)]
pub use windows::{Conn, Listener, connect, listen};

#[cfg(unix)]
pub use unix::{Conn, Listener, connect, listen};

/// Takes the channel `name` ([`listen`]) and answers connections on it ([`Listener::serve`]).
/// Returns only when the channel could not be taken; dropping the future stops it.
pub async fn serve<F, Fut>(name: &str, token: String, handler: F) -> Result<()>
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Reply> + Send,
{
    listen(name).await?.serve(token, handler).await;
    Ok(())
}

/// The pause after a failed accept, doubling up to [`LONGEST_PAUSE`] while it keeps failing.
const FIRST_PAUSE: Duration = Duration::from_millis(50);
const LONGEST_PAUSE: Duration = Duration::from_secs(5);

/// Where connections come from: the pipe, the socket, a script in tests.
trait Accept {
    type Conn;
    fn accept(&mut self) -> impl Future<Output = std::io::Result<Self::Conn>> + Send;
}

/// Hands each accepted connection to `take`, forever: an error accepting one (no handles
/// left for a moment, say) is logged and tried again after a pause, instead of leaving the
/// app unreachable until it restarts.
async fn accept_loop<A: Accept>(from: &mut A, mut take: impl FnMut(A::Conn)) -> ! {
    let mut pause = FIRST_PAUSE;
    loop {
        match from.accept().await {
            Ok(conn) => {
                pause = FIRST_PAUSE;
                take(conn);
            }
            Err(e) => {
                tracing::warn!(
                    "bridge: could not accept a connection, trying again in {pause:?}: {e}"
                );
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(LONGEST_PAUSE);
            }
        }
    }
}

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
        let reply = if request.ends_with_client() {
            let work = handler(request);
            tokio::pin!(work);
            let mut byte = [0u8; 1];
            tokio::select! {
                reply = &mut work => reply,
                // A client waits for its reply: the connection ending (or anything else
                // arriving) means it is gone, and the work with it.
                _ = conn.read(&mut byte) => return Ok(()),
            }
        } else {
            handler(request).await
        };
        write_json(&mut conn, &reply).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// Sets its flag when dropped: the work it sits in was given up.
    struct Dropped(Arc<AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn a_grab_ends_when_its_client_goes_away() {
        let (server, mut client) = tokio::io::duplex(1024);
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&dropped);
        let handler = move |_: Request| {
            let guard = Dropped(Arc::clone(&flag));
            async move {
                let _guard = guard;
                std::future::pending::<Reply>().await
            }
        };
        let answering = tokio::spawn(async move { answer(server, "t", &handler).await });
        let hello = Request::Hello { token: "t".into() };
        write_json(&mut client, &hello).await.expect("hello");
        let ok: Reply = read_json(&mut client).await.expect("read").expect("reply");
        assert!(ok.ok);
        let grab = Request::Grab {
            url: "https://example.org/".into(),
            depth: None,
            same_site: None,
        };
        write_json(&mut client, &grab).await.expect("grab");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!dropped.load(Ordering::SeqCst), "still waited for");
        drop(client);
        tokio::time::timeout(std::time::Duration::from_secs(5), answering)
            .await
            .expect("answer ended")
            .expect("join")
            .expect("clean end");
        assert!(dropped.load(Ordering::SeqCst));
    }

    /// Accepts as scripted, then waits forever.
    struct Script(std::collections::VecDeque<std::io::Result<u32>>);

    impl Accept for Script {
        type Conn = u32;
        async fn accept(&mut self) -> std::io::Result<u32> {
            match self.0.pop_front() {
                Some(next) => next,
                None => std::future::pending().await,
            }
        }
    }

    #[tokio::test]
    async fn a_failed_accept_is_tried_again() {
        let failed = || Err(std::io::Error::other("no handles left"));
        let mut script = Script([failed(), failed(), Ok(1), failed(), Ok(2)].into());
        let (taken, mut took) = tokio::sync::mpsc::unbounded_channel();
        let looping = tokio::spawn(async move {
            accept_loop(&mut script, |conn| taken.send(conn).expect("send")).await
        });
        for expected in [1, 2] {
            let conn = tokio::time::timeout(Duration::from_secs(5), took.recv())
                .await
                .expect("in time");
            assert_eq!(conn, Some(expected));
        }
        looping.abort();
    }

    /// A second app on the same data folder cannot take the channel, and the running app's
    /// token stays on disk for its clients.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_app_leaves_the_running_apps_token() {
        let dir = std::env::temp_dir().join(format!("shardload-second-app-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let (running, token) = crate::listen_with_token(&dir).await.expect("first app");
        tokio::spawn(running.serve(token, |_| async { Reply::ok() }));
        assert!(
            crate::listen_with_token(&dir).await.is_err(),
            "the name is taken"
        );
        let token = crate::read_token(&dir).expect("token");
        connect(&crate::pipe_name(&dir), &token)
            .await
            .expect("the running app accepts the token on disk");
        std::fs::remove_dir_all(&dir).ok();
    }
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

    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    use crate::{Reply, Request, Result};

    /// A client's connection to the app.
    pub type Conn = NamedPipeClient;

    /// `ERROR_PIPE_BUSY`: every instance of the pipe is in use; another appears shortly.
    const PIPE_BUSY: i32 = 231;

    /// The app's end of the pipe, taken by [`listen`].
    pub struct Listener {
        name: String,
        /// The instance the next client connects to.
        listening: Option<NamedPipeServer>,
    }

    /// Takes the pipe `name`. The first instance is created with `first_pipe_instance`, so
    /// if another process already owns the name this fails instead of sharing it; remote
    /// clients are refused. Async like its Unix twin; needs a Tokio runtime.
    #[allow(clippy::unused_async)]
    pub async fn listen(name: &str) -> Result<Listener> {
        let first = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(name)?;
        Ok(Listener {
            name: name.to_owned(),
            listening: Some(first),
        })
    }

    impl Listener {
        /// Answers connections until dropped. Each connection must say `hello` with `token`
        /// first; then every request gets the handler's reply.
        pub async fn serve<F, Fut>(mut self, token: String, handler: F)
        where
            F: Fn(Request) -> Fut + Send + Sync + 'static,
            Fut: Future<Output = Reply> + Send,
        {
            let handler = Arc::new(handler);
            let token: Arc<str> = token.into();
            super::accept_loop(&mut self, |conn| {
                super::spawn_answer(conn, &token, &handler);
            })
            .await
        }

        fn instance(&self) -> std::io::Result<NamedPipeServer> {
            ServerOptions::new()
                .reject_remote_clients(true)
                .create(&self.name)
        }
    }

    impl super::Accept for Listener {
        type Conn = NamedPipeServer;

        async fn accept(&mut self) -> std::io::Result<NamedPipeServer> {
            let server = match self.listening.take() {
                Some(s) => s,
                None => self.instance()?,
            };
            server.connect().await?;
            // The next client gets a fresh instance, made before this one is handed over so
            // that the name always has one; when it cannot be made now, the next accept
            // tries again.
            self.listening = self
                .instance()
                .inspect_err(|e| tracing::warn!("bridge: no pipe for the next client yet: {e}"))
                .ok();
            Ok(server)
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

    /// The app's end of the socket, taken by [`listen`].
    pub struct Listener(UnixListener);

    /// Takes the socket at `name` (a path in the data folder): fails when another instance
    /// answers on it, replaces one left by an app that is gone. The socket is readable by
    /// this user only.
    pub async fn listen(name: &str) -> Result<Listener> {
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
        Ok(Listener(listener))
    }

    impl Listener {
        /// Answers connections until dropped. Each connection must say `hello` with `token`
        /// first; then every request gets the handler's reply.
        pub async fn serve<F, Fut>(mut self, token: String, handler: F)
        where
            F: Fn(Request) -> Fut + Send + Sync + 'static,
            Fut: Future<Output = Reply> + Send,
        {
            let handler = Arc::new(handler);
            let token: Arc<str> = token.into();
            super::accept_loop(&mut self, |conn| {
                super::spawn_answer(conn, &token, &handler);
            })
            .await
        }
    }

    impl super::Accept for Listener {
        type Conn = UnixStream;

        async fn accept(&mut self) -> std::io::Result<UnixStream> {
            self.0.accept().await.map(|(conn, _)| conn)
        }
    }

    /// Connects to the socket at `name` and presents `token`. Fails at once when no app is
    /// listening.
    pub async fn connect(name: &str, token: &str) -> Result<Conn> {
        let conn = UnixStream::connect(name).await?;
        super::hello(conn, token).await
    }
}
