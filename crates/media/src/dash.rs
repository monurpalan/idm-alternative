//! DASH manifests (MPD, ISO/IEC 23009-1): periods hold adaptation sets of one content type,
//! each with representations at different qualities.

use roxmltree::{Document, Node};

use crate::{Error, Format, Streams, Track, Variant, sort_best_first};

pub fn parse_mpd(_url: &str, text: &str) -> Result<Streams, Error> {
    let doc = Document::parse(text).map_err(|e| Error(format!("not a DASH manifest: {e}")))?;
    let mpd = doc.root_element();
    if mpd.tag_name().name() != "MPD" {
        return Err(Error("not a DASH manifest (no <MPD>)".into()));
    }
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut subtitles = Vec::new();
    let mut drm = false;

    // The first period is the programme; later ones are usually ads or bumpers.
    let Some(period) = children(mpd, "Period").next() else {
        return Err(Error("the manifest has no period".into()));
    };
    for set in children(period, "AdaptationSet") {
        drm |= children(set, "ContentProtection").next().is_some();
        let set_mime = set.attribute("mimeType").unwrap_or("");
        let content = set.attribute("contentType").unwrap_or("");
        let lang = set.attribute("lang").map(str::to_owned);
        let label = children(set, "Label")
            .next()
            .and_then(|l| l.text())
            .map(str::to_owned);
        for rep in children(set, "Representation") {
            drm |= children(rep, "ContentProtection").next().is_some();
            let mime = rep.attribute("mimeType").unwrap_or(set_mime);
            let kind = if content.is_empty() {
                mime.split('/').next().unwrap_or("")
            } else {
                content
            };
            let id = rep.attribute("id").unwrap_or_default().to_owned();
            let bandwidth = rep.attribute("bandwidth").and_then(|b| b.parse().ok());
            let codecs = rep
                .attribute("codecs")
                .or_else(|| set.attribute("codecs"))
                .map(str::to_owned);
            match kind {
                "video" => video.push(Variant {
                    id,
                    bandwidth: bandwidth.unwrap_or(0),
                    width: number(rep, set, "width"),
                    height: number(rep, set, "height"),
                    codecs,
                    frame_rate: rep
                        .attribute("frameRate")
                        .or_else(|| set.attribute("frameRate"))
                        .and_then(frame_rate),
                    audio_group: None,
                }),
                "audio" => audio.push(Track {
                    id: Some(id),
                    name: label.clone(),
                    language: lang.clone(),
                    group: None,
                    default: false,
                    bandwidth,
                    codecs,
                }),
                "text" | "application" => subtitles.push(Track {
                    id: Some(id),
                    name: label.clone(),
                    language: lang.clone(),
                    group: None,
                    default: false,
                    bandwidth,
                    codecs,
                }),
                _ => {}
            }
        }
    }
    if video.is_empty() && audio.is_empty() {
        return Err(Error("the manifest has no video or audio".into()));
    }
    sort_best_first(&mut video);
    // Best audio first as well.
    audio.sort_by_key(|a| std::cmp::Reverse(a.bandwidth));
    let live = mpd.attribute("type") == Some("dynamic");
    Ok(Streams {
        format: Format::Dash,
        video,
        audio,
        subtitles,
        duration: mpd
            .attribute("mediaPresentationDuration")
            .and_then(iso_duration),
        live,
        encryption: drm.then(|| "DRM".to_owned()),
    })
}

fn children<'a, 'input>(
    node: Node<'a, 'input>,
    name: &'static str,
) -> impl Iterator<Item = Node<'a, 'input>> {
    node.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == name)
}

/// An attribute of the representation, inherited from its adaptation set.
fn number(rep: Node, set: Node, name: &str) -> Option<u32> {
    rep.attribute(name)
        .or_else(|| set.attribute(name))
        .and_then(|v| v.parse().ok())
}

/// `30`, `30000/1001`.
fn frame_rate(v: &str) -> Option<f64> {
    match v.split_once('/') {
        Some((n, d)) => {
            let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
            (d != 0.0).then(|| (n / d * 1000.0).round() / 1000.0)
        }
        None => v.parse().ok(),
    }
}

/// ISO 8601 durations as DASH uses them: `PT1H2M3.5S`, `P1DT2H`.
pub(crate) fn iso_duration(v: &str) -> Option<f64> {
    let rest = v.strip_prefix('P')?;
    let (date, time) = rest.split_once('T').unwrap_or((rest, ""));
    let mut total = 0.0;
    let mut take = |part: &str, units: &[(char, f64)]| -> Option<()> {
        let mut num = String::new();
        for c in part.chars() {
            if c.is_ascii_digit() || c == '.' {
                num.push(c);
            } else {
                let scale = units.iter().find(|(u, _)| *u == c)?.1;
                total += num.parse::<f64>().ok()? * scale;
                num.clear();
            }
        }
        num.is_empty().then_some(())
    };
    take(date, &[('D', 86_400.0)])?;
    take(time, &[('H', 3600.0), ('M', 60.0), ('S', 1.0)])?;
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MPD: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT1H2M3.5S">
  <Period id="1">
    <AdaptationSet contentType="video" mimeType="video/mp4" frameRate="30000/1001">
      <Representation id="v360" bandwidth="700000" width="640" height="360" codecs="avc1.4d401e"/>
      <Representation id="v1080" bandwidth="4800000" width="1920" height="1080" codecs="avc1.640028"/>
    </AdaptationSet>
    <AdaptationSet mimeType="audio/mp4" lang="en">
      <Label>English</Label>
      <Representation id="a64" bandwidth="64000" codecs="mp4a.40.5"/>
      <Representation id="a128" bandwidth="128000" codecs="mp4a.40.2"/>
    </AdaptationSet>
    <AdaptationSet contentType="text" mimeType="text/vtt" lang="tr">
      <Representation id="s-tr" bandwidth="1000"/>
    </AdaptationSet>
  </Period>
  <Period id="ad"><AdaptationSet contentType="video"><Representation id="ad" bandwidth="1"/></AdaptationSet></Period>
</MPD>"#;

    #[test]
    fn manifest() {
        let s = parse_mpd("https://cdn/x.mpd", MPD).expect("parse");
        assert_eq!(s.format, Format::Dash);
        assert_eq!(
            s.video.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            ["v1080", "v360"]
        );
        assert_eq!(s.video[0].frame_rate, Some(29.97));
        assert_eq!(s.video[0].height, Some(1080));
        assert_eq!(
            s.audio.iter().map(|a| a.id.as_deref()).collect::<Vec<_>>(),
            [Some("a128"), Some("a64")]
        );
        assert_eq!(s.audio[0].language.as_deref(), Some("en"));
        assert_eq!(s.audio[0].name.as_deref(), Some("English"));
        assert_eq!(s.subtitles[0].language.as_deref(), Some("tr"));
        assert_eq!(s.duration, Some(3723.5));
        assert!(!s.live);
        assert_eq!(s.encryption, None);
    }

    #[test]
    fn drm_live_and_errors() {
        let drm = r#"<MPD type="dynamic"><Period><AdaptationSet mimeType="video/mp4"><ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/><Representation id="v" bandwidth="1" width="1" height="1"/></AdaptationSet></Period></MPD>"#;
        let s = parse_mpd("u", drm).expect("drm");
        assert!(s.live);
        assert_eq!(s.encryption.as_deref(), Some("DRM"));
        assert!(parse_mpd("u", "<MPD></MPD>").is_err());
        assert!(parse_mpd("u", "<html/>").is_err());
        assert!(parse_mpd("u", "<MPD><Period/></MPD>").is_err());
    }

    #[test]
    fn durations_and_rates() {
        assert_eq!(iso_duration("PT10S"), Some(10.0));
        assert_eq!(iso_duration("P1DT1M"), Some(86_460.0));
        assert_eq!(iso_duration("PT0.5S"), Some(0.5));
        assert_eq!(iso_duration("10S"), None);
        assert_eq!(iso_duration("PT5X"), None);
        assert_eq!(frame_rate("25"), Some(25.0));
        assert_eq!(frame_rate("24000/1001"), Some(23.976));
        assert_eq!(frame_rate("1/0"), None);
    }
}
