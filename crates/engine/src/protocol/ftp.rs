//! FTP, and FTP over TLS: implicit (`ftps://`, port 990) and explicit (`ftpes://`: `AUTH TLS` on
//! port 21), as curl and FileZilla name them. Every part of a download has a control connection
//! of its own: `REST` to where it starts, `RETR`, and the connection is dropped once the part
//! has what it needs.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use suppaftp::tokio::{
    AsyncFtpStream, AsyncRustlsConnector, AsyncRustlsFtpStream, ImplAsyncFtpStream, TokioTlsStream,
    TransferStream,
};
use suppaftp::types::FileType;
use suppaftp::{FtpError, Status};
use tokio::io::{AsyncRead, ReadBuf};

use super::{Login, Reader, Stat};
use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tls {
    None,
    Implicit,
    Explicit,
}

pub(super) struct Ftp {
    host: String,
    port: u16,
    path: String,
    login: Login,
    tls: Tls,
    insecure: bool,
}

impl Ftp {
    pub(super) fn new(url: &url::Url, login: Login, insecure: bool) -> Self {
        let tls = match url.scheme() {
            "ftps" => Tls::Implicit,
            "ftpes" => Tls::Explicit,
            _ => Tls::None,
        };
        let port = url
            .port()
            .unwrap_or(if tls == Tls::Implicit { 990 } else { 21 });
        Self {
            host: url.host_str().unwrap_or_default().to_owned(),
            port,
            path: super::path(url),
            login,
            tls,
            insecure,
        }
    }

    pub(super) async fn stat(&self) -> Result<Stat> {
        match self.tls {
            Tls::None => stat(self, self.plain().await?).await,
            _ => stat(self, self.secure().await?).await,
        }
    }

    pub(super) async fn open(&self, offset: u64) -> Result<Reader> {
        match self.tls {
            Tls::None => open(self, self.plain().await?, offset).await,
            _ => open(self, self.secure().await?, offset).await,
        }
    }

    async fn plain(&self) -> Result<AsyncFtpStream> {
        let mut ftp = AsyncFtpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(remote)?;
        self.log_in(&mut ftp).await?;
        Ok(ftp)
    }

    async fn secure(&self) -> Result<AsyncRustlsFtpStream> {
        let config = super::tls_config(self.insecure)?;
        let connector =
            AsyncRustlsConnector::from(tokio_rustls::TlsConnector::from(Arc::new(config)));
        let addr = (self.host.as_str(), self.port);
        let mut ftp = if self.tls == Tls::Implicit {
            AsyncRustlsFtpStream::connect_secure_implicit(addr, connector, &self.host)
                .await
                .map_err(remote)?
        } else {
            AsyncRustlsFtpStream::connect(addr)
                .await
                .map_err(remote)?
                .into_secure(connector, &self.host)
                .await
                .map_err(remote)?
        };
        self.log_in(&mut ftp).await?;
        Ok(ftp)
    }

    async fn log_in<T: TokioTlsStream + Send>(
        &self,
        ftp: &mut ImplAsyncFtpStream<T>,
    ) -> Result<()> {
        let (user, password) = match &self.login {
            Some((user, password)) => (user.as_str(), password.as_str()),
            None => ("anonymous", "anonymous@"),
        };
        match ftp.login(user, password).await {
            Ok(()) => {}
            Err(FtpError::UnexpectedResponse(r)) if r.status == Status::NotLoggedIn => {
                return Err(Error::AuthRequired { realm: None });
            }
            Err(e) => return Err(remote(e)),
        }
        ftp.transfer_type(FileType::Binary).await.map_err(remote)
    }
}

async fn stat<T: TokioTlsStream + Send>(f: &Ftp, mut ftp: ImplAsyncFtpStream<T>) -> Result<Stat> {
    let size = match ftp.size(&f.path).await {
        Ok(size) => Some(size as u64),
        Err(FtpError::UnexpectedResponse(r)) if r.status == Status::FileUnavailable => {
            return Err(Error::NotFound(f.path.clone()));
        }
        // Some servers only answer SIZE in ASCII mode, or not at all: then it is not known.
        Err(_) => None,
    };
    let modified = ftp.mdtm(&f.path).await.ok().map(|t| t.to_string());
    let resumable = ftp
        .feat()
        .await
        .is_ok_and(|feat| feat.keys().any(|k| k.eq_ignore_ascii_case("REST")));
    ftp.quit().await.ok();
    Ok(Stat {
        size,
        modified,
        resumable,
    })
}

async fn open<T: TokioTlsStream + Send + 'static>(
    f: &Ftp,
    mut ftp: ImplAsyncFtpStream<T>,
    offset: u64,
) -> Result<Reader> {
    if offset > 0 {
        ftp.resume_transfer(offset as usize).await.map_err(remote)?;
    }
    let data = match ftp.retr_as_stream(&f.path).await {
        Ok(data) => data,
        Err(FtpError::UnexpectedResponse(r)) if r.status == Status::FileUnavailable => {
            return Err(Error::NotFound(f.path.clone()));
        }
        Err(e) => return Err(remote(e)),
    };
    Ok(Box::new(Retr { data, _ftp: ftp }))
}

/// A file being sent, with the control connection it came on (dropped together).
struct Retr<T: TokioTlsStream + Send> {
    data: TransferStream<T>,
    _ftp: ImplAsyncFtpStream<T>,
}

impl<T: TokioTlsStream + Send> AsyncRead for Retr<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().data).poll_read(cx, buf)
    }
}

fn remote(e: FtpError) -> Error {
    Error::Remote(format!("FTP: {e}"))
}
