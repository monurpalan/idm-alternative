//! Where a DASH representation's media is: the URLs (and byte ranges) of its initialisation
//! and media segments, from `SegmentTemplate` (with `$Number$` or `SegmentTimeline` and
//! `$Time$`), `SegmentList`, or a single file (`SegmentBase` / a plain `BaseURL`). A live
//! (dynamic) manifest is read with [`dash_live`]: the segments it offers at that moment.

use roxmltree::{Document, Node};

use crate::dash::{iso_datetime, iso_duration};
use crate::{Error, resolve};

/// A piece of media: a whole resource, or `[start, end)` of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub url: String,
    pub range: Option<(u64, u64)>,
    /// How long it plays, in seconds, when the manifest says.
    pub duration: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DashTrack {
    pub init: Option<Piece>,
    pub segments: Vec<Piece>,
    /// The representation is one file (`SegmentBase` or only a `BaseURL`): `segments` holds
    /// it whole, and it is worth downloading with several connections.
    pub single: bool,
    /// The number of `segments[0]` (`startNumber`); the others follow one by one.
    pub first: u64,
}

/// A live manifest as read at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct DashLive {
    pub track: DashTrack,
    /// The broadcast is over (the manifest became `static`): nothing more will come.
    pub ended: bool,
    /// How long to wait before reading the manifest again (`minimumUpdatePeriod`), seconds.
    pub refresh: Option<f64>,
}

/// How far back a live `$Number$` template is read when the manifest does not say
/// (`timeShiftBufferDepth`): a few segments behind the newest.
const LIVE_DEPTH: f64 = 30.0;
/// More segments than a manifest may list: a day of one-second segments five times over. A
/// hostile manifest (a duration of 10^90 seconds, a repeat count of 2^63) would otherwise
/// keep the parser filling memory.
const MAX_SEGMENTS: usize = 500_000;

/// The widest `%0Nd` a template's number is padded to.
const MAX_WIDTH: usize = 20;

fn too_many() -> Error {
    Error(format!(
        "the manifest lists more than {MAX_SEGMENTS} segments"
    ))
}

fn child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
}

fn children<'a, 'i>(node: Node<'a, 'i>, name: &'static str) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == name)
}

/// `"100-199"` (inclusive) → `(100, 200)`.
fn byte_range(r: &str) -> Option<(u64, u64)> {
    let (a, b) = r.trim().split_once('-')?;
    let (a, b): (u64, u64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (b >= a).then_some((a, b + 1))
}

/// Fills a template: `$RepresentationID$`, `$Bandwidth$`, `$Number$`, `$Time$` (each
/// optionally `%0Nd`), and `$$`.
pub(crate) fn fill(template: &str, rep: &str, bandwidth: u64, number: u64, time: u64) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('$') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let token = &after[..end];
        let (name, format) = token.split_once('%').unwrap_or((token, ""));
        let value = match name {
            "" => Some("$".to_owned()),
            "RepresentationID" => Some(rep.to_owned()),
            "Bandwidth" => Some(bandwidth.to_string()),
            "Number" => Some(number.to_string()),
            "Time" => Some(time.to_string()),
            _ => None,
        };
        match value {
            Some(v) => {
                // `%05d`: pad with zeros to width 5. No real manifest asks for more than a few
                // digits; a width of millions would make every segment's address that long.
                let width = format
                    .trim_start_matches('0')
                    .trim_end_matches('d')
                    .parse::<usize>()
                    .unwrap_or(0)
                    .min(MAX_WIDTH);
                if width > v.len() && !name.is_empty() {
                    out.push_str(&"0".repeat(width - v.len()));
                }
                out.push_str(&v);
            }
            None => {
                out.push('$');
                out.push_str(token);
                out.push('$');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The attribute from the representation's template, else the adaptation set's.
fn attr<'a>(templates: &[Node<'a, '_>], name: &str) -> Option<&'a str> {
    templates.iter().find_map(|t| t.attribute(name))
}

/// The segments of representation `rep_id` in the manifest at `url`.
pub fn dash_track(url: &str, text: &str, rep_id: &str) -> Result<DashTrack, Error> {
    let doc = Document::parse(text).map_err(|e| Error(format!("not a DASH manifest: {e}")))?;
    track(url, doc.root_element(), rep_id, None)
}

/// The segments of representation `rep_id` that a live manifest offers at `now` (Unix
/// seconds): the ones listed (`SegmentTimeline`, `SegmentList`), or for a `$Number$`
/// template the ones out by the clock since `availabilityStartTime`, as far back as
/// `timeShiftBufferDepth`. A manifest that is `static` (the broadcast ended) gives all of
/// its segments and says it ended.
pub fn dash_live(url: &str, text: &str, rep_id: &str, now: f64) -> Result<DashLive, Error> {
    let doc = Document::parse(text).map_err(|e| Error(format!("not a DASH manifest: {e}")))?;
    let mpd = doc.root_element();
    let live = mpd.attribute("type") == Some("dynamic");
    Ok(DashLive {
        track: track(url, mpd, rep_id, live.then_some(now))?,
        ended: !live,
        refresh: mpd.attribute("minimumUpdatePeriod").and_then(iso_duration),
    })
}

/// `now` is set for a live manifest.
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "one long sequence kept together; to be split when it next changes"
)]
fn track(url: &str, mpd: Node, rep_id: &str, now: Option<f64>) -> Result<DashTrack, Error> {
    let period = child(mpd, "Period").ok_or_else(|| Error("the manifest has no period".into()))?;
    let (set, rep) = children(period, "AdaptationSet")
        .find_map(|set| {
            children(set, "Representation")
                .find(|r| r.attribute("id") == Some(rep_id))
                .map(|r| (set, r))
        })
        .ok_or_else(|| Error(format!("the manifest has no representation {rep_id}")))?;

    // BaseURLs nest: each level resolves against the one above.
    let mut base = url.to_owned();
    for node in [mpd, period, set, rep] {
        if let Some(b) = child(node, "BaseURL").and_then(|b| b.text()) {
            base = resolve(&base, b.trim());
        }
    }
    let bandwidth = rep
        .attribute("bandwidth")
        .and_then(|b| b.parse().ok())
        .unwrap_or(0);
    let duration = period
        .attribute("duration")
        .or_else(|| mpd.attribute("mediaPresentationDuration"))
        .and_then(iso_duration);
    let period_start = period
        .attribute("start")
        .and_then(iso_duration)
        .unwrap_or(0.0);

    let templates: Vec<Node> = [child(rep, "SegmentTemplate"), child(set, "SegmentTemplate")]
        .into_iter()
        .flatten()
        .collect();
    if !templates.is_empty() {
        let media = attr(&templates, "media")
            .ok_or_else(|| Error("SegmentTemplate without media".into()))?;
        let timescale: u64 = attr(&templates, "timescale")
            .and_then(|t| t.parse().ok())
            .unwrap_or(1)
            .max(1);
        let start_number: u64 = attr(&templates, "startNumber")
            .and_then(|n| n.parse().ok())
            .unwrap_or(1);
        let piece = |t: &str, number, time, ticks: Option<f64>| Piece {
            url: resolve(&base, &fill(t, rep_id, bandwidth, number, time)),
            range: None,
            duration: ticks.map(|d| d / timescale as f64),
        };
        let init = attr(&templates, "initialization").map(|i| piece(i, 0, 0, None));

        // Where the media timeline is now, in ticks, for a live manifest.
        let live_ticks = match now {
            Some(now) => {
                let start = mpd
                    .attribute("availabilityStartTime")
                    .and_then(iso_datetime)
                    .ok_or_else(|| Error("a live manifest without its start time".into()))?;
                let offset: u64 = attr(&templates, "presentationTimeOffset")
                    .and_then(|o| o.parse().ok())
                    .unwrap_or(0);
                Some((
                    ((now - start - period_start).max(0.0) * timescale as f64) as u64,
                    offset,
                ))
            }
            None => None,
        };
        let mut segments = Vec::new();
        let mut first = start_number;
        if let Some(timeline) = templates.iter().find_map(|t| child(*t, "SegmentTimeline")) {
            let end = match live_ticks {
                Some((ticks, offset)) => Some(offset + ticks),
                None => duration.map(|d| (d * timescale as f64).round() as u64),
            };
            let mut time = 0u64;
            let mut number = start_number;
            let entries: Vec<Node> = children(timeline, "S").collect();
            for (i, s) in entries.iter().enumerate() {
                if let Some(t) = s.attribute("t").and_then(|t| t.parse().ok()) {
                    time = t;
                }
                let d: u64 = s
                    .attribute("d")
                    .and_then(|d| d.parse().ok())
                    .filter(|d| *d > 0)
                    .ok_or_else(|| Error("SegmentTimeline entry without a duration".into()))?;
                let r: i64 = s.attribute("r").and_then(|r| r.parse().ok()).unwrap_or(0);
                // r = -1: repeat until the next entry's start, or the end of the period.
                let until = if r < 0 {
                    entries
                        .get(i + 1)
                        .and_then(|n| n.attribute("t"))
                        .and_then(|t| t.parse().ok())
                        .or(end)
                        .ok_or_else(|| Error("an open-ended timeline needs a duration".into()))?
                } else {
                    time.saturating_add(d.saturating_mul(r as u64 + 1))
                };
                if until.saturating_sub(time) / d >= (MAX_SEGMENTS - segments.len()) as u64 {
                    return Err(too_many());
                }
                while time < until {
                    segments.push(piece(media, number, time, Some(d as f64)));
                    time = time.saturating_add(d);
                    number = number.saturating_add(1);
                }
            }
        } else {
            let seg: f64 = attr(&templates, "duration")
                .and_then(|d| d.parse().ok())
                .filter(|d: &f64| *d > 0.0)
                .ok_or_else(|| Error("SegmentTemplate without duration or timeline".into()))?;
            // Live: the segments complete by now, as far back as the manifest keeps them.
            let (from, count) = match live_ticks {
                Some((ticks, _)) => {
                    let out = (ticks as f64 / seg).floor() as u64;
                    let depth = mpd
                        .attribute("timeShiftBufferDepth")
                        .and_then(iso_duration)
                        .unwrap_or(LIVE_DEPTH);
                    let keep = ((depth * timescale as f64 / seg).floor() as u64).max(1);
                    (out.saturating_sub(keep), out)
                }
                None => {
                    let total = duration
                        .ok_or_else(|| Error("the manifest does not say how long it is".into()))?;
                    (0, (total * timescale as f64 / seg).ceil() as u64)
                }
            };
            if count.saturating_sub(from) > MAX_SEGMENTS as u64 {
                return Err(too_many());
            }
            first = start_number.saturating_add(from);
            for i in from..count {
                segments.push(piece(
                    media,
                    start_number.saturating_add(i),
                    (i as f64 * seg) as u64,
                    Some(seg),
                ));
            }
        }
        // A live stream may not have its first segment out yet.
        if segments.is_empty() && now.is_none() {
            return Err(Error("the representation has no segments".into()));
        }
        return Ok(DashTrack {
            init,
            segments,
            single: false,
            first,
        });
    }

    if let Some(list) = child(rep, "SegmentList").or_else(|| child(set, "SegmentList")) {
        let init = child(list, "Initialization").map(|i| Piece {
            url: i
                .attribute("sourceURL")
                .map_or_else(|| base.clone(), |s| resolve(&base, s)),
            range: i.attribute("range").and_then(byte_range),
            duration: None,
        });
        let timescale: f64 = list
            .attribute("timescale")
            .and_then(|t| t.parse().ok())
            .filter(|t: &f64| *t > 0.0)
            .unwrap_or(1.0);
        let each = list
            .attribute("duration")
            .and_then(|d| d.parse::<f64>().ok())
            .map(|d| d / timescale);
        let segments: Vec<Piece> = children(list, "SegmentURL")
            .map(|s| Piece {
                url: s
                    .attribute("media")
                    .map_or_else(|| base.clone(), |m| resolve(&base, m)),
                range: s.attribute("mediaRange").and_then(byte_range),
                duration: each,
            })
            .collect();
        if segments.is_empty() && now.is_none() {
            return Err(Error("the segment list is empty".into()));
        }
        return Ok(DashTrack {
            init,
            segments,
            single: false,
            first: list
                .attribute("startNumber")
                .and_then(|n| n.parse().ok())
                .unwrap_or(1),
        });
    }

    // SegmentBase, or nothing: the BaseURL is the whole file.
    Ok(DashTrack {
        init: None,
        segments: vec![Piece {
            url: base,
            range: None,
            duration,
        }],
        single: true,
        first: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates() {
        assert_eq!(fill("seg-$Number%05d$.m4s", "v1", 0, 7, 0), "seg-00007.m4s");
        assert_eq!(
            fill(
                "$RepresentationID$/$Bandwidth$/$Time$.m4s",
                "v1",
                800,
                0,
                9000
            ),
            "v1/800/9000.m4s"
        );
        assert_eq!(fill("a$$b$Unknown$", "v", 0, 0, 0), "a$b$Unknown$");
        // Found by fuzzing: a width of hundreds of millions, for every segment.
        assert_eq!(fill("$Number%0777777775d$", "v", 0, 7, 0).len(), MAX_WIDTH);
    }

    #[test]
    fn number_template_with_inheritance_and_base_urls() {
        let mpd = r#"<MPD mediaPresentationDuration="PT10S"><BaseURL>cdn/</BaseURL>
          <Period><AdaptationSet mimeType="video/mp4">
            <BaseURL>video/</BaseURL>
            <SegmentTemplate timescale="1000" duration="4000" startNumber="0"
              initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/$Number$.m4s"/>
            <Representation id="v720" bandwidth="1"/>
            <Representation id="v360" bandwidth="1"><SegmentTemplate startNumber="5"/></Representation>
          </AdaptationSet></Period></MPD>"#;
        let t = dash_track("https://h/show/m.mpd", mpd, "v720").expect("track");
        assert_eq!(
            t.init.expect("init").url,
            "https://h/show/cdn/video/v720/init.mp4"
        );
        // 10 s in 4 s segments: 3.
        let urls: Vec<_> = t.segments.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://h/show/cdn/video/v720/0.m4s",
                "https://h/show/cdn/video/v720/1.m4s",
                "https://h/show/cdn/video/v720/2.m4s"
            ]
        );
        assert!(!t.single);
        // The representation's own template overrides the start number.
        let t = dash_track("https://h/show/m.mpd", mpd, "v360").expect("track");
        assert_eq!(t.segments[0].url, "https://h/show/cdn/video/v360/5.m4s");
    }

    #[test]
    fn timeline_with_repeats() {
        let mpd = r#"<MPD mediaPresentationDuration="PT20S"><Period><AdaptationSet>
          <SegmentTemplate timescale="90000" media="t/$Time$.m4s" initialization="i.mp4">
            <SegmentTimeline><S t="0" d="180000" r="2"/><S d="90000"/><S d="180000" r="-1"/></SegmentTimeline>
          </SegmentTemplate><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        let t = dash_track("https://h/x.mpd", mpd, "a").expect("track");
        let times: Vec<_> = t
            .segments
            .iter()
            .map(|s| {
                s.url
                    .trim_start_matches("https://h/t/")
                    .trim_end_matches(".m4s")
                    .to_owned()
            })
            .collect();
        // 3 × 2 s, then 1 s, then 2 s until 20 s.
        assert_eq!(
            times,
            [
                "0", "180000", "360000", "540000", "630000", "810000", "990000", "1170000",
                "1350000", "1530000", "1710000"
            ]
        );
    }

    #[test]
    fn live_number_template_follows_the_clock() {
        // Started at 12:00:00 UTC, 2 s segments, 10 s kept.
        let mpd = r#"<MPD type="dynamic" availabilityStartTime="2026-10-31T12:00:00Z"
            minimumUpdatePeriod="PT2S" timeShiftBufferDepth="PT10S"><Period start="PT0S">
            <AdaptationSet><SegmentTemplate timescale="1000" duration="2000" startNumber="1"
              initialization="i.mp4" media="$Number$.m4s"/>
            <Representation id="v" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        let noon = 1_793_448_000.0;
        // 7 s in: segments 1–3 are out (the 4th still being made).
        let l = dash_live("https://h/m.mpd", mpd, "v", noon + 7.0).expect("live");
        assert!(!l.ended);
        assert_eq!(l.refresh, Some(2.0));
        assert_eq!(l.track.first, 1);
        let urls: Vec<_> = l.track.segments.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            ["https://h/1.m4s", "https://h/2.m4s", "https://h/3.m4s"]
        );
        // A minute in: only the last 10 s are kept, 26–30.
        let l = dash_live("https://h/m.mpd", mpd, "v", noon + 61.0).expect("live");
        assert_eq!(l.track.first, 26);
        assert_eq!(l.track.segments.len(), 5);
        assert_eq!(l.track.segments[4].url, "https://h/30.m4s");
        // Before the first segment is out: nothing yet, no error.
        let l = dash_live("https://h/m.mpd", mpd, "v", noon + 1.0).expect("live");
        assert!(l.track.segments.is_empty());
        // Once the broadcast is over the manifest is static: everything, and it ended.
        let over = mpd.replace(
            r#"type="dynamic""#,
            r#"type="static" mediaPresentationDuration="PT6S""#,
        );
        let l = dash_live("https://h/m.mpd", &over, "v", noon + 99.0).expect("static");
        assert!(l.ended);
        assert_eq!(l.track.segments.len(), 3);
    }

    #[test]
    fn live_timeline_window() {
        let mpd = r#"<MPD type="dynamic" availabilityStartTime="2026-10-31T12:00:00.500+00:00">
            <Period><AdaptationSet>
            <SegmentTemplate timescale="1000" startNumber="41" media="t$Time$.m4s">
              <SegmentTimeline><S t="80000" d="2000" r="2"/></SegmentTimeline>
            </SegmentTemplate><Representation id="v" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        let l = dash_live("https://h/m.mpd", mpd, "v", 1_793_448_090.0).expect("live");
        assert_eq!(l.track.first, 41);
        let urls: Vec<_> = l.track.segments.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://h/t80000.m4s",
                "https://h/t82000.m4s",
                "https://h/t84000.m4s"
            ]
        );
        // A live manifest without its start time cannot place `$Number$` segments.
        let bad = r#"<MPD type="dynamic"><Period><AdaptationSet><SegmentTemplate duration="2" media="$Number$"/><Representation id="v" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        assert!(dash_live("https://h/m.mpd", bad, "v", 0.0).is_err());
    }

    #[test]
    fn segment_list_and_single_file() {
        let mpd = r#"<MPD><Period><AdaptationSet>
          <Representation id="l" bandwidth="1"><BaseURL>file.mp4</BaseURL>
            <SegmentList><Initialization range="0-99"/>
              <SegmentURL mediaRange="100-199"/><SegmentURL media="other.m4s"/></SegmentList>
          </Representation>
          <Representation id="b" bandwidth="1"><BaseURL>whole.mp4</BaseURL><SegmentBase indexRange="0-500"/></Representation>
        </AdaptationSet></Period></MPD>"#;
        let l = dash_track("https://h/a/m.mpd", mpd, "l").expect("list");
        assert_eq!(
            l.init,
            Some(Piece {
                url: "https://h/a/file.mp4".into(),
                range: Some((0, 100)),
                duration: None
            })
        );
        assert_eq!(l.segments[0].range, Some((100, 200)));
        assert_eq!(l.segments[1].url, "https://h/a/other.m4s");
        let b = dash_track("https://h/a/m.mpd", mpd, "b").expect("base");
        assert!(b.single);
        assert_eq!(b.segments[0].url, "https://h/a/whole.mp4");
        assert!(dash_track("https://h/a/m.mpd", mpd, "nope").is_err());
    }

    #[test]
    fn endless_manifests_are_refused() {
        let long = r#"<MPD mediaPresentationDuration="PT9999999999999H"><Period><AdaptationSet>
          <SegmentTemplate media="$Number$.m4s" duration="4"/>
          <Representation id="v" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        assert!(dash_track("https://h/m.mpd", long, "v").is_err());
        let repeated = r#"<MPD><Period><AdaptationSet><SegmentTemplate media="$Time$.m4s">
          <SegmentTimeline><S t="0" d="1" r="4611686018427387904"/></SegmentTimeline></SegmentTemplate>
          <Representation id="v" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        assert!(dash_track("https://h/m.mpd", repeated, "v").is_err());
    }

    /// Inputs that once made the parser fail under fuzzing (`fuzz/regressions/dash/`).
    #[test]
    fn fuzz_regressions() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/regressions/dash");
        for entry in std::fs::read_dir(dir).expect("the regression inputs") {
            let data = std::fs::read(entry.expect("an input").path()).expect("an input");
            let text = String::from_utf8_lossy(&data);
            for id in ["1", "2", "v", "a"] {
                let _ = dash_track("https://h/m.mpd", &text, id);
                let _ = dash_live("https://h/m.mpd", &text, id, 1.7e9);
            }
        }
    }
}
