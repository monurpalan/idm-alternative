//! Clips: a time range of a video, as typed (`1:30`, `1:02:03`, `90`), checked, and named.

pub use shardload_engine::Clip;

/// Seconds from `m:ss`, `h:mm:ss` or plain seconds (`90`, `12.5`); `None` for anything else.
pub fn parse_time(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() > 3 || parts.iter().any(|p| p.trim().is_empty()) {
        return None;
    }
    let mut seconds = 0.0;
    for (i, p) in parts.iter().enumerate() {
        let v: f64 = p
            .trim()
            .parse()
            .ok()
            .filter(|v: &f64| v.is_finite() && *v >= 0.0)?;
        // Minutes and seconds after the first part stay under 60.
        if i > 0 && v >= 60.0 {
            return None;
        }
        seconds = seconds * 60.0 + v;
    }
    Some(seconds)
}

/// A clip from two typed times, checked against the video's length when it is known.
pub fn from_times(
    start: &str,
    end: &str,
    exact: bool,
    length: Option<f64>,
) -> Result<Clip, String> {
    let start = parse_time(start).ok_or_else(|| format!("`{start}` is not a time (m:ss)"))?;
    let end = parse_time(end).ok_or_else(|| format!("`{end}` is not a time (m:ss)"))?;
    let clip = Clip { start, end, exact };
    check(&clip, length)?;
    Ok(clip)
}

/// A clip must start before it ends, and within the video.
pub fn check(clip: &Clip, length: Option<f64>) -> Result<(), String> {
    if !(clip.start >= 0.0 && clip.end > clip.start) {
        return Err("the clip must end after it starts".into());
    }
    if let Some(len) = length
        && clip.end > len + 0.5
    {
        return Err(format!("the video is only {} long", clock(len.floor())));
    }
    Ok(())
}

/// `1:05` or `1:02:03`.
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    match s / 3600 {
        0 => format!("{}:{:02}", s / 60, s % 60),
        h => format!("{h}:{:02}:{:02}", (s / 60) % 60, s % 60),
    }
}

/// What goes into the file name: ` (1.05-2.30)`, with dots for the colons file names cannot
/// have.
pub fn label(clip: &Clip) -> String {
    format!(
        " ({}-{})",
        clock(clip.start).replace(':', "."),
        clock(clip.end).replace(':', ".")
    )
}

/// yt-dlp's `--download-sections` for the clip.
pub fn section(clip: &Clip) -> String {
    format!("*{:.3}-{:.3}", clip.start, clip.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time("90"), Some(90.0));
        assert_eq!(parse_time("1:30"), Some(90.0));
        assert_eq!(parse_time(" 1:02:03 "), Some(3723.0));
        assert_eq!(parse_time("0:12.5"), Some(12.5));
        for bad in ["", "1:", "1:60", "a", "-1", "1:2:3:4", ":30"] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn clips() {
        let c = from_times("1:05", "2:30", false, Some(200.0)).expect("clip");
        assert_eq!((c.start, c.end), (65.0, 150.0));
        assert_eq!(label(&c), " (1.05-2.30)");
        assert_eq!(section(&c), "*65.000-150.000");
        assert!(from_times("2:00", "1:00", false, None).is_err());
        let long = from_times("0:10", "5:00", false, Some(120.0)).expect_err("too long");
        assert!(long.contains("2:00"), "{long}");
        assert_eq!(clock(3723.0), "1:02:03");
        assert_eq!(
            label(&Clip {
                start: 3600.0,
                end: 3725.0,
                exact: true
            }),
            " (1.00.00-1.02.05)"
        );
    }
}
