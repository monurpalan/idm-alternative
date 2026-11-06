//! A track whose initialisation segment changes part way: joined run by run.

use super::*;

/// A run of segments: the initialisation segment they are read with, and their files.
type Run = (Option<PathBuf>, Vec<(Segment, PathBuf)>);

/// Where the initialisation segment that takes over at segment `from` is kept.
pub(crate) fn map_file(dir: &Path, track: &str, from: usize) -> PathBuf {
    dir.join(format!("{track}-init-{from:06}.seg"))
}

/// A track whose initialisation segment changes part way (see [`MediaPlaylist::maps`]),
/// joined into `out` as MPEG-TS: each run of segments with its own initialisation segment,
/// remuxed by ffmpeg, then one after the other. Joined as they are, the later runs would be
/// read with the first run's initialisation, and come out broken or not at all.
pub(crate) async fn join_runs(
    dir: &Path,
    track: &str,
    playlist: &MediaPlaylist,
    clip: Option<Clip>,
    ffmpeg: &Path,
    out: &Path,
) -> Result<()> {
    let kept = clip.map(|c| window(playlist, c).0);
    let segment_file = |i: usize| dir.join(format!("{track}-{i:06}.seg"));
    let mut runs: Vec<Run> = Vec::new();
    let mut current = (playlist.init.is_some()).then(|| dir.join(format!("{track}-init.seg")));
    for (i, s) in playlist.segments.iter().enumerate() {
        if playlist.maps.iter().any(|(start, _)| *start == i) {
            current = Some(map_file(dir, track, i));
        }
        if kept.as_ref().is_some_and(|k| !k.contains(&i)) {
            continue;
        }
        match runs.last_mut() {
            Some((init, files)) if *init == current => files.push((s.clone(), segment_file(i))),
            _ => runs.push((current.clone(), vec![(s.clone(), segment_file(i))])),
        }
    }
    let mut parts = Vec::new();
    for (n, (init, files)) in runs.iter().enumerate() {
        let mp4 = dir.join(format!("{track}-run-{n}.mp4"));
        let all: Vec<(Segment, PathBuf)> = init
            .iter()
            .map(|p| (files[0].0.clone(), p.clone()))
            .chain(files.iter().cloned())
            .collect();
        join(&all, &mp4).await?;
        let ts = dir.join(format!("{track}-run-{n}.ts"));
        let output = tokio::process::Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&mp4)
            .args(["-map", "0", "-c", "copy", "-f", "mpegts"])
            .arg(&ts)
            .stdin(std::process::Stdio::null())
            .output()
            .await?;
        if !output.status.success() {
            return Err(Error::State(format!(
                "ffmpeg could not read part {} of the stream: {}",
                n + 1,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        tokio::fs::remove_file(&mp4).await.ok();
        parts.push((files[0].0.clone(), ts));
    }
    join(&parts, out).await?;
    for (_, ts) in &parts {
        tokio::fs::remove_file(ts).await.ok();
    }
    Ok(())
}
