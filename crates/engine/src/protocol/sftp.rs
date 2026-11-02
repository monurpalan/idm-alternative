//! SFTP over SSH. The server's host key is checked against `~/.ssh/known_hosts` and, when the
//! download gives one, a known-hosts file of the program's own, where a server seen for the
//! first time is recorded; a key that changed is refused. Logs in with the password given, or
//! else with the user's usual keys (`~/.ssh/id_ed25519`, `id_ecdsa`, `id_rsa`, without a
//! passphrase).

use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use russh::client::{self, Handle};
use russh::keys;
use russh::keys::{PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate, known_hosts};
use russh_sftp::client::SftpSession;
use tokio::io::AsyncSeekExt;

use super::{Login, Reader, Stat};
use crate::error::{Error, Result};

pub(super) struct Sftp {
    host: String,
    port: u16,
    path: String,
    login: Login,
    user: String,
    known_hosts: Option<PathBuf>,
    insecure: bool,
}

impl Sftp {
    pub(super) fn new(
        url: &url::Url,
        login: Login,
        known_hosts: Option<PathBuf>,
        insecure: bool,
    ) -> Self {
        let user = match &login {
            Some((user, _)) => user.clone(),
            None => std::env::var("USERNAME")
                .or_else(|_| std::env::var("USER"))
                .unwrap_or_default(),
        };
        Self {
            host: url.host_str().unwrap_or_default().to_owned(),
            port: url.port().unwrap_or(22),
            path: super::path(url),
            login,
            user,
            known_hosts,
            insecure,
        }
    }

    pub(super) async fn stat(&self) -> Result<Stat> {
        let (_ssh, sftp) = self.session().await?;
        let meta = sftp
            .metadata(&self.path)
            .await
            .map_err(|e| self.failed(&e))?;
        Ok(Stat {
            size: meta.size,
            modified: meta.mtime.map(|t| t.to_string()),
            resumable: meta.size.is_some(),
        })
    }

    pub(super) async fn open(&self, offset: u64) -> Result<Reader> {
        let (ssh, sftp) = self.session().await?;
        let mut file = sftp.open(&self.path).await.map_err(|e| self.failed(&e))?;
        if offset > 0 {
            file.seek(SeekFrom::Start(offset)).await?;
        }
        Ok(Box::new(Open {
            file,
            _sftp: sftp,
            _ssh: ssh,
        }))
    }

    async fn session(&self) -> Result<(Handle<Client>, SftpSession)> {
        let refused = Arc::new(Mutex::new(None));
        let client = Client {
            host: self.host.clone(),
            port: self.port,
            known_hosts: self.known_hosts.clone(),
            insecure: self.insecure,
            refused: Arc::clone(&refused),
        };
        let config = Arc::new(client::Config::default());
        let mut ssh = match client::connect(config, (self.host.as_str(), self.port), client).await {
            Ok(ssh) => ssh,
            Err(e) => {
                let why = refused
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                return Err(Error::Remote(why.unwrap_or_else(|| format!("SSH: {e}"))));
            }
        };
        self.log_in(&mut ssh).await?;
        let channel = ssh.channel_open_session().await.map_err(ssh_error)?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(ssh_error)?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| Error::Remote(format!("SFTP: {e}")))?;
        Ok((ssh, sftp))
    }

    async fn log_in(&self, ssh: &mut Handle<Client>) -> Result<()> {
        if let Some((user, password)) = &self.login {
            let done = ssh
                .authenticate_password(user, password)
                .await
                .map_err(ssh_error)?;
            return if done.success() {
                Ok(())
            } else {
                Err(Error::AuthRequired { realm: None })
            };
        }
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
        let keys = ["id_ed25519", "id_ecdsa", "id_rsa"]
            .iter()
            .filter_map(|name| Some(PathBuf::from(home.clone()?).join(".ssh").join(name)))
            .filter(|p| p.is_file());
        for path in keys {
            let Ok(key) = keys::load_secret_key(&path, None) else {
                continue; // a passphrase, or a kind of key that cannot be read
            };
            let hash = ssh.best_supported_rsa_hash().await.ok().flatten().flatten();
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
            if ssh
                .authenticate_publickey(&self.user, key)
                .await
                .map_err(ssh_error)?
                .success()
            {
                return Ok(());
            }
        }
        Err(Error::AuthRequired { realm: None })
    }

    fn failed(&self, e: &russh_sftp::client::error::Error) -> Error {
        use russh_sftp::client::error::Error as E;
        use russh_sftp::protocol::StatusCode;
        match e {
            E::Status(s) if s.status_code == StatusCode::NoSuchFile => {
                Error::NotFound(self.path.clone())
            }
            _ => Error::Remote(format!("SFTP: {e}")),
        }
    }
}

fn ssh_error(e: russh::Error) -> Error {
    Error::Remote(format!("SSH: {e}"))
}

/// Checks the server's host key.
struct Client {
    host: String,
    port: u16,
    known_hosts: Option<PathBuf>,
    insecure: bool,
    /// Why the key was refused, for the message.
    refused: Arc<Mutex<Option<String>>>,
}

impl Client {
    fn trusted(&self, key: &PublicKey) -> std::result::Result<bool, String> {
        let changed = |e: keys::Error| match e {
            keys::Error::KeyChanged { line } => format!(
                "the host key of {} changed (known hosts, line {line}): refused, it may not be \
                 the same server",
                self.host
            ),
            e => format!("the host key of {} could not be checked: {e}", self.host),
        };
        match known_hosts::check_known_hosts(&self.host, self.port, key) {
            Ok(true) => return Ok(true),
            Ok(false) | Err(keys::Error::NoHomeDir | keys::Error::IO(_)) => {}
            Err(e) => return Err(changed(e)),
        }
        let Some(path) = &self.known_hosts else {
            return Err(format!(
                "the host key of {} is not in ~/.ssh/known_hosts",
                self.host
            ));
        };
        match known_hosts::check_known_hosts_path(&self.host, self.port, key, path) {
            Ok(true) => Ok(true),
            // Seen for the first time: trusted from now on.
            Ok(false) | Err(keys::Error::IO(_)) => {
                known_hosts::learn_known_hosts_path(&self.host, self.port, key, path)
                    .map_err(changed)?;
                Ok(true)
            }
            Err(e) => Err(changed(e)),
        }
    }
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        if self.insecure {
            return Ok(true);
        }
        let verdict = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => self.trusted(key),
            PublicKeyOrCertificate::Certificate(_) => {
                Err("host certificates are not supported".to_owned())
            }
        };
        match verdict {
            Ok(trusted) => Ok(trusted),
            Err(why) => {
                *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = Some(why);
                Ok(false)
            }
        }
    }
}

/// An open file, with the session it was opened in (dropped together).
struct Open {
    file: russh_sftp::client::fs::File,
    _sftp: SftpSession,
    _ssh: Handle<Client>,
}

impl tokio::io::AsyncRead for Open {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().file).poll_read(cx, buf)
    }
}
