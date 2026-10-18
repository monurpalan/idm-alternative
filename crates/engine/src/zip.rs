//! Picking files out of a ZIP archive on a server without downloading the whole archive.
//!
//! The end of the archive (its last 64 KiB and a little more) holds the end of central
//! directory record, which points at the central directory: the list of entries with their
//! sizes and where each one's data starts. With range requests that list is read first, then
//! only the chosen entries' bytes are fetched and unpacked (stored or deflated), each
//! streamed to disk and checked against its CRC-32.
//!
//! Each entry is fetched over one range request; entries are not split across connections.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::Serialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::download::{Options, Phase, Progress, http_config};
use crate::error::{Error, Result};
use crate::limit::Limiter;
use crate::remote::{Http, Remote};

const EOCD_SIG: u32 = 0x0605_4b50;
const EOCD64_SIG: u32 = 0x0606_4b50;
const LOCATOR_SIG: u32 = 0x0706_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
const EOCD_LEN: usize = 22;
const LOCATOR_LEN: usize = 20;
const EOCD64_LEN: usize = 56;
const LOCAL_LEN: usize = 30;
const CENTRAL_LEN: usize = 46;
/// The end of central directory record with the longest comment, the ZIP64 locator and
/// record before it, and some slack: one request finds all of them.
const TAIL: u64 = (EOCD_LEN + 0xFFFF + LOCATOR_LEN + EOCD64_LEN) as u64 + 1024;
/// A larger central directory is refused rather than held in memory.
const MAX_CENTRAL: u64 = 256 << 20;
/// Output is buffered this much before it is written.
const WRITE_BUFFER: usize = 1 << 20;

/// One entry of an archive, as its central directory describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZipEntry {
    /// Position in the central directory.
    pub index: usize,
    /// The path inside the archive, `/`-separated; directories end with `/`.
    pub name: String,
    pub compressed_size: u64,
    /// Size once unpacked.
    pub size: u64,
    /// Compression method: 0 stored, 8 deflate; others (bzip2, LZMA, ...) are listed but
    /// not extracted.
    pub method: u16,
    pub crc32: u32,
    /// Where the entry's local header starts in the file on the server.
    pub header_offset: u64,
    pub is_dir: bool,
    pub encrypted: bool,
}

impl ZipEntry {
    /// Why this entry cannot be extracted, if it cannot.
    pub fn unsupported(&self) -> Option<String> {
        if self.is_dir {
            None
        } else if self.encrypted || self.method == 99 {
            Some("encrypted entries are not supported".to_owned())
        } else if !matches!(self.method, 0 | 8) {
            Some(format!(
                "compression method {} is not supported (only stored and deflate)",
                method_name(self.method)
            ))
        } else {
            None
        }
    }
}

/// A readable name for a compression method number.
pub fn method_name(method: u16) -> String {
    match method {
        0 => "stored".into(),
        8 => "deflate".into(),
        9 => "deflate64".into(),
        12 => "bzip2".into(),
        14 => "lzma".into(),
        93 => "zstd".into(),
        95 => "xz".into(),
        98 => "ppmd".into(),
        99 => "aes".into(),
        m => m.to_string(),
    }
}

/// What extracting one chosen entry came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Extracted {
    pub name: String,
    /// Where it went (or would have gone); `None` when the name is unsafe.
    pub path: Option<PathBuf>,
    pub size: u64,
    /// Already there with the right size, not fetched again.
    pub skipped: bool,
    /// Why the entry was not extracted; the others still are.
    pub error: Option<String>,
}

/// A ZIP archive on a server that supports range requests, with its entries read.
pub struct RemoteZip {
    http: Http,
    remote: Remote,
    limiters: Vec<Limiter>,
    read_timeout: Duration,
    attempts: u32,
    /// Bytes before the archive proper (a self-extracting archive's program): added to
    /// every offset the archive records.
    shift: u64,
    /// Where the central directory starts in the file on the server.
    central_at: u64,
    zip64: bool,
    entries: Vec<ZipEntry>,
}

impl RemoteZip {
    /// Reads the archive's entries with two or three small range requests. Fails with
    /// [`Error::NoRanges`] when the server cannot send parts of the file.
    pub async fn open(url: &str, headers: &[(String, String)], opts: &Options) -> Result<Self> {
        let http = Http::new(&http_config(headers, opts))?;
        // The first request already asks for the end of the file.
        let (remote, tail) = http.probe_tail(url, TAIL).await?;
        if remote.html {
            return Err(Error::HtmlResponse);
        }
        let Some((tail_start, tail)) = tail.filter(|_| remote.size.is_some()) else {
            return Err(Error::NoRanges);
        };
        let mut zip = Self {
            http,
            remote,
            limiters: opts.limiters.clone(),
            read_timeout: opts.read_timeout,
            attempts: opts.max_failures.clamp(1, 5),
            shift: 0,
            central_at: 0,
            zip64: false,
            entries: Vec::new(),
        };
        let end = parse_end(&tail, tail_start).map_err(Error::Archive)?;
        let end = match end.locator {
            Some(record) => {
                // Usually right where the locator says; in a self-extracting archive it is
                // further on by the program's size, and ends where the locator starts.
                let at_locator = end.at - LOCATOR_LEN as u64;
                let mut found = None;
                for at in [record, at_locator.saturating_sub(EOCD64_LEN as u64)] {
                    let Ok(bytes) = zip.read(&tail, tail_start, at, EOCD64_LEN).await else {
                        continue;
                    };
                    if let Ok(e) = parse_end64(&bytes) {
                        found = Some((at, e));
                        break;
                    }
                }
                let (at, e) = found.ok_or_else(|| {
                    Error::Archive("the ZIP64 end of central directory is missing".into())
                })?;
                zip.zip64 = true;
                End { at, ..e }
            }
            None => end,
        };
        zip.shift = end
            .at
            .checked_sub(end.central_offset + end.central_size)
            .ok_or_else(|| Error::Archive("the central directory overlaps its end".into()))?;
        zip.central_at = end.central_offset + zip.shift;
        if end.central_size > MAX_CENTRAL {
            return Err(Error::Archive(format!(
                "the list of files is too large ({} MiB)",
                end.central_size >> 20
            )));
        }
        let central = zip
            .read(&tail, tail_start, zip.central_at, end.central_size as usize)
            .await?;
        zip.entries = parse_central(&central, end.entries, zip.shift).map_err(Error::Archive)?;
        Ok(zip)
    }

    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// The archive's URL after redirects.
    pub fn url(&self) -> &str {
        &self.remote.url
    }

    /// The archive's file name, from `Content-Disposition` or the URL.
    pub fn name(&self) -> &str {
        &self.remote.name
    }

    /// Size of the whole archive.
    pub fn size(&self) -> u64 {
        self.remote.size.unwrap_or_default()
    }

    /// Whether the archive has ZIP64 records (more than 65535 entries or past 4 GiB).
    pub fn is_zip64(&self) -> bool {
        self.zip64
    }

    /// `len` bytes at `at`, from the tail already fetched when they are in it.
    async fn read(&self, tail: &[u8], tail_start: u64, at: u64, len: usize) -> Result<Vec<u8>> {
        let end = at + len as u64;
        if at >= tail_start && end <= tail_start + tail.len() as u64 {
            let from = (at - tail_start) as usize;
            return Ok(tail[from..from + len].to_vec());
        }
        if end > self.size() {
            return Err(Error::Archive(
                "a record points past the end of the file".into(),
            ));
        }
        let bytes = self.http.fetch(&self.remote, at, end, 0).await?;
        if bytes.len() != len {
            return Err(Error::Truncated);
        }
        Ok(bytes)
    }

    /// Extracts the entries at `indices` into `out_dir`, keeping their folders. Entries
    /// that cannot be extracted (unsupported, encrypted, unsafe name, damaged) are reported
    /// in the result and the rest go on; a network or disk failure or cancellation ends the
    /// whole call with an error. An entry already on disk with the right size is skipped
    /// unless `overwrite`; a different file in its place is an error for that entry unless
    /// `overwrite`.
    ///
    /// Progress counts the compressed bytes fetched.
    pub async fn extract(
        &self,
        indices: &[usize],
        out_dir: &Path,
        overwrite: bool,
        progress: Option<&watch::Sender<Progress>>,
        cancel: &CancellationToken,
    ) -> Result<Vec<Extracted>> {
        let mut chosen: Vec<&ZipEntry> = indices
            .iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|&i| self.entries.get(i))
            .collect();
        // In file order, so the reads move forward through the archive.
        chosen.sort_by_key(|e| e.header_offset);
        // Each local record ends where the next one (or the central directory) starts.
        let mut starts: Vec<u64> = self.entries.iter().map(|e| e.header_offset).collect();
        starts.push(self.central_at);
        starts.sort_unstable();
        starts.dedup();

        let total: u64 = chosen
            .iter()
            .filter(|e| !e.is_dir && e.unsupported().is_none())
            .map(|e| e.compressed_size)
            .sum();
        let mut meter = Meter::new(total, progress);
        let mut results = Vec::with_capacity(chosen.len());
        for e in chosen {
            let path = safe_path(&e.name).map(|p| out_dir.join(p));
            let mut result = Extracted {
                name: e.name.clone(),
                path: path.clone(),
                size: e.size,
                skipped: false,
                error: None,
            };
            let Some(path) = path else {
                result.error = Some("unsafe name: it would be written outside the folder".into());
                results.push(result);
                continue;
            };
            if e.is_dir {
                std::fs::create_dir_all(&path)?;
                results.push(result);
                continue;
            }
            if let Some(why) = e.unsupported() {
                result.error = Some(why);
                results.push(result);
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&path) {
                if !overwrite && meta.is_file() && meta.len() == e.size {
                    result.skipped = true;
                    meter.add(e.compressed_size);
                    results.push(result);
                    continue;
                }
                if !overwrite || !meta.is_file() {
                    result.error = Some("a different file is already there".into());
                    results.push(result);
                    continue;
                }
            }
            let end = local_end(e, &starts, self.size());
            let mut attempt = 0;
            let outcome = loop {
                attempt += 1;
                let before = meter.received;
                match self.fetch_entry(e, end, &path, &mut meter, cancel).await {
                    Err(err) if !err.is_fatal() && attempt < self.attempts => {
                        tracing::debug!("zip entry {}: {err}; trying again", e.name);
                        meter.received = before;
                        tokio::select! {
                            () = tokio::time::sleep(Duration::from_secs(1)) => {}
                            () = cancel.cancelled() => break Err(Error::Cancelled),
                        }
                    }
                    other => break other,
                }
            };
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(why)) => result.error = Some(why),
                Err(err) => return Err(err),
            }
            results.push(result);
        }
        meter.finish();
        Ok(results)
    }

    /// Fetches one entry into `path` (through `<path>.idma-part`). The outer error ends the
    /// whole extraction; the inner one is this entry's problem only.
    async fn fetch_entry(
        &self,
        e: &ZipEntry,
        end: u64,
        path: &Path,
        meter: &mut Meter<'_>,
        cancel: &CancellationToken,
    ) -> Result<std::result::Result<(), String>> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let part = crate::state::sibling(path, ".idma-part");
        let resp = self
            .http
            .range(&self.remote, e.header_offset, Some(end), 0)
            .await?;
        let mut stream = resp.bytes_stream();
        let mut header = Vec::with_capacity(LOCAL_LEN);
        let mut sink = None;
        let mut left = e.compressed_size;
        while left > 0 || sink.is_none() {
            let chunk = tokio::select! {
                c = tokio::time::timeout(self.read_timeout, stream.next()) => match c {
                    Err(_) => return Err(Error::Stalled(self.read_timeout)),
                    Ok(None) => return Err(Error::Truncated),
                    Ok(Some(c)) => c?,
                },
                () = cancel.cancelled() => return Err(Error::Cancelled),
            };
            let data = if sink.is_none() {
                // The local header first: its extra field can differ from the central one.
                header.extend_from_slice(&chunk);
                if header.len() < LOCAL_LEN {
                    continue;
                }
                if u32_at(&header, 0) != LOCAL_SIG {
                    return Ok(Err("its local header is missing".into()));
                }
                let at = LOCAL_LEN + u16_at(&header, 26) as usize + u16_at(&header, 28) as usize;
                if header.len() < at {
                    continue;
                }
                sink = Some(Sink::new(e.method, std::fs::File::create(&part)?, e.size));
                header.split_off(at)
            } else {
                chunk.to_vec()
            };
            let take = data.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            if let Err(r) = self.feed(&mut sink, &data[..take], meter, &part).await? {
                return Ok(Err(r));
            }
            left -= take as u64;
        }
        drop(stream);
        let Some(sink) = sink else {
            return Err(Error::Truncated);
        };
        let (written, crc) = match sink.finish() {
            Ok(done) => done,
            Err(err) => return damaged(err, &part),
        };
        if written != e.size {
            std::fs::remove_file(&part).ok();
            return Ok(Err(format!(
                "unpacked to {written} bytes instead of {}",
                e.size
            )));
        }
        if crc != e.crc32 {
            std::fs::remove_file(&part).ok();
            return Ok(Err(format!(
                "CRC-32 mismatch: expected {:08x}, got {crc:08x}",
                e.crc32
            )));
        }
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        std::fs::rename(&part, path)?;
        Ok(Ok(()))
    }

    async fn feed(
        &self,
        sink: &mut Option<Sink>,
        bytes: &[u8],
        meter: &mut Meter<'_>,
        part: &Path,
    ) -> Result<std::result::Result<(), String>> {
        if bytes.is_empty() {
            return Ok(Ok(()));
        }
        for l in &self.limiters {
            l.acquire(bytes.len()).await;
        }
        meter.add(bytes.len() as u64);
        match sink.as_mut().map(|s| s.write(bytes)) {
            Some(Err(err)) => damaged(err, part),
            _ => Ok(Ok(())),
        }
    }
}

/// An unpacking error is the entry's problem; any other write error (a full disk) is not.
fn damaged(err: std::io::Error, part: &Path) -> Result<std::result::Result<(), String>> {
    use std::io::ErrorKind as K;
    std::fs::remove_file(part).ok();
    match err.kind() {
        K::InvalidData | K::InvalidInput | K::WriteZero => {
            Ok(Err(format!("the compressed data is damaged: {err}")))
        }
        _ => Err(err.into()),
    }
}

/// Where the request for an entry's local record ends: where the next record starts, or,
/// when the offsets make no sense, far enough for the largest possible header.
fn local_end(e: &ZipEntry, starts: &[u64], size: u64) -> u64 {
    let least = e.header_offset + LOCAL_LEN as u64 + e.compressed_size;
    let next = starts
        .iter()
        .copied()
        .find(|&s| s > e.header_offset)
        .unwrap_or(size);
    if next >= least {
        next.min(size)
    } else {
        (least + 2 * 0xFFFF).min(size)
    }
}

/// Unpacks into a file while counting and hashing what comes out.
enum Sink {
    Stored(Out),
    Deflate(Box<flate2::write::DeflateDecoder<Out>>),
}

impl Sink {
    fn new(method: u16, file: std::fs::File, limit: u64) -> Self {
        let out = Out {
            file: std::io::BufWriter::with_capacity(WRITE_BUFFER, file),
            crc: crc32fast::Hasher::new(),
            written: 0,
            limit,
        };
        if method == 8 {
            Sink::Deflate(Box::new(flate2::write::DeflateDecoder::new(out)))
        } else {
            Sink::Stored(out)
        }
    }

    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match self {
            Sink::Stored(o) => o.write_all(bytes),
            Sink::Deflate(d) => d.write_all(bytes),
        }
    }

    /// Bytes written and their CRC-32.
    fn finish(self) -> std::io::Result<(u64, u32)> {
        let mut out = match self {
            Sink::Stored(o) => o,
            Sink::Deflate(d) => d.finish()?,
        };
        out.file.flush()?;
        Ok((out.written, out.crc.finalize()))
    }
}

struct Out {
    file: std::io::BufWriter<std::fs::File>,
    crc: crc32fast::Hasher,
    written: u64,
    /// The size the archive gives: more output is damaged data (or a decompression bomb).
    limit: u64,
}

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written + buf.len() as u64 > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "more data than the archive says",
            ));
        }
        let n = self.file.write(buf)?;
        self.crc.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Progress over all chosen entries.
struct Meter<'a> {
    total: u64,
    received: u64,
    progress: Option<&'a watch::Sender<Progress>>,
    sample: (Instant, u64),
    speed: f64,
}

impl<'a> Meter<'a> {
    fn new(total: u64, progress: Option<&'a watch::Sender<Progress>>) -> Self {
        let m = Self {
            total,
            received: 0,
            progress,
            sample: (Instant::now(), 0),
            speed: 0.0,
        };
        m.send(Phase::Downloading);
        m
    }

    fn add(&mut self, n: u64) {
        self.received += n;
        let elapsed = self.sample.0.elapsed();
        if elapsed >= Duration::from_millis(500) {
            self.speed = self.received.saturating_sub(self.sample.1) as f64 / elapsed.as_secs_f64();
            self.sample = (Instant::now(), self.received);
        }
        self.send(Phase::Downloading);
    }

    fn finish(&mut self) {
        self.speed = 0.0;
        self.send(Phase::Done);
    }

    fn send(&self, phase: Phase) {
        if let Some(p) = self.progress {
            p.send_modify(|p| {
                p.phase = phase;
                p.size = Some(self.total);
                p.received = self.received;
                p.speed = self.speed;
                p.connections = usize::from(phase == Phase::Downloading);
            });
        }
    }
}

/// The relative path an entry name may be written to: `\` read as `/`, `.` parts dropped,
/// each part made a valid file name. `None` for names that climb out of the folder (`..`),
/// are absolute (`/x`, `C:x`, `\\server\x`) or are empty.
pub fn safe_path(name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    if name.starts_with('/') {
        return None;
    }
    let mut path = PathBuf::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            p if p.contains(':') => return None,
            p => path.push(crate::name::sanitize(p)),
        }
    }
    (!path.as_os_str().is_empty()).then_some(path)
}

/// The entries `patterns` choose, by index, and the patterns that chose nothing. A pattern
/// is an entry name or a glob (`*` and `?` within one folder, `**` across folders); a folder
/// chooses everything in it.
pub fn select(entries: &[ZipEntry], patterns: &[String]) -> (Vec<usize>, Vec<String>) {
    let mut chosen = BTreeSet::new();
    let mut unmatched = Vec::new();
    for pattern in patterns {
        let pattern = pattern.replace('\\', "/");
        let folder = format!("{}/", pattern.trim_end_matches('/'));
        let mut any = false;
        for e in entries {
            if glob(pattern.as_bytes(), e.name.as_bytes())
                || e.name.starts_with(&folder)
                || (e.is_dir && glob(folder.as_bytes(), e.name.as_bytes()))
            {
                any = true;
                chosen.insert(e.index);
                // A folder brings everything in it.
                if e.is_dir {
                    for inner in entries.iter().filter(|i| i.name.starts_with(&e.name)) {
                        chosen.insert(inner.index);
                    }
                }
            }
        }
        if !any {
            unmatched.push(pattern);
        }
    }
    (chosen.into_iter().collect(), unmatched)
}

/// `*` and `?` match within one path part, `**` across parts.
fn glob(p: &[u8], s: &[u8]) -> bool {
    match p {
        [] => s.is_empty(),
        // `**/`: no folders, or any number of whole ones.
        [b'*', b'*', b'/', rest @ ..] => {
            glob(rest, s)
                || s.iter()
                    .enumerate()
                    .any(|(i, &c)| c == b'/' && glob(rest, &s[i + 1..]))
        }
        [b'*', b'*', rest @ ..] => (0..=s.len()).any(|i| glob(rest, &s[i..])),
        [b'*', rest @ ..] => {
            for i in 0..=s.len() {
                if glob(rest, &s[i..]) {
                    return true;
                }
                if s.get(i) == Some(&b'/') {
                    break;
                }
            }
            false
        }
        [b'?', rest @ ..] => matches!(s.first(), Some(c) if *c != b'/') && glob(rest, &s[1..]),
        [c, rest @ ..] => s.first() == Some(c) && glob(rest, &s[1..]),
    }
}

/// The end of central directory record, or what the ZIP64 record says instead.
#[derive(Debug, Clone, PartialEq, Eq)]
struct End {
    /// Where the record starts in the file.
    at: u64,
    entries: u64,
    central_size: u64,
    central_offset: u64,
    /// Where the archive says its ZIP64 end record is, when it has one.
    locator: Option<u64>,
}

/// Finds the end of central directory record in the last bytes of the file (`tail`, which
/// starts at `tail_start`).
fn parse_end(tail: &[u8], tail_start: u64) -> std::result::Result<End, String> {
    if tail.len() < EOCD_LEN {
        return Err("the file is too small to be a ZIP archive".into());
    }
    let found = |exact: bool| {
        (0..=tail.len() - EOCD_LEN).rev().find(|&i| {
            let comment = u16_at(tail, i + 20) as usize;
            u32_at(tail, i) == EOCD_SIG
                && if exact {
                    i + EOCD_LEN + comment == tail.len()
                } else {
                    i + EOCD_LEN + comment <= tail.len()
                }
        })
    };
    // A comment can hold the signature; a record whose comment ends the file is the one.
    let i = found(true)
        .or_else(|| found(false))
        .ok_or("no end of central directory: not a ZIP archive, or a damaged one")?;
    let r = &tail[i..];
    let locator = i
        .checked_sub(LOCATOR_LEN)
        .filter(|&l| u32_at(tail, l) == LOCATOR_SIG)
        .map(|l| u64_at(tail, l + 8));
    let saturated =
        u16_at(r, 10) == 0xFFFF || u32_at(r, 12) == u32::MAX || u32_at(r, 16) == u32::MAX;
    if saturated && locator.is_none() {
        return Err("a ZIP64 archive without its ZIP64 locator".into());
    }
    if u16_at(r, 4) != u16_at(r, 6) && locator.is_none() {
        return Err("archives split over several files are not supported".into());
    }
    Ok(End {
        at: tail_start + i as u64,
        entries: u64::from(u16_at(r, 10)),
        central_size: u64::from(u32_at(r, 12)),
        central_offset: u64::from(u32_at(r, 16)),
        locator,
    })
}

/// The ZIP64 end of central directory record (`at` is filled in by the caller).
fn parse_end64(r: &[u8]) -> std::result::Result<End, String> {
    if r.len() < EOCD64_LEN || u32_at(r, 0) != EOCD64_SIG {
        return Err("no ZIP64 end of central directory".into());
    }
    if u32_at(r, 16) != 0 || u32_at(r, 20) != 0 {
        return Err("archives split over several files are not supported".into());
    }
    Ok(End {
        at: 0,
        entries: u64_at(r, 32),
        central_size: u64_at(r, 40),
        central_offset: u64_at(r, 48),
        locator: None,
    })
}

/// Reads `count` central directory records; `shift` is added to every local header offset.
fn parse_central(buf: &[u8], count: u64, shift: u64) -> std::result::Result<Vec<ZipEntry>, String> {
    if count.saturating_mul(CENTRAL_LEN as u64) > buf.len() as u64 {
        return Err(format!(
            "{count} entries cannot fit in the central directory"
        ));
    }
    let mut entries = Vec::with_capacity(count as usize);
    let mut at = 0;
    for index in 0..count as usize {
        let r = buf
            .get(at..at + CENTRAL_LEN)
            .ok_or("the central directory is cut short")?;
        if u32_at(r, 0) != CENTRAL_SIG {
            return Err(format!("entry {index} of the central directory is damaged"));
        }
        let flags = u16_at(r, 8);
        let method = u16_at(r, 10);
        let crc32 = u32_at(r, 16);
        let mut compressed_size = u64::from(u32_at(r, 20));
        let mut size = u64::from(u32_at(r, 24));
        let name_len = u16_at(r, 28) as usize;
        let extra_len = u16_at(r, 30) as usize;
        let comment_len = u16_at(r, 32) as usize;
        let mut header_offset = u64::from(u32_at(r, 42));
        let name_at = at + CENTRAL_LEN;
        let extra_at = name_at + name_len;
        let next = extra_at + extra_len + comment_len;
        if next > buf.len() {
            return Err("the central directory is cut short".into());
        }
        let raw_name = &buf[name_at..extra_at];
        let mut name = None;
        let mut extra = &buf[extra_at..extra_at + extra_len];
        while extra.len() >= 4 {
            let (id, len) = (u16_at(extra, 0), u16_at(extra, 2) as usize);
            let Some(field) = extra.get(4..4 + len) else {
                break;
            };
            match id {
                // ZIP64: the values the record left at their maximum, in this order.
                0x0001 => {
                    let mut f = field;
                    let mut next64 = |v: &mut u64, max: bool| {
                        if max && f.len() >= 8 {
                            *v = u64_at(f, 0);
                            f = &f[8..];
                        }
                    };
                    let max = u64::from(u32::MAX);
                    let wanted = (size == max, compressed_size == max, header_offset == max);
                    next64(&mut size, wanted.0);
                    next64(&mut compressed_size, wanted.1);
                    next64(&mut header_offset, wanted.2);
                }
                // Info-ZIP Unicode path: the UTF-8 name, if the name itself is not.
                0x7075 if field.len() > 5 && flags & (1 << 11) == 0 => {
                    name = std::str::from_utf8(&field[5..]).ok().map(str::to_owned);
                }
                _ => {}
            }
            extra = &extra[4 + len..];
        }
        let name = name.unwrap_or_else(|| decode_name(raw_name, flags & (1 << 11) != 0));
        entries.push(ZipEntry {
            index,
            is_dir: name.ends_with('/') || name.ends_with('\\'),
            name,
            compressed_size,
            size,
            method,
            crc32,
            header_offset: header_offset + shift,
            encrypted: flags & 1 != 0,
        });
        at = next;
    }
    Ok(entries)
}

/// Names are UTF-8 when the archive says so (or when they are valid UTF-8), else code page
/// 437 as in DOS.
fn decode_name(raw: &[u8], utf8: bool) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_owned(),
        Err(_) if utf8 => String::from_utf8_lossy(raw).into_owned(),
        Err(_) => raw
            .iter()
            .map(|&b| {
                if b < 0x80 {
                    b as char
                } else {
                    CP437_HIGH.chars().nth(usize::from(b - 0x80)).unwrap_or('_')
                }
            })
            .collect(),
    }
}

const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

fn u16_at(b: &[u8], i: usize) -> u16 {
    b.get(i..i + 2)
        .map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    b.get(i..i + 4)
        .map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    b.get(i..i + 8).map_or(0, |s| {
        u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use idma_testserver::zip::{DEFLATE, Entry, STORED, write};

    fn list(archive: &[u8]) -> (End, Vec<ZipEntry>) {
        // The whole file is "the tail" here, as for any archive under 64 KiB.
        let mut end = parse_end(archive, 0).expect("end record");
        let mut shift_base = end.at;
        if let Some(at) = end.locator {
            let locator = end.at as usize - LOCATOR_LEN;
            let at = if parse_end64(&archive[at as usize..]).is_ok() {
                at as usize
            } else {
                locator - EOCD64_LEN
            };
            end = parse_end64(&archive[at..]).expect("zip64 record");
            shift_base = at as u64;
        }
        let shift = shift_base - end.central_offset - end.central_size;
        let central_at = (end.central_offset + shift) as usize;
        let central = &archive[central_at..central_at + end.central_size as usize];
        let entries = parse_central(central, end.entries, shift).expect("central directory");
        (end, entries)
    }

    fn fixtures() -> Vec<Entry> {
        vec![
            Entry::new("a.txt", "hello", STORED),
            Entry::new("dir/b.txt", "hello hello hello hello".repeat(100), DEFLATE),
            Entry::dir("dir/"),
            Entry {
                descriptor: true,
                ..Entry::new("dir/c.bin", vec![7u8; 5000], DEFLATE)
            },
            Entry::new("../../evil.txt", "x", STORED),
        ]
    }

    #[test]
    fn reads_plain_archives() {
        let fx = fixtures();
        let zip = write(&fx, false, &[], b"comment with PK\x05\x06 inside");
        let (end, entries) = list(&zip);
        assert_eq!(end.locator, None);
        assert_eq!(entries.len(), fx.len());
        for (e, f) in entries.iter().zip(&fx) {
            assert_eq!(e.name, f.name);
            assert_eq!(e.size, f.data.len() as u64);
            assert_eq!(e.method, f.method);
            assert_eq!(e.crc32, crc32fast::hash(&f.data));
            assert!(!e.encrypted);
        }
        assert_eq!(entries[0].header_offset, 0);
        assert_eq!(entries[0].compressed_size, 5);
        assert!(entries[1].compressed_size < entries[1].size);
        assert!(entries[2].is_dir);
        assert_eq!(safe_path(&entries[4].name), None);
    }

    #[test]
    fn reads_zip64_archives() {
        let fx = fixtures();
        let zip = write(&fx, true, &[], b"");
        let plain_end = parse_end(&zip, 0).expect("end record");
        assert_eq!(plain_end.entries, 0xFFFF);
        assert!(plain_end.locator.is_some());
        let (end, entries) = list(&zip);
        assert_eq!(end.entries, fx.len() as u64);
        let plain = list(&write(&fx, false, &[], b"")).1;
        for (a, b) in entries.iter().zip(&plain) {
            assert_eq!(
                (
                    &a.name,
                    a.size,
                    a.compressed_size,
                    a.crc32,
                    a.header_offset > 0
                ),
                (
                    &b.name,
                    b.size,
                    b.compressed_size,
                    b.crc32,
                    b.header_offset > 0
                )
            );
        }
    }

    #[test]
    fn finds_archives_after_a_prefix() {
        let fx = fixtures();
        for zip64 in [false, true] {
            let zip = write(&fx, zip64, b"MZ stub of a self-extracting archive", b"");
            let entries = list(&zip).1;
            let at = entries[1].header_offset as usize;
            assert_eq!(u32_at(&zip, at), LOCAL_SIG, "zip64 {zip64}");
        }
    }

    #[test]
    fn flags_what_cannot_be_extracted() {
        let zip = write(
            &[
                Entry {
                    encrypted: true,
                    ..Entry::new("s.txt", "x", STORED)
                },
                Entry::new("p.xz", "x", 95),
            ],
            false,
            &[],
            b"",
        );
        let entries = list(&zip).1;
        assert!(entries[0].encrypted);
        assert!(
            entries[0]
                .unsupported()
                .is_some_and(|w| w.contains("encrypted"))
        );
        assert!(entries[1].unsupported().is_some_and(|w| w.contains("xz")));
    }

    #[test]
    fn rejects_what_is_not_an_archive() {
        assert!(parse_end(b"just some text, no records at all", 0).is_err());
        assert!(parse_end(b"tiny", 0).is_err());
        let mut zip = write(&fixtures(), false, &[], b"");
        // Claims more entries than the directory holds.
        let n = zip.len() - EOCD_LEN + 10;
        zip[n] = 200;
        let end = parse_end(&zip, 0).expect("end");
        let central = &zip[end.central_offset as usize..][..end.central_size as usize];
        assert!(parse_central(central, end.entries, 0).is_err());
    }

    #[test]
    fn unsafe_names_stay_inside() {
        assert_eq!(safe_path("a/b.txt"), Some(PathBuf::from("a").join("b.txt")));
        assert_eq!(safe_path("./a//b"), Some(PathBuf::from("a").join("b")));
        assert_eq!(safe_path("a\\b"), Some(PathBuf::from("a").join("b")));
        for bad in [
            "../x",
            "a/../../x",
            "/etc/passwd",
            "\\\\server\\share\\x",
            "C:\\x",
            "C:x",
            "a/..\\..\\x",
            "",
            "/",
            "file:stream",
        ] {
            assert_eq!(safe_path(bad), None, "{bad}");
        }
        assert_eq!(
            safe_path("con/aux.txt"),
            Some(PathBuf::from("_con").join("_aux.txt"))
        );
        assert_eq!(
            safe_path("docs\u{202E}/invoice\u{202E}fdp.exe"),
            Some(PathBuf::from("docs_").join("invoice_fdp.exe"))
        );
    }

    #[test]
    fn selection() {
        let zip = write(&fixtures(), false, &[], b"");
        let entries = list(&zip).1;
        let pick = |p: &[&str]| {
            select(
                &entries,
                &p.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            )
        };
        assert_eq!(pick(&["a.txt"]), (vec![0], vec![]));
        assert_eq!(pick(&["*.txt"]), (vec![0], vec![]));
        assert_eq!(pick(&["**/*.txt"]), (vec![0, 1, 4], vec![]));
        assert_eq!(pick(&["dir"]), (vec![1, 2, 3], vec![]));
        assert_eq!(pick(&["dir/"]), (vec![1, 2, 3], vec![]));
        assert_eq!(
            pick(&["dir/?.txt", "nope"]),
            (vec![1], vec!["nope".to_owned()])
        );
    }

    #[test]
    fn code_page_437_names() {
        assert_eq!(CP437_HIGH.chars().count(), 128);
        assert_eq!(decode_name(&[0x80, b'a', 0x81], false), "Çaü");
        assert_eq!(decode_name("ğ".as_bytes(), false), "ğ");
    }

    #[test]
    fn globs() {
        assert!(glob(b"*.txt", b"a.txt"));
        assert!(!glob(b"*.txt", b"d/a.txt"));
        assert!(glob(b"**/a.txt", b"a.txt"));
        assert!(glob(b"**/a.txt", b"x/y/a.txt"));
        assert!(glob(b"x/**", b"x/y/z"));
        assert!(glob(b"a?c", b"abc"));
        assert!(!glob(b"a?c", b"a/c"));
    }
}
