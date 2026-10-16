//! Moving a download to another link of the same file: a fresh link from the browser after
//! the old one expired, or the original URL resolved again after a redirect target expired.
//!
//! Validators are not trusted for this: mirrors and CDN nodes serve the same file with
//! different ETags. Instead the new link must report the same size and serve the same bytes
//! as a sample the download already has on disk.

use std::path::Path;

use crate::error::{Error, Result};
use crate::remote::{Http, Remote};
use crate::segments::SavedSegment;
use crate::state::{State, sibling};
use crate::{Options, Request};

/// How much already-downloaded data is compared with the new link.
const SAMPLE: u64 = 64 << 10;

/// Bytes already on disk, `(offset, data)`, to compare a candidate link against.
pub(crate) type Sample = Option<(u64, Vec<u8>)>;

/// Where to take the sample from: the start of the first segment with data.
pub(crate) fn sample_region(segments: &[SavedSegment]) -> Option<(u64, u64)> {
    segments
        .iter()
        .find(|s| s.written > s.start)
        .map(|s| (s.start, s.written.min(s.start + SAMPLE)))
}

/// Reads the sample from a partial file that is not open for writing.
pub(crate) fn read_sample(part: &Path, segments: &[SavedSegment]) -> Result<Sample> {
    let Some((start, end)) = sample_region(segments) else {
        return Ok(None);
    };
    let file = std::fs::File::open(part)?;
    let data = crate::file::read_exact_at(&file, (end - start) as usize, start)?;
    Ok(Some((start, data)))
}

/// Whether `candidate` serves the file the download holds part of.
pub(crate) async fn same_content(
    http: &Http,
    candidate: &Remote,
    size: u64,
    sample: &Sample,
    slot: u64,
) -> Result<bool> {
    if !candidate.ranges || candidate.size != Some(size) {
        return Ok(false);
    }
    let Some((start, data)) = sample else {
        return Ok(true); // nothing on disk yet: the size is all there is to compare
    };
    let served = http
        .fetch(candidate, *start, start + data.len() as u64, slot)
        .await?;
    Ok(served == *data)
}

/// Points an interrupted download (`req.output`) at a new link (`req.url`), after checking
/// that the link serves the same file. Downloading with the new link then resumes.
///
/// `download` does the same check by itself when called with a different URL than the
/// interrupted attempt used; this is for confirming a refreshed link before resuming, e.g.
/// right after the browser captured it.
pub async fn patch_url(req: &Request, opts: &Options) -> Result<()> {
    let _lock = crate::place::Lock::acquire(&req.output)?;
    let state_path = sibling(&req.output, ".idma-state");
    let part = sibling(&req.output, ".idma-part");
    let mut state = State::load(&state_path).ok_or_else(|| {
        Error::State(format!(
            "no interrupted download at {}",
            req.output.display()
        ))
    })?;
    let http = Http::new(&crate::download::http_config(&req.headers, opts))?;
    let (remote, resp) = http.probe(&req.url, 0, None, 0).await?;
    drop(resp);
    let sample = read_sample(&part, &state.segments)?;
    if !same_content(&http, &remote, state.size, &sample, 1).await? {
        return Err(Error::FileChanged);
    }
    state.url.clone_from(&req.url);
    state.validators = remote.validators;
    state.save(&state_path)?;
    Ok(())
}
