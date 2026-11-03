//! Delta downloads with zsync. A `.zsync` file next to a file lists a checksum of every block
//! of it; blocks found in a copy already here (an older version: anywhere in it, also moved)
//! are copied from that copy, and only the rest is downloaded, in ranges. The finished file is
//! checked against the SHA-1 the `.zsync` gives; when it does not match (a block matched by
//! chance), the file is downloaded whole instead.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use md4::{Digest, Md4};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::checksum::{Algorithm, Checksum};
use crate::download::{Options, Outcome, Phase, Progress, Request, http_config};
use crate::error::{Error, Result};
use crate::remote::{Http, Validators};
use crate::segments::SavedSegment;
use crate::state::{State, ZsyncState, sibling};

/// A `.zsync` larger than this is not one.
const MAX_CONTROL: usize = 64 << 20;
/// Downloaded at most this much per request, so a long run of changed blocks streams.
const MAX_RANGE: u64 = 4 << 20;

/// What a `.zsync` file says about its file.
#[derive(Debug, Clone)]
pub struct Control {
    /// The file's name, as the `.zsync` gives it.
    pub filename: Option<String>,
    pub length: u64,
    /// Where the file is, relative to the `.zsync` (resolved by [`Control::target`]).
    pub urls: Vec<String>,
    pub sha1: Option<String>,
    blocksize: usize,
    weak_len: usize,
    strong_len: usize,
    blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
struct Block {
    weak: u32,
    strong: Vec<u8>,
}

impl Control {
    /// Reads a `.zsync`: a header of `Key: value` lines, an empty line, then each block's
    /// checksums. Compressed (`Z-Map2`) files are not supported.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let bad = |why: &str| Error::State(format!("not a usable .zsync file: {why}"));
        let end = find(data, b"\n\n").ok_or_else(|| bad("no header"))?;
        let header = std::str::from_utf8(&data[..end]).map_err(|_| bad("header not text"))?;
        let mut fields: HashMap<String, Vec<String>> = HashMap::new();
        for line in header.lines() {
            if let Some((k, v)) = line.split_once(':') {
                fields
                    .entry(k.trim().to_ascii_lowercase())
                    .or_default()
                    .push(v.trim().to_owned());
            }
        }
        let one = |k: &str| fields.get(k).and_then(|v| v.first()).cloned();
        if fields.contains_key("z-map2") || fields.contains_key("recompress") {
            return Err(bad("compressed files are not supported"));
        }
        let number = |k: &str| -> Result<u64> {
            one(k)
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| bad(&format!("no {k}")))
        };
        let length = number("length")?;
        let blocksize = number("blocksize")? as usize;
        if !blocksize.is_power_of_two() || !(256..=1 << 24).contains(&blocksize) {
            return Err(bad("odd block size"));
        }
        let lengths: Vec<usize> = one("hash-lengths")
            .unwrap_or_else(|| "1,4,16".into())
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        let [_, weak_len, strong_len] = lengths[..] else {
            return Err(bad("odd hash lengths"));
        };
        if !(1..=4).contains(&weak_len) || !(1..=16).contains(&strong_len) {
            return Err(bad("odd hash lengths"));
        }
        let count = length.div_ceil(blocksize as u64) as usize;
        let body = &data[end + 2..];
        let each = weak_len + strong_len;
        if body.len() < count.saturating_mul(each) {
            return Err(bad("cut short"));
        }
        let blocks = body
            .chunks_exact(each)
            .take(count)
            .map(|b| Block {
                weak: b[..weak_len]
                    .iter()
                    .fold(0u32, |w, &byte| (w << 8) | u32::from(byte)),
                strong: b[weak_len..].to_vec(),
            })
            .collect();
        Ok(Self {
            filename: one("filename"),
            length,
            urls: fields.get("url").cloned().unwrap_or_default(),
            sha1: one("sha-1").map(|s| s.to_ascii_lowercase()),
            blocksize,
            weak_len,
            strong_len,
            blocks,
        })
    }

    /// The file's address: its `URL:`, relative to `control_url`, or `control_url` without
    /// `.zsync`.
    pub fn target(&self, control_url: &str) -> String {
        let base = url::Url::parse(control_url).ok();
        self.urls
            .iter()
            .find_map(|u| base.as_ref()?.join(u).ok().map(String::from))
            .unwrap_or_else(|| control_url.trim_end_matches(".zsync").to_owned())
    }

    fn mask(&self) -> u32 {
        if self.weak_len == 4 {
            u32::MAX
        } else {
            (1u32 << (8 * self.weak_len)) - 1
        }
    }

    /// The block at `data` (`blocksize` bytes, zeros past the end of a short one) has block
    /// `i`'s strong checksum.
    fn strong_matches(&self, i: usize, data: &[u8]) -> bool {
        let mut md4 = Md4::new();
        md4.update(data);
        if data.len() < self.blocksize {
            md4.update(vec![0u8; self.blocksize - data.len()]);
        }
        md4.finalize()[..self.strong_len] == self.blocks[i].strong[..]
    }

    /// Identifies this `.zsync` (its file's length and every block's checksums), for a
    /// download that resumes to follow the same one.
    fn fingerprint(&self) -> String {
        let mut md4 = Md4::new();
        md4.update(self.length.to_le_bytes());
        md4.update((self.blocksize as u64).to_le_bytes());
        for b in &self.blocks {
            md4.update(b.weak.to_le_bytes());
            md4.update(&b.strong);
        }
        md4.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Block `i`'s place in the file.
    fn block(&self, i: usize) -> (u64, u64) {
        let start = i as u64 * self.blocksize as u64;
        (start, (start + self.blocksize as u64).min(self.length))
    }
}

/// zsync's weak checksum of `data` (as rsync's): `a` the sum of the bytes, `b` each byte
/// weighted by how far it is from the end, both modulo 2^16, as `a << 16 | b`.
fn rsum(data: &[u8]) -> (u16, u16) {
    let n = data.len();
    data.iter()
        .enumerate()
        .fold((0u16, 0u16), |(a, b), (i, &c)| {
            (
                a.wrapping_add(u16::from(c)),
                b.wrapping_add(((n - i) as u16).wrapping_mul(u16::from(c))),
            )
        })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// For every block of the file, where in `seed` the same bytes are, if anywhere: every
/// offset of the seed is tried (the weak checksum rolling along), blocks past its end
/// counted as zeros.
pub(crate) fn find_blocks(
    control: &Control,
    seed: &Path,
    stop: &AtomicBool,
) -> Result<Vec<Option<u64>>> {
    let mut data = Vec::new();
    std::fs::File::open(seed)?.read_to_end(&mut data)?;
    let size = control.blocksize;
    let mask = control.mask();
    let mut by_weak: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, b) in control.blocks.iter().enumerate() {
        by_weak.entry(b.weak & mask).or_default().push(i);
    }
    let mut found = vec![None; control.blocks.len()];
    let mut left = found.len();
    data.extend(std::iter::repeat_n(0u8, size));
    let real = data.len() - size;
    let mut at = 0;
    let (mut a, mut b) = rsum(&data[..size.min(data.len())]);
    while at < real && left > 0 {
        if at % (1 << 20) == 0 && stop.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let weak = (u32::from(a) << 16 | u32::from(b)) & mask;
        let mut jumped = false;
        if let Some(candidates) = by_weak.get(&weak) {
            let window = &data[at..at + size];
            for &i in candidates {
                let (start, end) = control.block(i);
                let len = (end - start) as usize;
                // A short last block matches only where the seed's bytes end with zeros.
                if found[i].is_none()
                    && window[len..].iter().all(|&z| z == 0)
                    && control.strong_matches(i, &window[..len])
                {
                    found[i] = Some(at as u64);
                    left -= 1;
                    jumped = true;
                }
            }
        }
        if jumped && at + size < real {
            at += size;
            (a, b) = rsum(&data[at..at + size]);
            continue;
        }
        // Roll one byte on: the first byte leaves, the next comes in.
        let (out, inn) = (u16::from(data[at]), u16::from(data[at + size]));
        a = a.wrapping_sub(out).wrapping_add(inn);
        b = b
            .wrapping_sub((size as u16).wrapping_mul(out))
            .wrapping_add(a);
        at += 1;
    }
    Ok(found)
}

/// Where the `.zsync` of the file at `url` would be: `.zsync` after its path, its query kept.
pub fn control_url(url: &str) -> Option<String> {
    let mut u = url::Url::parse(url).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let path = format!("{}.zsync", u.path());
    u.set_path(&path);
    Some(u.into())
}

/// Reads the `.zsync` at `url`.
pub async fn fetch_control(
    url: &str,
    headers: &[(String, String)],
    opts: &Options,
) -> Result<Control> {
    let data = crate::fetch_bytes(url, headers, opts, MAX_CONTROL).await?;
    Control::parse(&data)
}

/// How a delta download went.
#[derive(Debug)]
pub struct Delta {
    pub outcome: Outcome,
    /// Bytes taken from the copies here rather than downloaded.
    pub reused: u64,
    /// The copy most of them came from.
    pub from: Option<PathBuf>,
}

/// Downloads the file `control` (read from `control_url`) describes to `req.output`, taking
/// every block it can from `seeds`. `req.url` is not used: the file's address comes from the
/// `.zsync`. The state file keeps the ranges still to download, so a download stopped (or
/// killed) part way resumes with only those, without matching the copies again.
pub async fn update(
    control_url: &str,
    control: &Control,
    seeds: &[PathBuf],
    req: &Request,
    opts: &Options,
    progress: Option<&watch::Sender<Progress>>,
    cancel: &CancellationToken,
) -> Result<Delta> {
    let started = Instant::now();
    let target = control.target(control_url);
    let whole = Request {
        url: target.clone(),
        ..req.clone()
    };
    let part = sibling(&req.output, ".shardload-part");
    let state_path = sibling(&req.output, ".shardload-state");
    // Held while this writes the file; given up before downloading it whole, which takes it.
    let lock = crate::place::Lock::acquire(&req.output)?;
    let http = Http::new(&http_config(&req.headers, opts))?;
    let (remote, _) = http.probe_sample(&target, 1, 0).await?;
    if !remote.ranges || remote.size != Some(control.length) {
        std::fs::remove_file(&part).ok();
        std::fs::remove_file(&state_path).ok();
        drop(lock);
        return whole_file(&whole, opts, progress, cancel).await;
    }
    let (mut state, resumed_from) = match resumable(control, req, &remote.validators) {
        Some(state) => {
            let left: u64 = state.segments.iter().map(|s| s.end - s.written).sum();
            (state, control.length - left)
        }
        None => (
            start(control, seeds, req, remote.validators.clone(), cancel).await?,
            0,
        ),
    };
    let missing = Missing {
        http: &http,
        remote: &remote,
        part: &part,
        state_path: &state_path,
        opts,
        progress,
    };
    missing.fetch(&mut state, cancel).await?;
    if let Some(hex) = &control.sha1 {
        let want = Checksum {
            algorithm: Algorithm::Sha1,
            digest: (0..hex.len() / 2)
                .filter_map(|i| u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok())
                .collect(),
        };
        match crate::download::verify(&part, &want, None, progress, cancel).await {
            Ok(()) => {}
            // A block taken from the copy by a chance match: the whole file instead.
            Err(Error::ChecksumMismatch { .. }) => {
                tracing::warn!("a block reused by zsync was wrong; downloading the whole file");
                std::fs::remove_file(&part).ok();
                std::fs::remove_file(&state_path).ok();
                drop(lock);
                return whole_file(&whole, opts, progress, cancel).await;
            }
            Err(e) => return Err(e),
        }
    }
    std::fs::rename(&part, &req.output)?;
    std::fs::remove_file(&state_path).ok();
    if opts.mark_of_the_web {
        crate::mark_of_the_web(&req.output, &target, &req.headers);
    }
    if let Some(p) = progress {
        p.send_modify(|p| p.phase = Phase::Done);
    }
    let (reused, from) = state.zsync.map_or((0, None), |z| (z.reused, z.from));
    Ok(Delta {
        outcome: Outcome {
            size: control.length,
            resumed_from,
            elapsed: started.elapsed(),
            checksum: None,
            checksum_from: None,
            structure: None,
        },
        reused,
        from,
    })
}

/// Whether a zsync download to `output` stopped part way, to be resumed by [`update`].
pub fn resumes(output: &Path) -> bool {
    State::load(&sibling(output, ".shardload-state")).is_some_and(|s| s.zsync.is_some())
        && sibling(output, ".shardload-part").exists()
}

/// The state of a download of `control` to `req.output` that stopped part way, when it
/// follows the same `.zsync`, the server's file is the same and its partial file is there.
fn resumable(control: &Control, req: &Request, validators: &Validators) -> Option<State> {
    let state = State::load(&sibling(&req.output, ".shardload-state"))?;
    let part = std::fs::metadata(sibling(&req.output, ".shardload-part")).ok()?;
    let same = state.url == req.url
        && state.size == control.length
        && part.len() == control.length
        && &state.validators == validators
        && state
            .zsync
            .as_ref()
            .is_some_and(|z| z.control == control.fingerprint());
    same.then_some(state)
}

/// Starts the download: the blocks found in `seeds` copied into the partial file, and a
/// state whose segments are the ranges still to download, saved.
async fn start(
    control: &Control,
    seeds: &[PathBuf],
    req: &Request,
    validators: Validators,
    cancel: &CancellationToken,
) -> Result<State> {
    let found = match_seeds(control, seeds, cancel).await?;
    let (reused, from) = {
        let mut file = std::fs::File::create(sibling(&req.output, ".shardload-part"))?;
        file.set_len(control.length)?;
        let copied = copy_blocks(control, seeds, &found, &mut file)?;
        file.sync_all()?;
        copied
    };
    let segments = ranges(control, &found)
        .into_iter()
        .map(|(start, end)| SavedSegment {
            start,
            end,
            written: start,
            speed: 0,
        })
        .collect();
    let mut state = State::new(&req.url, control.length, validators, segments);
    // Every byte it counts is on the disk (see `Missing::fetch`).
    state.clean = true;
    state.zsync = Some(ZsyncState {
        control: control.fingerprint(),
        reused,
        from,
    });
    state.save(&sibling(&req.output, ".shardload-state"))?;
    Ok(state)
}

/// Where the ranges a zsync download still misses come from and go to.
struct Missing<'a> {
    http: &'a Http,
    remote: &'a crate::remote::Remote,
    part: &'a Path,
    state_path: &'a Path,
    opts: &'a Options,
    progress: Option<&'a watch::Sender<Progress>>,
}

impl Missing<'_> {
    /// Downloads what `state`'s segments still miss, saving the state as it goes.
    async fn fetch(&self, state: &mut State, cancel: &CancellationToken) -> Result<()> {
        let mut file = std::fs::OpenOptions::new().write(true).open(self.part)?;
        let left: u64 = state.segments.iter().map(|s| s.end - s.written).sum();
        let mut received = state.size - left;
        self.report(state.size, received);
        for i in 0..state.segments.len() {
            while state.segments[i].written < state.segments[i].end {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let at = state.segments[i].written;
                let to = (at + MAX_RANGE).min(state.segments[i].end);
                let bytes = self.http.fetch(self.remote, at, to, 0).await?;
                if bytes.len() as u64 != to - at {
                    return Err(Error::Truncated);
                }
                for l in &self.opts.limiters {
                    l.acquire(bytes.len()).await;
                }
                file.seek(SeekFrom::Start(at))?;
                file.write_all(&bytes)?;
                // On the disk before the state counts it: resuming after anything, a power
                // loss too, downloads only what is missing. A few MB each time, so not often.
                file.sync_data()?;
                state.segments[i].written = to;
                state.save(self.state_path)?;
                received += bytes.len() as u64;
                self.report(state.size, received);
            }
        }
        Ok(())
    }

    fn report(&self, size: u64, received: u64) {
        if let Some(p) = self.progress {
            p.send_modify(|p| {
                p.phase = Phase::Downloading;
                p.size = Some(size);
                p.received = received;
                p.connections = 1;
            });
        }
    }
}

async fn whole_file(
    req: &Request,
    opts: &Options,
    progress: Option<&watch::Sender<Progress>>,
    cancel: &CancellationToken,
) -> Result<Delta> {
    Ok(Delta {
        outcome: crate::download(req, opts, progress, cancel).await?,
        reused: 0,
        from: None,
    })
}

/// For every block, a seed and the offset in it it was found at.
async fn match_seeds(
    control: &Control,
    seeds: &[PathBuf],
    cancel: &CancellationToken,
) -> Result<Vec<Option<(usize, u64)>>> {
    let mut found = vec![None; control.blocks.len()];
    for (s, seed) in seeds.iter().enumerate() {
        let (c, seed) = (control.clone(), seed.clone());
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let watching = std::sync::Arc::clone(&stop);
        let job = tokio::task::spawn_blocking(move || find_blocks(&c, &seed, &watching));
        let here = tokio::select! {
            r = job => r.map_err(|e| Error::State(format!("zsync: {e}")))?,
            () = cancel.cancelled() => {
                stop.store(true, Ordering::Relaxed);
                return Err(Error::Cancelled);
            }
        };
        // A seed that cannot be read is no help, and no harm.
        let Ok(here) = here else { continue };
        for (slot, at) in found.iter_mut().zip(here) {
            if slot.is_none() {
                *slot = at.map(|at| (s, at));
            }
        }
    }
    Ok(found)
}

/// Copies every block found into `file`; the bytes copied, and the seed most came from.
fn copy_blocks(
    control: &Control,
    seeds: &[PathBuf],
    found: &[Option<(usize, u64)>],
    file: &mut std::fs::File,
) -> Result<(u64, Option<PathBuf>)> {
    let mut opened: HashMap<usize, std::fs::File> = HashMap::new();
    let mut per_seed: HashMap<usize, u64> = HashMap::new();
    let mut buf = vec![0u8; control.blocksize];
    let mut copied = 0;
    for (i, at) in found.iter().enumerate() {
        let Some((s, at)) = *at else { continue };
        let (start, end) = control.block(i);
        let len = (end - start) as usize;
        let seed = match opened.entry(s) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(std::fs::File::open(&seeds[s])?)
            }
        };
        seed.seek(SeekFrom::Start(at))?;
        // Past the seed's end the block was matched as zeros.
        let n = read_up_to(seed, &mut buf[..len])?;
        buf[n..len].fill(0);
        file.seek(SeekFrom::Start(start))?;
        file.write_all(&buf[..len])?;
        copied += len as u64;
        *per_seed.entry(s).or_default() += len as u64;
    }
    let most = per_seed
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(s, _)| seeds[s].clone());
    Ok((copied, most))
}

fn read_up_to(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// The blocks not found, as ranges of the file, neighbours joined.
fn ranges(control: &Control, found: &[Option<(usize, u64)>]) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (i, f) in found.iter().enumerate() {
        if f.is_some() {
            continue;
        }
        let (start, end) = control.block(i);
        match out.last_mut() {
            Some(last) if last.1 == start => last.1 = end,
            _ => out.push((start, end)),
        }
    }
    out
}
