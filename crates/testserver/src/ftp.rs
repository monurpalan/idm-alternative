//! An FTP server over a folder (libunftp), for testing FTP downloads: on a free port of
//! 127.0.0.1, anonymous or as [`USER`] with [`PASSWORD`], and with TLS (`AUTH TLS`, a
//! self-signed certificate) when asked. [`file`] puts the usual seeded content in the folder.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use libunftp::auth::AnonymousAuthenticator;
use tokio::net::TcpListener;
use unftp_core::auth::{AuthenticationError, Authenticator, Credentials, Principal};

pub const USER: &str = "user";
pub const PASSWORD: &str = "secret";

/// Writes `size` bytes of seed `seed`'s content (see [`crate::content`]) to `dir/name`.
pub fn file(dir: &Path, name: &str, size: u64, seed: u64) -> std::io::Result<PathBuf> {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut data = vec![0u8; size as usize];
    crate::content::fill(seed, 0, &mut data);
    std::fs::write(&path, data)?;
    Ok(path)
}

/// Anonymous users are let in; [`USER`] only with [`PASSWORD`].
#[derive(Debug)]
struct Logins;

#[async_trait::async_trait]
impl Authenticator for Logins {
    async fn authenticate(
        &self,
        username: &str,
        creds: &Credentials,
    ) -> Result<Principal, AuthenticationError> {
        if username == USER && creds.password.as_deref() != Some(PASSWORD) {
            return Err(AuthenticationError::BadPassword);
        }
        AnonymousAuthenticator.authenticate(username, creds).await
    }
}

/// Opens each passive data port where the system says, on a port no other socket holds.
/// libunftp's own picks one at random from a range with `SO_REUSEADDR`, which on Windows lets
/// two servers (two tests running at once) hold the same port: a client then reaches the
/// other test's server, a plain one where TLS was expected, or none that answers.
#[derive(Debug)]
struct FreePort;

#[async_trait::async_trait]
impl libunftp::options::Binder for FreePort {
    async fn bind(
        &mut self,
        local_addr: std::net::IpAddr,
        _passive_ports: std::ops::RangeInclusive<u16>,
    ) -> std::io::Result<tokio::net::TcpSocket> {
        let socket = match local_addr {
            std::net::IpAddr::V4(_) => tokio::net::TcpSocket::new_v4()?,
            std::net::IpAddr::V6(_) => tokio::net::TcpSocket::new_v6()?,
        };
        socket.bind(std::net::SocketAddr::new(local_addr, 0))?;
        Ok(socket)
    }
}

/// Passes a control connection between `client` and libunftp at `server`, holding what the
/// client sends for a moment after libunftp says `234` to `AUTH TLS`. libunftp answers at
/// once but switches to TLS only once a task it spawned has told its loop to; a quick
/// client's TLS hello read before that is taken for a command, answered in plain text, and
/// the client's TLS fails ("received corrupt message of type InvalidContentType").
async fn hold_after_auth(client: tokio::net::TcpStream, server: std::net::SocketAddr) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Ok(upstream) = tokio::net::TcpStream::connect(server).await else {
        return;
    };
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = upstream.into_split();
    let hold: Arc<std::sync::Mutex<Option<tokio::time::Instant>>> = Arc::default();
    let set = Arc::clone(&hold);
    let down = async move {
        let mut buf = vec![0u8; 16 << 10];
        while let Ok(n) = server_read.read(&mut buf).await {
            let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
                break;
            };
            if chunk.starts_with(b"234 ") || chunk.windows(5).any(|w| w == b"\n234 ") {
                let until = tokio::time::Instant::now() + std::time::Duration::from_millis(200);
                *set.lock().expect("hold") = Some(until);
            }
            if client_write.write_all(chunk).await.is_err() {
                break;
            }
        }
        client_write.shutdown().await.ok();
    };
    let up = async move {
        let mut buf = vec![0u8; 16 << 10];
        while let Ok(n) = client_read.read(&mut buf).await {
            let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
                break;
            };
            let until = hold.lock().expect("hold").take();
            if let Some(until) = until {
                tokio::time::sleep_until(until).await;
            }
            if server_write.write_all(chunk).await.is_err() {
                break;
            }
        }
        server_write.shutdown().await.ok();
    };
    tokio::join!(down, up);
}

/// Serves `root` until the runtime ends; returns the port. With `tls`, the certificate and key
/// are written next to `root` (`root.pem`, `root.key`).
pub async fn serve(root: PathBuf, tls: bool) -> anyhow::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let certs = if tls {
        let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(vec![
            "localhost".to_owned(),
            "127.0.0.1".to_owned(),
        ])?;
        let (pem, key) = (root.with_extension("pem"), root.with_extension("key"));
        std::fs::write(&pem, cert.pem())?;
        std::fs::write(&key, signing_key.serialize_pem())?;
        Some((pem, key))
    } else {
        None
    };
    // libunftp listens on its own port; clients come through `hold_after_auth`.
    let inner = TcpListener::bind("127.0.0.1:0").await?;
    let inner_addr = inner.local_addr()?;
    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            tokio::spawn(hold_after_auth(client, inner_addr));
        }
    });
    let listener = inner;
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let root = root.clone();
            let mut builder = libunftp::ServerBuilder::with_authenticator(
                Box::new(move || unftp_sbe_fs::Filesystem::new(root.clone()).expect("folder")),
                Arc::new(Logins),
            )
            .binder(FreePort);
            if let Some((pem, key)) = &certs {
                builder = builder.ftps(pem.clone(), key.clone());
            }
            if let Ok(server) = builder.build() {
                tokio::spawn(server.service(stream));
            }
        }
    });
    Ok(port)
}
