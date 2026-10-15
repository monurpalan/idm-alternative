//! HTTPS with a self-signed certificate, offering HTTP/2 and HTTP/1.1 through ALPN like a
//! real server does, so clients can be tested on what they negotiate.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::ConnectInfo;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// A certificate for `localhost` and `127.0.0.1`, generated fresh on every start.
pub struct SelfSigned {
    pub cert: CertificateDer<'static>,
    /// The certificate in PEM, for clients that want to trust it explicitly.
    pub pem: String,
    key: PrivatePkcs8KeyDer<'static>,
}

impl SelfSigned {
    pub fn generate() -> anyhow::Result<Self> {
        let names = vec!["localhost".to_owned(), "127.0.0.1".to_owned()];
        let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(names)?;
        Ok(Self {
            cert: cert.der().clone(),
            pem: cert.pem(),
            key: PrivatePkcs8KeyDer::from(signing_key.serialize_der()),
        })
    }
}

/// Serves `router` over TLS on `listener` until the task is dropped. Every request carries
/// the client's address as `ConnectInfo<SocketAddr>`, like `axum::serve` with connect info.
pub async fn serve_tls(
    listener: TcpListener,
    router: Router,
    cert: &SelfSigned,
) -> anyhow::Result<()> {
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.cert.clone()],
        PrivateKeyDer::Pkcs8(cert.key.clone_key()),
    )?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(config));

    loop {
        let (tcp, peer) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let service =
            TowerToHyperService::new(router.clone().layer(axum::Extension(ConnectInfo(peer))));
        tokio::spawn(async move {
            let Ok(tls) = acceptor.accept(tcp).await else {
                return;
            };
            auto::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(tls), service)
                .await
                .ok();
        });
    }
}

/// `axum::serve` with the client's address available to handlers.
pub async fn serve_plain(listener: TcpListener, router: Router) -> std::io::Result<()> {
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}
