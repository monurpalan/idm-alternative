//! The `Accept-Language` a browser on this system would send.

use wreq::header::HeaderValue;

/// `Accept-Language` for the system's language, worked out once.
pub(super) fn system_accept_language() -> Option<HeaderValue> {
    static LANG: std::sync::OnceLock<Option<HeaderValue>> = std::sync::OnceLock::new();
    LANG.get_or_init(|| {
        let locale = sys_locale::get_locale()?;
        HeaderValue::try_from(accept_language(&locale)).ok()
    })
    .clone()
}

/// What Chrome sends for `locale` (`tr-TR`): the locale, its language, then English.
fn accept_language(locale: &str) -> String {
    let lang = locale.split(['-', '_']).next().unwrap_or(locale);
    if lang.eq_ignore_ascii_case("en") {
        format!("{locale},en;q=0.9")
    } else if lang == locale {
        format!("{lang},en-US;q=0.9,en;q=0.8")
    } else {
        format!("{locale},{lang};q=0.9,en-US;q=0.8,en;q=0.7")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_language_like_chrome() {
        assert_eq!(
            accept_language("tr-TR"),
            "tr-TR,tr;q=0.9,en-US;q=0.8,en;q=0.7"
        );
        assert_eq!(accept_language("en-US"), "en-US,en;q=0.9");
        assert_eq!(accept_language("de"), "de,en-US;q=0.9,en;q=0.8");
    }
}
