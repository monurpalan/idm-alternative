//! Messages from the extension to the app, and the app's replies. The host passes them on
//! without looking inside, except to answer `ping` when the app cannot be reached.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// First message on every pipe connection.
    Hello { token: String },
    /// Is anyone there? Answered with the app's version.
    Ping,
    /// The browser started a download and handed it over.
    Download(Capture),
    /// "Download all links": the links of a page, for the user to pick from.
    Links {
        referrer: Option<String>,
        links: Vec<Link>,
    },
}

/// Everything needed to download what the browser was downloading, as the browser would.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capture {
    pub url: String,
    /// The page the download started from.
    pub referrer: Option<String>,
    /// The name the browser chose, if any.
    pub filename: Option<String>,
    /// Size the browser was told, if any.
    pub size: Option<u64>,
    /// `Cookie` header value for the URL, from the browser's cookie store.
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
    /// Any other request headers worth repeating (e.g. `Authorization`).
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub url: String,
    /// The link's text on the page.
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The app's version, in answer to `ping`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl Reply {
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
            version: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
            version: None,
        }
    }
}

impl Capture {
    /// The request headers for a download of this capture: cookies, user agent, referrer
    /// and the rest, without duplicates.
    pub fn request_headers(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let mut push = |k: &str, v: &str| {
            if !v.is_empty() && !out.iter().any(|(o, _)| o.eq_ignore_ascii_case(k)) {
                out.push((k.to_owned(), v.to_owned()));
            }
        };
        if let Some(c) = &self.cookies {
            push("Cookie", c);
        }
        if let Some(r) = &self.referrer {
            push("Referer", r);
        }
        if let Some(ua) = &self.user_agent {
            push("User-Agent", ua);
        }
        for (k, v) in &self.headers {
            push(k, v);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape() {
        let r: Request = serde_json::from_str(
            r#"{"type":"download","url":"https://x/a.zip","cookies":"s=1","headers":[["Authorization","Bearer t"]]}"#,
        )
        .expect("parse");
        let Request::Download(c) = r else {
            panic!("not a download")
        };
        assert_eq!(c.url, "https://x/a.zip");
        assert_eq!(
            c.request_headers(),
            vec![
                ("Cookie".to_owned(), "s=1".to_owned()),
                ("Authorization".to_owned(), "Bearer t".to_owned())
            ]
        );
        assert_eq!(
            serde_json::to_string(&Request::Ping).expect("json"),
            r#"{"type":"ping"}"#
        );
        assert_eq!(
            serde_json::to_string(&Reply::ok()).expect("json"),
            r#"{"ok":true}"#
        );
    }

    #[test]
    fn explicit_headers_do_not_duplicate() {
        let c = Capture {
            url: "u".into(),
            referrer: Some("https://page".into()),
            user_agent: Some("UA".into()),
            headers: vec![("referer".into(), "other".into())],
            ..Capture::default()
        };
        let h = c.request_headers();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0], ("Referer".to_owned(), "https://page".to_owned()));
    }
}
