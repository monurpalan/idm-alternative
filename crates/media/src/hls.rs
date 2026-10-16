//! HLS playlists (RFC 8216): a master playlist lists variants and alternative renditions; a
//! media playlist lists segments.

use crate::{Error, Format, Streams, Track, Variant, resolve, sort_best_first};

/// `KEY=value,KEY="quoted, with commas"` → pairs.
fn attributes(list: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = list.trim();
    while !rest.is_empty() {
        let Some((key, after)) = rest.split_once('=') else {
            break;
        };
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            let end = quoted.find('"').unwrap_or(quoted.len());
            let after_quote = &quoted[(end + 1).min(quoted.len())..];
            (quoted[..end].to_owned(), after_quote)
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (after[..end].to_owned(), &after[end..])
        };
        out.push((key.trim().to_ascii_uppercase(), value));
        rest = next.trim_start_matches([',', ' ']);
    }
    out
}

fn get<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

pub fn parse_m3u8(url: &str, text: &str) -> Result<Streams, Error> {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim().trim_start_matches('\u{feff}'))
        .filter(|l| !l.is_empty())
        .collect();
    if lines.first() != Some(&"#EXTM3U") {
        return Err(Error("not an HLS playlist (no #EXTM3U)".into()));
    }

    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut subtitles = Vec::new();
    let mut duration = 0.0;
    let mut segments = 0usize;
    let mut ended = false;
    let mut encryption: Option<String> = None;

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(attrs) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let a = attributes(attrs);
            // The variant's URI is the next line that is not a tag.
            let uri = lines[i + 1..].iter().find(|l| !l.starts_with('#'));
            if let Some(uri) = uri {
                let (width, height) = get(&a, "RESOLUTION")
                    .and_then(|r| r.split_once('x'))
                    .map_or((None, None), |(w, h)| (w.parse().ok(), h.parse().ok()));
                video.push(Variant {
                    id: resolve(url, uri),
                    bandwidth: get(&a, "BANDWIDTH")
                        .or_else(|| get(&a, "AVERAGE-BANDWIDTH"))
                        .and_then(|b| b.parse().ok())
                        .unwrap_or(0),
                    width,
                    height,
                    codecs: get(&a, "CODECS").map(str::to_owned),
                    frame_rate: get(&a, "FRAME-RATE").and_then(|f| f.parse().ok()),
                    audio_group: get(&a, "AUDIO").map(str::to_owned),
                });
            }
        } else if let Some(attrs) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = attributes(attrs);
            let track = Track {
                id: get(&a, "URI").map(|u| resolve(url, u)),
                name: get(&a, "NAME").map(str::to_owned),
                language: get(&a, "LANGUAGE").map(str::to_owned),
                group: get(&a, "GROUP-ID").map(str::to_owned),
                default: get(&a, "DEFAULT") == Some("YES"),
                bandwidth: None,
                codecs: None,
            };
            match get(&a, "TYPE") {
                Some("AUDIO") => audio.push(track),
                Some("SUBTITLES") => subtitles.push(track),
                _ => {}
            }
        } else if let Some(d) = line.strip_prefix("#EXTINF:") {
            segments += 1;
            duration += d
                .split(',')
                .next()
                .and_then(|n| n.trim().parse::<f64>().ok())
                .unwrap_or(0.0);
        } else if line == "#EXT-X-ENDLIST" {
            ended = true;
        } else if let Some(attrs) = line
            .strip_prefix("#EXT-X-KEY:")
            .or_else(|| line.strip_prefix("#EXT-X-SESSION-KEY:"))
        {
            let a = attributes(attrs);
            match get(&a, "METHOD") {
                Some("NONE") | None => {}
                Some(method) => {
                    // A key format other than the plain one is DRM (FairPlay, Widevine…).
                    let format = get(&a, "KEYFORMAT").unwrap_or("identity");
                    encryption = Some(if format == "identity" {
                        method.to_owned()
                    } else {
                        "DRM".to_owned()
                    });
                }
            }
        }
        i += 1;
    }

    let master = !video.is_empty();
    if !master && segments == 0 {
        return Err(Error(
            "the playlist lists neither variants nor segments".into(),
        ));
    }
    if !master {
        // A media playlist: one quality, as is.
        video.push(Variant {
            id: url.to_owned(),
            bandwidth: 0,
            width: None,
            height: None,
            codecs: None,
            frame_rate: None,
            audio_group: None,
        });
    }
    sort_best_first(&mut video);
    Ok(Streams {
        format: Format::Hls,
        video,
        audio,
        subtitles,
        duration: (!master && ended).then_some(duration),
        live: !master && !ended,
        encryption,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: &str = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",NAME="English",LANGUAGE="en",DEFAULT=YES,URI="audio/en.m3u8"
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",NAME="Türkçe",LANGUAGE="tr",URI="audio/tr.m3u8"
#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID="subs",NAME="English, CC",LANGUAGE="en",URI="subs/en.m3u8"
#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS="avc1.4d401e,mp4a.40.2",AUDIO="aud",SUBTITLES="subs"
360/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5000000,AVERAGE-BANDWIDTH=4500000,RESOLUTION=1920x1080,FRAME-RATE=29.970,CODECS="avc1.640028,mp4a.40.2",AUDIO="aud"
1080/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,AUDIO="aud"

https://other.cdn/720.m3u8
"#;

    #[test]
    fn master_playlist() {
        let s = parse_m3u8("https://cdn.x/show/master.m3u8?token=9", MASTER).expect("parse");
        assert_eq!(s.format, Format::Hls);
        let heights: Vec<_> = s.video.iter().map(|v| v.height).collect();
        assert_eq!(heights, [Some(1080), Some(720), Some(360)]);
        assert_eq!(s.video[0].id, "https://cdn.x/show/1080/index.m3u8");
        assert_eq!(s.video[0].bandwidth, 5_000_000);
        assert_eq!(s.video[0].frame_rate, Some(29.97));
        assert_eq!(s.video[1].id, "https://other.cdn/720.m3u8");
        assert_eq!(s.video[2].codecs.as_deref(), Some("avc1.4d401e,mp4a.40.2"));
        assert_eq!(s.audio.len(), 2);
        assert_eq!(s.audio[1].name.as_deref(), Some("Türkçe"));
        assert!(s.audio[0].default);
        assert_eq!(s.subtitles[0].name.as_deref(), Some("English, CC"));
        assert_eq!(
            s.subtitles[0].id.as_deref(),
            Some("https://cdn.x/show/subs/en.m3u8")
        );
        assert!(!s.live);
        assert_eq!(s.duration, None);
        assert_eq!(s.encryption, None);
    }

    #[test]
    fn media_playlist_vod_live_and_keys() {
        let vod = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-KEY:METHOD=AES-128,URI=\"k.bin\"\n#EXTINF:6.0,\na.ts\n#EXTINF:4.5,\nb.ts\n#EXT-X-ENDLIST\n";
        let s = parse_m3u8("https://x/v.m3u8", vod).expect("vod");
        assert_eq!(s.video.len(), 1);
        assert_eq!(s.video[0].id, "https://x/v.m3u8");
        assert_eq!(s.duration, Some(10.5));
        assert!(!s.live);
        assert_eq!(s.encryption.as_deref(), Some("AES-128"));

        let live = "#EXTM3U\n#EXTINF:2,\na.ts\n";
        assert!(parse_m3u8("https://x/l.m3u8", live).expect("live").live);

        let drm = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n";
        assert_eq!(
            parse_m3u8("https://x/d.m3u8", drm)
                .expect("drm")
                .encryption
                .as_deref(),
            Some("DRM")
        );
        assert!(parse_m3u8("https://x/e.m3u8", "#EXTM3U\n#EXT-X-VERSION:3\n").is_err());
        assert!(parse_m3u8("https://x/e.m3u8", "nope").is_err());
    }

    #[test]
    fn attribute_lists() {
        let a = attributes(r#"BANDWIDTH=1,CODECS="a, b",NAME="x=y",DEFAULT=YES"#);
        assert_eq!(get(&a, "CODECS"), Some("a, b"));
        assert_eq!(get(&a, "NAME"), Some("x=y"));
        assert_eq!(get(&a, "DEFAULT"), Some("YES"));
        assert_eq!(get(&a, "BANDWIDTH"), Some("1"));
    }
}
