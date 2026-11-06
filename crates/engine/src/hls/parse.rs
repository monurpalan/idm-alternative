//! Playlists: master and media playlists read into their variants, renditions and
//! segments (keys, byte ranges, init sections), and what a stream cannot be (DRM).

use super::*;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Segment {
    pub url: String,
    /// `[start, end)` within `url` (`EXT-X-BYTERANGE`).
    pub range: Option<(u64, u64)>,
    pub key: Option<Key>,
    /// Seconds it plays (`EXTINF`), when known.
    pub duration: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Key {
    pub url: String,
    pub iv: [u8; 16],
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct MediaPlaylist {
    /// fMP4 initialisation segment (`EXT-X-MAP`), joined first.
    pub init: Option<Segment>,
    /// Initialisation segments that take over later in the list (an ad break in another
    /// encoding): each with the index of the first segment it is for.
    pub maps: Vec<(usize, Segment)>,
    pub segments: Vec<Segment>,
    /// The media sequence number of the first segment (`EXT-X-MEDIA-SEQUENCE`).
    pub sequence: u64,
    /// `#EXT-X-ENDLIST`: no segment comes after these. A live stream's playlist has none.
    pub ended: bool,
    /// `EXT-X-TARGETDURATION`: the longest segment, in seconds; how often a live stream's
    /// playlist is read again.
    pub target: Option<f64>,
}

/// What a playlist is: segments, or (a master playlist) the best variant to follow.
#[derive(Debug, PartialEq)]
pub(crate) enum Playlist {
    Media(MediaPlaylist),
    Best {
        video: String,
        audio: Option<String>,
    },
}

pub(crate) fn attr<'a>(list: &'a str, key: &str) -> Option<&'a str> {
    // Good enough for the attributes read here (no commas inside the quoted ones we need).
    let mut rest = list;
    while let Some(eq) = rest.find('=') {
        let k = rest[..eq].trim().trim_start_matches(',').trim();
        let after = &rest[eq + 1..];
        let (value, next) = if let Some(q) = after.strip_prefix('"') {
            let end = q.find('"').unwrap_or(q.len());
            (&q[..end], q.get(end + 1..).unwrap_or(""))
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (&after[..end], &after[end..])
        };
        if k.eq_ignore_ascii_case(key) {
            return Some(value);
        }
        rest = next;
    }
    None
}

pub(crate) fn resolve(base: &str, rel: &str) -> String {
    url::Url::parse(base)
        .and_then(|b| b.join(rel))
        .map_or_else(|_| rel.to_owned(), |u| u.to_string())
}

/// `0x1234…` → 16 bytes, right-aligned.
pub(crate) fn hex_iv(s: &str) -> Option<[u8; 16]> {
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    let n = u128::from_str_radix(hex, 16).ok()?;
    Some(n.to_be_bytes())
}

pub(crate) fn parse_playlist(url: &str, text: &str) -> Result<Playlist> {
    let p = parse_any(url, text)?;
    if let Playlist::Media(m) = &p
        && !m.ended
    {
        return Err(Error::State(
            "this is a live stream (the playlist has no end): record it instead".into(),
        ));
    }
    Ok(p)
}

/// A playlist, a live one too.
pub(crate) fn parse_any(url: &str, text: &str) -> Result<Playlist> {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim().trim_start_matches('\u{feff}'))
        .filter(|l| !l.is_empty())
        .collect();
    if lines.first() != Some(&"#EXTM3U") {
        return Err(Error::State(format!("{url} is not an HLS playlist")));
    }

    if let Some(best) = master(url, &lines) {
        return Ok(best);
    }

    let mut segments = Vec::new();
    let mut init: Option<Segment> = None;
    let mut maps: Vec<(usize, Segment)> = Vec::new();
    let mut key: Option<(String, Option<[u8; 16]>)> = None;
    let mut sequence: u64 = 0;
    let mut range: Option<(u64, u64)> = None;
    let mut last_end: HashMap<String, u64> = HashMap::new();
    let mut ended = false;
    let mut in_segment = false;
    let mut duration: Option<f64> = None;
    let mut first_sequence = None;
    let mut target = None;
    for line in &lines {
        if let Some(n) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            sequence = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target = n.trim().parse::<f64>().ok().filter(|t| *t > 0.0);
        } else if let Some(a) = line.strip_prefix("#EXT-X-KEY:") {
            key = match attr(a, "METHOD") {
                None | Some("NONE") => None,
                Some("AES-128") => {
                    let uri = attr(a, "URI")
                        .ok_or_else(|| Error::State("AES-128 key without URI".into()))?;
                    Some((resolve(url, uri), attr(a, "IV").and_then(hex_iv)))
                }
                Some(other) => {
                    return Err(Error::State(format!(
                        "segments encrypted with {other} cannot be downloaded"
                    )));
                }
            };
        } else if let Some(a) = line.strip_prefix("#EXT-X-MAP:") {
            let uri = attr(a, "URI").ok_or_else(|| Error::State("EXT-X-MAP without URI".into()))?;
            let range = attr(a, "BYTERANGE").and_then(|r| byte_range(r, 0));
            let map = Segment {
                url: resolve(url, uri),
                range,
                key: None,
                duration: None,
            };
            let current = maps.last().map(|(_, m)| m).or(init.as_ref());
            if segments.is_empty() {
                init = Some(map);
            } else if current != Some(&map) {
                maps.push((segments.len(), map));
            }
        } else if let Some(r) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            // (offset, length); an offset of MAX means "where the previous range of this
            // URI ended", resolved once the URI is known.
            range = parse_range_spec(r).map(|(len, off)| (off.unwrap_or(u64::MAX), len));
        } else if let Some(info) = line.strip_prefix("#EXTINF:") {
            in_segment = true;
            duration = info
                .split(',')
                .next()
                .and_then(|d| d.trim().parse::<f64>().ok())
                .filter(|d| d.is_finite() && *d >= 0.0);
        } else if *line == "#EXT-X-ENDLIST" {
            ended = true;
        } else if !line.starts_with('#') && in_segment {
            let seg_url = resolve(url, line);
            let byte_range = range.take().map(|(off, len)| {
                let start = if off == u64::MAX {
                    *last_end.get(&seg_url).unwrap_or(&0)
                } else {
                    off
                };
                (start, start + len)
            });
            if let Some((_, end)) = byte_range {
                last_end.insert(seg_url.clone(), end);
            }
            let k = key.as_ref().map(|(u, iv)| Key {
                url: u.clone(),
                // Without an IV attribute, the segment's sequence number (RFC 8216 §5.2).
                iv: iv.unwrap_or_else(|| u128::from(sequence).to_be_bytes()),
            });
            first_sequence.get_or_insert(sequence);
            segments.push(Segment {
                url: seg_url,
                range: byte_range,
                key: k,
                duration: duration.take(),
            });
            sequence += 1;
            in_segment = false;
        }
    }
    // A live stream may list nothing yet; it fills in.
    if segments.is_empty() && ended {
        return Err(Error::State("the playlist has no segments".into()));
    }
    Ok(Playlist::Media(MediaPlaylist {
        init,
        maps,
        segments,
        sequence: first_sequence.unwrap_or(sequence),
        ended,
        target,
    }))
}

/// A master playlist: the variant with the most pixels, then bandwidth, with its audio.
fn master(url: &str, lines: &[&str]) -> Option<Playlist> {
    let mut best: Option<(u64, u64, String, Option<String>)> = None;
    let mut audio_groups: HashMap<String, String> = HashMap::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(a) = line.strip_prefix("#EXT-X-MEDIA:")
            && attr(a, "TYPE") == Some("AUDIO")
            && let (Some(group), Some(uri)) = (attr(a, "GROUP-ID"), attr(a, "URI"))
        {
            let default = attr(a, "DEFAULT") == Some("YES");
            if default || !audio_groups.contains_key(group) {
                audio_groups.insert(group.to_owned(), resolve(url, uri));
            }
        }
        if let Some(a) = line.strip_prefix("#EXT-X-STREAM-INF:")
            && let Some(uri) = lines[i + 1..].iter().find(|l| !l.starts_with('#'))
        {
            let pixels = attr(a, "RESOLUTION")
                .and_then(|r| r.split_once('x'))
                .and_then(|(w, h)| Some(w.parse::<u64>().ok()? * h.parse::<u64>().ok()?))
                .unwrap_or(0);
            let bw = attr(a, "BANDWIDTH")
                .and_then(|b| b.parse().ok())
                .unwrap_or(0);
            if best
                .as_ref()
                .is_none_or(|(p, b, ..)| (pixels, bw) > (*p, *b))
            {
                best = Some((
                    pixels,
                    bw,
                    resolve(url, uri),
                    attr(a, "AUDIO").map(str::to_owned),
                ));
            }
        }
    }
    let (_, _, video, group) = best?;
    let audio = group.and_then(|g| audio_groups.get(&g).cloned());
    Some(Playlist::Best { video, audio })
}

/// `length[@offset]`.
pub(crate) fn parse_range_spec(r: &str) -> Option<(u64, Option<u64>)> {
    let (len, off) = match r.trim().split_once('@') {
        Some((l, o)) => (l, Some(o.parse().ok()?)),
        None => (r.trim(), None),
    };
    Some((len.parse().ok()?, off))
}

pub(crate) fn byte_range(r: &str, default_start: u64) -> Option<(u64, u64)> {
    let (len, off) = parse_range_spec(r)?;
    let start = off.unwrap_or(default_start);
    Some((start, start + len))
}
