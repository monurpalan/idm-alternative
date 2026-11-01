//! The few texts the Rust side shows itself (tray menu, notifications, notices), in English
//! or Turkish like the page (`src/lib/i18n.svelte.ts`). English is the key.

/// The language the interface uses: the setting, or Windows' display language for "system".
pub fn resolve(setting: &str) -> &'static str {
    match setting {
        "en" => "en",
        "tr" => "tr",
        _ => windows_language(),
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_language() -> &'static str {
    use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
    // SAFETY: no arguments, returns a language id.
    let id = unsafe { GetUserDefaultUILanguage() };
    // The primary language is the low 10 bits; 0x1F is Turkish.
    if id & 0x3FF == 0x1F { "tr" } else { "en" }
}

#[cfg(not(windows))]
fn windows_language() -> &'static str {
    "en"
}

/// `text` in `lang`, with `{name}` replaced from `params`.
pub fn t(lang: &str, text: &str, params: &[(&str, &str)]) -> String {
    let mut s = if lang == "tr" {
        turkish(text).unwrap_or(text)
    } else {
        text
    }
    .to_owned();
    for (k, v) in params {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

fn turkish(text: &str) -> Option<&'static str> {
    Some(match text {
        "Show Shardload" => "Shardload'u göster",
        "Speed limit" => "Hız sınırı",
        "No limit" => "Sınır yok",
        "Start all" => "Tümünü başlat",
        "Pause all" => "Tümünü duraklat",
        "Where did a file come from?…" => "Bu dosya nereden geldi?…",
        "Quit" => "Çıkış",
        "Download" => "İndirme",
        "Download complete" => "İndirme tamamlandı",
        "Virus scan" => "Virüs taraması",
        "Microsoft Defender found a threat in {name} and dealt with it" => {
            "Microsoft Defender {name} içinde bir tehdit buldu ve gereğini yaptı"
        }
        "The virus scanner reported a problem with {name} (exit code {code})" => {
            "Virüs tarayıcısı {name} için bir sorun bildirdi (çıkış kodu {code})"
        }
        "Could not scan {name} for viruses: {error}" => "{name} virüs için taranamadı: {error}",
        "Scheduler: started the queue" => "Zamanlayıcı: kuyruk başlatıldı",
        "Scheduler: paused the queue" => "Zamanlayıcı: kuyruk duraklatıldı",
        "Link refreshed: {name} continues" => "Link yenilendi: {name} devam ediyor",
        "That link leads to a different file, so it opens as a new download" => {
            "Bu link başka bir dosyaya gidiyor, yeni bir indirme olarak açılıyor"
        }
        "When done: would {action} now (dry run)" => "Bitince: şimdi {action} yapılırdı (deneme)",
        "Could not {action}: {error}" => "{action} yapılamadı: {error}",
        "close the app" => "uygulamayı kapat",
        "sleep" => "uyku",
        "hibernate" => "hazırda bekletme",
        "shut down" => "kapatma",
        "{s} s" => "{s} sn",
        "{m} min {s} s" => "{m} dk {s} sn",
        "{m} min" => "{m} dk",
        "{h} h {m} min" => "{h} sa {m} dk",
        "{d} days" => "{d} gün",
        "{a}–{b} s" => "{a}–{b} sn",
        "{a}–{b} min" => "{a}–{b} dk",
        "{a}–{b} h" => "{a}–{b} sa",
        "{n} downloading, {time} left" => "{n} indiriliyor, {time} kaldı",
        "{n} downloading" => "{n} indiriliyor",
        _ => return None,
    })
}

/// A duration as the list shows it: `45 s`, `3 min 20 s`, `14 min`, `2 h 5 min`, `3 days`.
pub fn duration(lang: &str, secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    let n = |v: u64| v.to_string();
    if s < 60 {
        return t(lang, "{s} s", &[("s", &n(s))]);
    }
    let m = s / 60;
    if m < 10 {
        return t(lang, "{m} min {s} s", &[("m", &n(m)), ("s", &n(s % 60))]);
    }
    if m < 60 {
        return t(lang, "{m} min", &[("m", &n(m))]);
    }
    let h = m / 60;
    if h < 48 {
        t(lang, "{h} h {m} min", &[("h", &n(h)), ("m", &n(m % 60))])
    } else {
        t(lang, "{d} days", &[("d", &n(h / 24))])
    }
}

/// Time left from `[soonest, latest]` seconds: one value when the two are close (`14 min`),
/// otherwise a range in the unit of the latest (`14–19 min`). The same rule as the UI's
/// `timeLeft`.
pub fn time_left(lang: &str, soonest: f64, latest: f64) -> String {
    let mid = (soonest + latest) / 2.0;
    if latest - soonest <= (0.15 * mid).max(5.0) {
        return duration(lang, mid);
    }
    let (a, b, key) = if latest < 90.0 {
        (soonest.round(), latest.round(), "{a}–{b} s")
    } else if latest < 90.0 * 60.0 {
        (
            (soonest / 60.0).round().max(1.0),
            (latest / 60.0).round(),
            "{a}–{b} min",
        )
    } else {
        (
            (soonest / 1800.0).round() / 2.0,
            (latest / 1800.0).round() / 2.0,
            "{a}–{b} h",
        )
    };
    if a >= b {
        return duration(lang, mid);
    }
    let num = |v: f64| {
        let s = format!("{v}");
        if lang == "tr" { s.replace('.', ",") } else { s }
    };
    t(lang, key, &[("a", &num(a)), ("b", &num(b))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_left_as_one_value_or_a_range() {
        assert_eq!(time_left("en", 840.0, 860.0), "14 min");
        assert_eq!(time_left("en", 840.0, 1140.0), "14–19 min");
        assert_eq!(time_left("tr", 840.0, 1140.0), "14–19 dk");
        assert_eq!(time_left("en", 20.0, 40.0), "20–40 s");
        assert_eq!(
            time_left("en", 3.0, 6.0),
            "5 s",
            "a few seconds apart is one value"
        );
        assert_eq!(time_left("tr", 5400.0, 9000.0), "1,5–2,5 sa");
        assert_eq!(
            time_left("en", 100.0, 130.0),
            "1 min 55 s",
            "rounds to the same minute"
        );
        assert_eq!(time_left("en", 0.0, 0.0), "0 s");
    }

    #[test]
    fn turkish_with_params() {
        assert_eq!(
            t(
                "tr",
                "Link refreshed: {name} continues",
                &[("name", "a.zip")]
            ),
            "Link yenilendi: a.zip devam ediyor"
        );
        assert_eq!(
            t(
                "en",
                "Link refreshed: {name} continues",
                &[("name", "a.zip")]
            ),
            "Link refreshed: a.zip continues"
        );
        assert_eq!(t("tr", "not in the table", &[]), "not in the table");
        assert_eq!(resolve("tr"), "tr");
        assert_eq!(resolve("en"), "en");
    }
}
