//! Downloads over FTP, FTPS and SFTP. Each protocol is a [`Source`]: it says how large the file
//! is and opens it at an offset. What they share is here: the file in parts over a few
//! connections, each from where it stopped, resuming from the state file, the speed limits,
//! the checksum and the lock, as for HTTP.

mod ftp;
mod sftp;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::download::{Options, Outcome, Phase, Probe, Progress, Request, SegmentView};
use crate::error::{Error, Result};
use crate::remote::Validators;
use crate::segments::SavedSegment;
use crate::state::{State, sibling};

/// Servers often allow a user few connections; more rarely help on a file server.
const MAX_CONNECTIONS: usize = 4;
/// Parts smaller than this are not worth a connection of their own.
const MIN_PART: u64 = 4 << 20;
/// How often the progress is reported.
const TICK: Duration = Duration::from_millis(250);

/// A user name and password, or none (anonymous FTP; SSH with the user's keys).
type Login = Option<(String, String)>;
/// The file as it arrives, from an offset.
type Reader = Box<dyn AsyncRead + Send + Unpin>;

/// What the server says about the file.
struct Stat {
    size: Option<u64>,
    /// The time it was last changed, as the server gives it: a change means another file.
    modified: Option<String>,
    /// It can be read from an offset (`REST` for FTP), so parts and resuming work.
    resumable: bool,
}

/// Whether `url` is one of the protocols here rather than HTTP.
pub(crate) fn handles(url: &str) -> bool {
    let scheme = url.split_once("://").map(|(s, _)| s.to_ascii_lowercase());
    matches!(scheme.as_deref(), Some("ftp" | "ftps" | "ftpes" | "sftp"))
}

enum Source {
    Ftp(ftp::Ftp),
    Sftp(sftp::Sftp),
}

impl Source {
    fn new(url: &str, headers: &[(String, String)], opts: &Options) -> Result<Self> {
        let parsed = url::Url::parse(url).map_err(|e| Error::State(format!("{url}: {e}")))?;
        if parsed.host_str().is_none_or(str::is_empty) {
            return Err(Error::State(format!("{url} names no server")));
        }
        let login = login(&parsed, headers);
        Ok(if parsed.scheme() == "sftp" {
            Self::Sftp(sftp::Sftp::new(&parsed, login, opts))
        } else {
            Self::Ftp(ftp::Ftp::new(&parsed, login, opts.insecure))
        })
    }

    async fn stat(&self) -> Result<Stat> {
        match self {
            Self::Ftp(f) => f.stat().await,
            Self::Sftp(s) => s.stat().await,
        }
    }

    async fn open(&self, offset: u64) -> Result<Reader> {
        match self {
            Self::Ftp(f) => f.open(offset).await,
            Self::Sftp(s) => s.open(offset).await,
        }
    }
}

/// The user name and password in the address, or else in a `Basic` `Authorization` header (a
/// saved login). An address with a user name alone takes the saved login's password when it
/// is that user's.
fn login(url: &url::Url, headers: &[(String, String)]) -> Login {
    let user = percent_encoding_decode(url.username());
    let password = percent_encoding_decode(url.password().unwrap_or_default());
    if !password.is_empty() {
        return Some((user, password));
    }
    match saved_login(headers) {
        Some(saved) if user.is_empty() || saved.0 == user => Some(saved),
        _ if user.is_empty() => None,
        _ => Some((user, password)),
    }
}

/// The user name and password of a `Basic` `Authorization` header.
fn saved_login(headers: &[(String, String)]) -> Login {
    use base64::Engine;
    let basic = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .and_then(|(_, v)| v.strip_prefix("Basic "))?;
    let text = base64::engine::general_purpose::STANDARD
        .decode(basic.trim())
        .ok()?;
    let (user, password) = std::str::from_utf8(&text).ok()?.split_once(':')?;
    Some((user.to_owned(), password.to_owned()))
}

fn percent_encoding_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .into_owned()
}

/// The file's path on the server, decoded.
fn path(url: &url::Url) -> String {
    percent_encoding_decode(url.path())
}

/// TLS for FTPS: the system's certificates, or (`insecure`) any certificate.
fn tls_config(insecure: bool) -> Result<rustls::ClientConfig> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Remote(format!("TLS: {e}")))?;
    let config = if insecure {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
            .with_no_client_auth()
    } else {
        builder
            .with_platform_verifier()
            .map_err(|e| Error::Remote(format!("TLS: {e}")))?
            .with_no_client_auth()
    };
    Ok(config)
}

/// Accepts any server certificate (`Options::insecure`).
#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Asks the server about the file at `url`, as [`crate::probe`] does for HTTP.
pub(crate) async fn probe(
    url: &str,
    headers: &[(String, String)],
    opts: &Options,
) -> Result<Probe> {
    let stat = Source::new(url, headers, opts)?.stat().await?;
    Ok(Probe {
        url: url.to_owned(),
        size: stat.size,
        resumable: stat.resumable && stat.size.is_some(),
        name: crate::name::choose(None, None, url, url),
        etag: None,
        last_modified: stat.modified,
    })
}

/// One part as it downloads.
struct Part {
    start: u64,
    /// `u64::MAX` when the size is not known: then the part ends where the file does.
    end: u64,
    written: AtomicU64,
}

/// What the parts share.
struct Shared {
    source: Source,
    file: std::path::PathBuf,
    parts: Vec<Part>,
    active: AtomicUsize,
}

/// Downloads `req.url` (FTP, FTPS or SFTP) to `req.output`, resuming an earlier attempt when
/// the server still has the same file.
pub(crate) async fn download(
    req: &Request,
    opts: &Options,
    progress: Option<&watch::Sender<Progress>>,
    cancel: &CancellationToken,
) -> Result<Outcome> {
    let started = Instant::now();
    let _lock = crate::place::Lock::acquire(&req.output)?;
    let source = Source::new(&req.url, &req.headers, opts)?;
    let stat = tokio::select! {
        s = source.stat() => s?,
        () = cancel.cancelled() => return Err(Error::Cancelled),
    };
    let part_path = sibling(&req.output, ".shardload-part");
    let state_path = sibling(&req.output, ".shardload-state");
    let validators = Validators {
        etag: None,
        last_modified: stat.modified.clone(),
    };
    let saved = State::load(&state_path).filter(|s| {
        stat.resumable
            && s.url == req.url
            && Some(s.size) == stat.size
            && s.validators == validators
            && part_path.exists()
    });
    let segments = match (&saved, stat.size) {
        (Some(s), _) => s.segments.clone(),
        (None, size) => {
            let file = std::fs::File::create(&part_path)?;
            if let Some(size) = size {
                file.set_len(size).map_err(Error::from)?;
            }
            split(size, if stat.resumable { opts.connections } else { 1 })
        }
    };
    let resumed_from = segments.iter().map(|s| s.written - s.start).sum();
    let shared = Arc::new(Shared {
        source,
        file: part_path.clone(),
        parts: segments
            .iter()
            .map(|s| Part {
                start: s.start,
                end: s.end,
                written: AtomicU64::new(s.written),
            })
            .collect(),
        active: AtomicUsize::new(0),
    });
    let save = |clean: bool| {
        let mut state = State::new(
            &req.url,
            stat.size.unwrap_or_default(),
            validators.clone(),
            shared
                .parts
                .iter()
                .map(|p| SavedSegment {
                    start: p.start,
                    end: p.end,
                    written: p.written.load(Ordering::Relaxed),
                    speed: 0,
                })
                .collect(),
        );
        state.clean = clean;
        // Without the size the download cannot be resumed: no state is kept.
        if stat.size.is_some() {
            state.save(&state_path).ok();
        }
    };
    let ran = run(&shared, opts, progress, cancel, stat.size, &save).await;
    if let Err(e) = ran {
        // What was written is on the disk before the state says so.
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&part_path) {
            f.sync_all().ok();
        }
        save(true);
        return Err(e);
    }
    let size = shared
        .parts
        .iter()
        .map(|p| p.written.load(Ordering::Relaxed) - p.start)
        .sum::<u64>();
    finish(req, opts, progress, cancel, &part_path, &state_path).await?;
    Ok(Outcome {
        size,
        resumed_from,
        elapsed: started.elapsed(),
        checksum: opts.checksum.clone(),
        checksum_from: opts.checksum_from.clone(),
        structure: None,
    })
}

/// The checksum, if one is given, then the file under its name and marked.
async fn finish(
    req: &Request,
    opts: &Options,
    progress: Option<&watch::Sender<Progress>>,
    cancel: &CancellationToken,
    part: &Path,
    state: &Path,
) -> Result<()> {
    if let Some(want) = &opts.checksum {
        match crate::download::verify(part, want, opts.checksum_from.as_deref(), progress, cancel)
            .await
        {
            Err(Error::ChecksumMismatch {
                expected, actual, ..
            }) => {
                std::fs::rename(part, &req.output)?;
                std::fs::remove_file(state).ok();
                return Err(Error::ChecksumMismatch {
                    expected,
                    actual,
                    origin: opts.checksum_from.clone(),
                });
            }
            other => other?,
        }
    }
    std::fs::rename(part, &req.output)?;
    std::fs::remove_file(state).ok();
    if opts.mark_of_the_web {
        crate::mark_of_the_web(&req.output, &req.url, &req.headers);
    }
    if let Some(p) = progress {
        p.send_modify(|p| p.phase = Phase::Done);
    }
    Ok(())
}

/// `size` in up to `connections` (at most [`MAX_CONNECTIONS`]) parts of at least [`MIN_PART`];
/// one part to the end when the size is not known.
fn split(size: Option<u64>, connections: usize) -> Vec<SavedSegment> {
    let Some(size) = size else {
        return vec![SavedSegment {
            start: 0,
            end: u64::MAX,
            written: 0,
            speed: 0,
        }];
    };
    let n = (size / MIN_PART).clamp(1, connections.clamp(1, MAX_CONNECTIONS) as u64);
    (0..n)
        .map(|i| {
            let (start, end) = (size * i / n, size * (i + 1) / n);
            SavedSegment {
                start,
                end,
                written: start,
                speed: 0,
            }
        })
        .collect()
}

/// Every unfinished part at once; reports progress and saves checkpoints meanwhile.
async fn run(
    shared: &Arc<Shared>,
    opts: &Options,
    progress: Option<&watch::Sender<Progress>>,
    cancel: &CancellationToken,
    size: Option<u64>,
    save: &impl Fn(bool),
) -> Result<()> {
    let mut parts = JoinSet::new();
    for i in 0..shared.parts.len() {
        let (shared, opts, cancel) = (Arc::clone(shared), opts.clone(), cancel.clone());
        parts.spawn(async move { fetch(&shared, i, &opts, &cancel).await });
    }
    let mut tick = tokio::time::interval(TICK);
    let mut checkpoint = Instant::now();
    let (mut last, mut speed) = (received(shared), 0.0);
    loop {
        tokio::select! {
            done = parts.join_next() => match done {
                None => return Ok(()),
                Some(Ok(Ok(()))) => {}
                Some(Ok(Err(e))) => {
                    parts.abort_all();
                    return Err(e);
                }
                Some(Err(e)) => return Err(Error::State(format!("a connection failed: {e}"))),
            },
            _ = tick.tick() => {
                let now = received(shared);
                speed = 0.7 * speed + 0.3 * (now - last) as f64 / TICK.as_secs_f64();
                last = now;
                if let Some(p) = progress {
                    p.send_replace(view(shared, size, now, speed));
                }
                if checkpoint.elapsed() >= opts.checkpoint_every {
                    save(false);
                    checkpoint = Instant::now();
                }
            }
        }
    }
}

fn received(shared: &Shared) -> u64 {
    shared
        .parts
        .iter()
        .map(|p| p.written.load(Ordering::Relaxed) - p.start)
        .sum()
}

fn view(shared: &Shared, size: Option<u64>, received: u64, speed: f64) -> Progress {
    Progress {
        phase: Phase::Downloading,
        size,
        received,
        speed,
        connections: shared.active.load(Ordering::Relaxed),
        segments: shared
            .parts
            .iter()
            .filter(|p| p.end != u64::MAX)
            .map(|p| {
                let written = p.written.load(Ordering::Relaxed);
                SegmentView {
                    start: p.start,
                    end: p.end,
                    received: written - p.start,
                    active: written < p.end,
                    speed: 0.0,
                }
            })
            .collect(),
        ..Progress::default()
    }
}

/// Downloads part `i`, connecting again (from where it stopped) after a failure, until
/// [`Options::max_failures`] attempts in a row bring nothing.
async fn fetch(
    shared: &Shared,
    i: usize,
    opts: &Options,
    cancel: &CancellationToken,
) -> Result<()> {
    let part = &shared.parts[i];
    let mut failures = 0;
    loop {
        let from = part.written.load(Ordering::Relaxed);
        if from >= part.end {
            return Ok(());
        }
        shared.active.fetch_add(1, Ordering::Relaxed);
        let result = attempt(shared, part, opts, cancel).await;
        shared.active.fetch_sub(1, Ordering::Relaxed);
        let e = match result {
            Ok(()) => return Ok(()),
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => e,
        };
        if part.written.load(Ordering::Relaxed) > from {
            failures = 0;
        }
        failures += 1;
        if failures >= opts.max_failures {
            return Err(Error::TooManyFailures {
                attempts: failures,
                last: e.to_string(),
            });
        }
        tracing::info!("{e}; connecting again");
        let wait = Duration::from_millis(500) * 2u32.pow(failures.min(5) - 1);
        tokio::select! {
            () = tokio::time::sleep(wait.min(Duration::from_secs(8))) => {}
            () = cancel.cancelled() => return Err(Error::Cancelled),
        }
    }
}

/// One connection's worth of part `part`: from where it stopped to its end.
async fn attempt(
    shared: &Shared,
    part: &Part,
    opts: &Options,
    cancel: &CancellationToken,
) -> Result<()> {
    let from = part.written.load(Ordering::Relaxed);
    let mut reader = tokio::select! {
        r = tokio::time::timeout(opts.read_timeout, shared.source.open(from)) => {
            r.map_err(|_| Error::Stalled(opts.read_timeout))??
        }
        () = cancel.cancelled() => return Err(Error::Cancelled),
    };
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&shared.file)
        .await?;
    file.seek(std::io::SeekFrom::Start(from)).await?;
    let mut buf = vec![0u8; 64 << 10];
    let mut at = from;
    while at < part.end {
        let n = tokio::select! {
            r = tokio::time::timeout(opts.read_timeout, reader.read(&mut buf)) => {
                r.map_err(|_| Error::Stalled(opts.read_timeout))??
            }
            () = cancel.cancelled() => return Err(Error::Cancelled),
        };
        if n == 0 {
            file.flush().await?;
            return if part.end == u64::MAX {
                Ok(())
            } else {
                Err(Error::Truncated)
            };
        }
        let take = (n as u64).min(part.end - at) as usize;
        for limiter in &opts.limiters {
            limiter.acquire(take).await;
        }
        file.write_all(&buf[..take]).await?;
        at += take as u64;
        part.written.store(at, Ordering::Relaxed);
    }
    file.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn logins_in_the_address_are_decoded_whole() {
        let login = |u: &str| super::login(&url::Url::parse(u).expect("url"), &[]);
        assert_eq!(
            login("ftp://me:p&ss=1+2%40x@h/f"),
            Some(("me".to_owned(), "p&ss=1+2@x".to_owned()))
        );
        assert_eq!(login("ftp://h/f"), None);
        // A user name alone: the saved login's password when it is that user's.
        let saved = [crate::basic_auth("me", "secret")];
        let with = |u: &str| super::login(&url::Url::parse(u).expect("url"), &saved);
        let me = Some(("me".to_owned(), "secret".to_owned()));
        assert_eq!(with("sftp://me@h/f"), me);
        assert_eq!(with("sftp://h/f"), me);
        assert_eq!(
            with("sftp://you@h/f"),
            Some(("you".to_owned(), String::new()))
        );
        assert_eq!(
            with("sftp://me:other@h/f"),
            Some(("me".to_owned(), "other".to_owned()))
        );
    }
}
