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
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
