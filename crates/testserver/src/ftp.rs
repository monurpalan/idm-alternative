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
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let root = root.clone();
            let mut builder = libunftp::ServerBuilder::with_authenticator(
                Box::new(move || unftp_sbe_fs::Filesystem::new(root.clone()).expect("folder")),
                Arc::new(Logins),
            )
            .passive_ports(40000..=49999);
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
