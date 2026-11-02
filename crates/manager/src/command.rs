//! A download given as a command, as browsers copy it ("Copy as cURL", "Copy as PowerShell")
//! or as people write it: `curl`, `wget` or PowerShell's `Invoke-WebRequest`. What is kept:
//! the address, the request headers (cookies, referrer, user agent, a login) and an output
//! name; a request that sends data (POST) cannot be repeated as a download and is reported.

mod words;

use words::{words_cmd, words_ps, words_sh};

use serde::Serialize;

/// What a pasted command asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Pasted {
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// `-o name`, `-O name`, `-OutFile name`.
    pub name: Option<String>,
    /// The command sends data or uses another method than GET: downloaded with GET, which
    /// may not give the same answer.
    pub post: bool,
    /// Headers the engine sets itself (`Range`, `Accept-Encoding`…), left out.
    pub dropped: Vec<String>,
}

/// Headers the engine sets itself, or that belong to one connection: a download never takes
/// them from the user.
const ENGINE_HEADERS: &[&str] = &[
    "range",
    "if-range",
    "accept-encoding",
    "content-length",
    "content-type",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "te",
    "host",
    "expect",
    "proxy-connection",
];

/// Whether `headers` can be sent with a download: names that are HTTP tokens, values on one
/// line, and none of the headers the engine sets itself.
pub fn check_headers(headers: &[(String, String)]) -> Result<(), String> {
    for (name, value) in headers {
        let token = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
        if name.is_empty() || !name.chars().all(token) {
            return Err(format!("`{name}` is not a header name"));
        }
        if value.contains(['\r', '\n', '\0']) {
            return Err(format!("the value of {name} must be on one line"));
        }
        if ENGINE_HEADERS.contains(&name.to_ascii_lowercase().as_str()) || name.starts_with(':') {
            return Err(format!("{name} is set by the download itself"));
        }
    }
    Ok(())
}

/// `Name: value` lines (blank ones and `#` comments skipped) as headers, checked.
pub fn parse_header_lines(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| format!("`{line}` is not `Name: value`"))?;
        out.push((name.trim().to_owned(), value.trim().to_owned()));
    }
    check_headers(&out)?;
    Ok(out)
}

/// Reads a `curl`, `wget` or `Invoke-WebRequest` command (bash, Windows cmd or PowerShell
/// quoting, over several lines).
pub fn parse_command(text: &str) -> Result<Pasted, String> {
    let text = text.trim();
    let first = text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_start_matches('&')
        .to_ascii_lowercase();
    let first = first.trim_end_matches(".exe");
    let mut pasted = if first.starts_with('$') || is_powershell(first) {
        powershell(text)?
    } else if first == "curl" || first == "wget" {
        let args = if cmd_quoted(text) {
            words_cmd(text)?
        } else {
            words_sh(text)?
        };
        if first == "curl" {
            curl(&args[1..])?
        } else {
            wget(&args[1..])?
        }
    } else {
        return Err("not a curl, wget or Invoke-WebRequest command".into());
    };
    if pasted.url.is_empty() {
        return Err("the command has no address".into());
    }
    // Keep what a download can send; say what was left out.
    let mut kept = Vec::new();
    for (name, value) in std::mem::take(&mut pasted.headers) {
        if ENGINE_HEADERS.contains(&name.to_ascii_lowercase().as_str()) || name.starts_with(':') {
            pasted.dropped.push(name);
        } else if let Some(same) = kept
            .iter_mut()
            .find(|(n, _): &&mut (String, String)| n.eq_ignore_ascii_case(&name))
        {
            // Cookies given twice (`-b` and `-H 'Cookie: …'`) go together.
            if name.eq_ignore_ascii_case("cookie") {
                same.1 = format!("{}; {value}", same.1);
            } else {
                same.1 = value;
            }
        } else {
            kept.push((name, value));
        }
    }
    check_headers(&kept)?;
    pasted.headers = kept;
    Ok(pasted)
}

fn is_powershell(first: &str) -> bool {
    matches!(
        first,
        "invoke-webrequest" | "iwr" | "invoke-restmethod" | "irm"
    )
}

/// Chrome's "Copy as cURL (cmd)": `^` escapes and `^` at line ends.
fn cmd_quoted(text: &str) -> bool {
    text.contains("^\"") || text.lines().any(|l| l.trim_end().ends_with('^'))
}

fn header(line: &str) -> Option<(String, String)> {
    let (name, value) = line.split_once(':')?;
    let name = name.trim();
    (!name.is_empty()).then(|| (name.to_owned(), value.trim().to_owned()))
}

fn basic(login: &str) -> (String, String) {
    let (user, password) = login.split_once(':').unwrap_or((login, ""));
    shardload_engine::basic_auth(user, password)
}

fn is_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    ["http://", "https://", "ftp://"]
        .iter()
        .any(|p| lower.starts_with(p))
}

/// curl options that take a value this does not use, so the value is not taken for the URL.
const CURL_VALUE_OPTIONS: &[&str] = &[
    "--connect-timeout",
    "-m",
    "--max-time",
    "--retry",
    "--retry-delay",
    "--retry-max-time",
    "-x",
    "--proxy",
    "-U",
    "--proxy-user",
    "--limit-rate",
    "-r",
    "--range",
    "-w",
    "--write-out",
    "--resolve",
    "--cacert",
    "--capath",
    "-E",
    "--cert",
    "--key",
    "-K",
    "--config",
    "-c",
    "--cookie-jar",
    "-D",
    "--dump-header",
    "--interface",
    "-C",
    "--continue-at",
    "-y",
    "--speed-time",
    "-Y",
    "--speed-limit",
    "--max-filesize",
    "--max-redirs",
    "-T",
    "--upload-file",
];

fn curl(args: &[String]) -> Result<Pasted, String> {
    let mut p = Pasted::default();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        // `--opt=value` and `-Xvalue` as well as `--opt value`.
        let (opt, attached) = match arg.split_once('=') {
            Some((o, v)) if o.starts_with("--") => (o, Some(v.to_owned())),
            _ if arg.len() > 2
                && arg.starts_with('-')
                && !arg.starts_with("--")
                && arg
                    .as_bytes()
                    .get(1)
                    .is_some_and(|b| b"HbAeuXdoF".contains(b)) =>
            {
                (&arg[..2], Some(arg[2..].to_owned()))
            }
            _ => (arg, None),
        };
        let mut value = || -> Result<String, String> {
            if let Some(v) = attached.clone() {
                return Ok(v);
            }
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{opt} needs a value"))
        };
        match opt {
            "--url" => p.url = value()?,
            "-H" | "--header" => {
                if let Some(h) = header(&value()?) {
                    p.headers.push(h);
                }
            }
            "-b" | "--cookie" => {
                let v = value()?;
                // Without `=` it is a cookie file, which this does not read.
                if v.contains('=') {
                    p.headers.push(("Cookie".into(), v));
                }
            }
            "-A" | "--user-agent" => p.headers.push(("User-Agent".into(), value()?)),
            "-e" | "--referer" => {
                let v = value()?;
                let v = v.trim_end_matches(";auto").to_owned();
                p.headers.push(("Referer".into(), v));
            }
            "-u" | "--user" => p.headers.push(basic(&value()?)),
            "-X" | "--request" => {
                let m = value()?;
                if !m.eq_ignore_ascii_case("GET") && !m.eq_ignore_ascii_case("HEAD") {
                    p.post = true;
                }
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-urlencode"
            | "--data-ascii" | "-F" | "--form" | "--json" => {
                value()?;
                p.post = true;
            }
            "-o" | "--output" => p.name = Some(value()?),
            o if CURL_VALUE_OPTIONS.contains(&o) => {
                value()?;
            }
            o if o.starts_with('-') => {}
            url if p.url.is_empty() && is_url(url) => p.url = url.to_owned(),
            _ => {}
        }
        i += 1;
    }
    Ok(p)
}

fn wget(args: &[String]) -> Result<Pasted, String> {
    let mut p = Pasted::default();
    let (mut user, mut password) = (None, None);
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let (opt, attached) = match arg.split_once('=') {
            Some((o, v)) if o.starts_with("--") => (o, Some(v.to_owned())),
            _ => (arg, None),
        };
        let mut value = || -> Result<String, String> {
            if let Some(v) = attached.clone() {
                return Ok(v);
            }
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{opt} needs a value"))
        };
        match opt {
            "--header" => {
                if let Some(h) = header(&value()?) {
                    p.headers.push(h);
                }
            }
            "-U" | "--user-agent" => p.headers.push(("User-Agent".into(), value()?)),
            "--referer" => p.headers.push(("Referer".into(), value()?)),
            "--user" | "--http-user" => user = Some(value()?),
            "--password" | "--http-password" => password = Some(value()?),
            "--method" => {
                let m = value()?;
                p.post |= !m.eq_ignore_ascii_case("GET") && !m.eq_ignore_ascii_case("HEAD");
            }
            "--post-data" | "--post-file" | "--body-data" | "--body-file" => {
                value()?;
                p.post = true;
            }
            "-O" | "--output-document" => p.name = Some(value()?),
            "-o" | "--output-file" | "-a" | "--append-output" | "-P" | "--directory-prefix"
            | "-t" | "--tries" | "-T" | "--timeout" | "--load-cookies" | "--save-cookies"
            | "-e" | "--execute" | "-w" | "--wait" | "--limit-rate" => {
                value()?;
            }
            o if o.starts_with('-') => {}
            url if p.url.is_empty() && is_url(url) => p.url = url.to_owned(),
            _ => {}
        }
        i += 1;
    }
    if let Some(user) = user {
        p.headers
            .push(basic(&format!("{user}:{}", password.unwrap_or_default())));
    }
    Ok(p)
}

/// `Invoke-WebRequest` with `-Uri`, `-Headers @{…}`, `-UserAgent`, `-OutFile`, `-Method`,
/// `-Body`, and the `$session` lines Chrome writes before it (user agent, cookies).
fn powershell(text: &str) -> Result<Pasted, String> {
    let words = words_ps(text)?;
    let mut p = Pasted::default();
    let mut cookies: Vec<String> = Vec::new();
    let mut i = 0;
    let lower = |s: &str| s.to_ascii_lowercase();
    while i < words.len() {
        let w = words[i].as_str();
        match lower(w).as_str() {
            "-uri" => {
                i += 1;
                p.url = words.get(i).cloned().unwrap_or_default();
            }
            "-useragent" => {
                i += 1;
                if let Some(v) = words.get(i) {
                    p.headers.push(("User-Agent".into(), v.clone()));
                }
            }
            "-outfile" => {
                i += 1;
                p.name = words.get(i).cloned();
            }
            "-method" => {
                i += 1;
                let m = words.get(i).map(|m| lower(m)).unwrap_or_default();
                p.post |= m != "get" && m != "head";
            }
            "-body" | "-infile" | "-form" => {
                i += 1;
                p.post = true;
            }
            "-headers" => {
                // `@{` then `"name"="value"` pairs (or `name = value`), up to `}`.
                i += 1;
                if words.get(i).map(String::as_str) == Some("@{") {
                    i += 1;
                    while i < words.len() && words[i] != "}" {
                        if words.get(i + 1).map(String::as_str) == Some("=") {
                            if let Some(v) = words.get(i + 2) {
                                p.headers.push((words[i].clone(), v.clone()));
                            }
                            i += 3;
                        } else {
                            i += 1;
                        }
                    }
                }
            }
            "$session.useragent" if words.get(i + 1).map(String::as_str) == Some("=") => {
                if let Some(v) = words.get(i + 2) {
                    p.headers.push(("User-Agent".into(), v.clone()));
                }
                i += 2;
            }
            "system.net.cookie(" => {
                // `New-Object System.Net.Cookie("name", "value", "/", "host")`.
                if let (Some(n), Some(v)) = (words.get(i + 1), words.get(i + 2)) {
                    cookies.push(format!("{n}={v}"));
                }
                i += 2;
            }
            _ if p.url.is_empty() && is_url(w) && i > 0 && is_powershell(&lower(&words[i - 1])) => {
                p.url = w.to_owned();
            }
            _ => {}
        }
        i += 1;
    }
    if !cookies.is_empty() {
        p.headers.push(("Cookie".into(), cookies.join("; ")));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h<'a>(p: &'a Pasted, name: &str) -> Option<&'a str> {
        p.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn curl_as_chrome_copies_it_for_bash() {
        let p = parse_command(
            "curl 'https://cdn.example.com/files/a%20b.zip?sig=x&e=1' \\\n  \
             -H 'accept: */*' \\\n  -H 'accept-language: tr-TR,tr;q=0.9' \\\n  \
             -b 'sid=abc; theme=dark' \\\n  -H 'referer: https://example.com/page' \\\n  \
             -H 'user-agent: Mozilla/5.0 (Windows NT 10.0)' \\\n  --compressed",
        )
        .expect("parse");
        assert_eq!(p.url, "https://cdn.example.com/files/a%20b.zip?sig=x&e=1");
        assert_eq!(h(&p, "cookie"), Some("sid=abc; theme=dark"));
        assert_eq!(h(&p, "referer"), Some("https://example.com/page"));
        assert_eq!(h(&p, "user-agent"), Some("Mozilla/5.0 (Windows NT 10.0)"));
        assert_eq!(h(&p, "accept-language"), Some("tr-TR,tr;q=0.9"));
        assert!(!p.post);
    }

    #[test]
    fn curl_as_chrome_copies_it_for_cmd() {
        let p = parse_command(
            "curl ^\"https://cdn.example.com/a.zip?x=1^&y=2^\" ^\r\n  \
             -H ^\"accept: text/html^\" ^\r\n  -b ^\"sid=abc; n=^%^22q^%^22^\" ^\r\n  \
             -H ^\"user-agent: Mozilla/5.0^\"",
        )
        .expect("parse");
        assert_eq!(p.url, "https://cdn.example.com/a.zip?x=1&y=2");
        assert_eq!(h(&p, "cookie"), Some("sid=abc; n=%22q%22"));
        assert_eq!(h(&p, "user-agent"), Some("Mozilla/5.0"));
    }

    #[test]
    fn curl_options_people_write() {
        let p = parse_command(
            r#"curl -L -sS -o out.iso -A "Agent 1" -e https://ref.example/ -u bob:s3cret --cookie "a=1" -H "Cookie: b=2" -H"X-Token: t" --connect-timeout 10 https://example.com/x.iso"#,
        )
        .expect("parse");
        assert_eq!(p.url, "https://example.com/x.iso");
        assert_eq!(p.name.as_deref(), Some("out.iso"));
        assert_eq!(h(&p, "user-agent"), Some("Agent 1"));
        assert_eq!(h(&p, "referer"), Some("https://ref.example/"));
        assert_eq!(h(&p, "authorization"), Some("Basic Ym9iOnMzY3JldA=="));
        assert_eq!(
            h(&p, "cookie"),
            Some("a=1; b=2"),
            "cookies given twice go together"
        );
        assert_eq!(h(&p, "x-token"), Some("t"));
        // A cookie file is not read.
        let p = parse_command("curl -b cookies.txt https://e.x/f").expect("parse");
        assert_eq!(h(&p, "cookie"), None);
    }

    #[test]
    fn posts_are_reported() {
        for cmd in [
            "curl -X POST https://e.x/f",
            "curl https://e.x/f -d 'a=1'",
            "curl --data-raw '{\"q\":1}' https://e.x/f",
            "curl -F file=@x https://e.x/f",
            "wget --post-data 'a=1' https://e.x/f",
            "Invoke-WebRequest -Uri https://e.x/f -Method Post -Body 'a'",
        ] {
            assert!(parse_command(cmd).expect(cmd).post, "{cmd}");
        }
        assert!(
            !parse_command("curl -X GET https://e.x/f")
                .expect("get")
                .post
        );
    }

    #[test]
    fn headers_the_engine_sets_are_left_out() {
        let p = parse_command(
            "curl https://e.x/f -H 'Range: bytes=0-' -H 'Accept-Encoding: gzip' -H ':authority: e.x' -H 'X-A: 1'",
        )
        .expect("parse");
        assert_eq!(p.headers, vec![("X-A".to_owned(), "1".to_owned())]);
        // HTTP/2 pseudo-headers (`:authority`) are not headers at all.
        assert_eq!(p.dropped, vec!["Range", "Accept-Encoding"]);
    }

    #[test]
    fn wget() {
        let p = parse_command(
            "wget --header='Cookie: s=1' --header \"Authorization: Bearer abc\" -U 'Agent' \
             --referer=https://r.example/ -O out.bin --http-user=u --http-password=p \
             https://e.x/file.bin",
        )
        .expect("parse");
        assert_eq!(p.url, "https://e.x/file.bin");
        assert_eq!(h(&p, "cookie"), Some("s=1"));
        assert_eq!(h(&p, "user-agent"), Some("Agent"));
        assert_eq!(h(&p, "referer"), Some("https://r.example/"));
        assert_eq!(p.name.as_deref(), Some("out.bin"));
        assert!(
            p.headers
                .iter()
                .any(|(n, v)| n == "Authorization" && v == "Basic dTpw")
        );
    }

    #[test]
    fn powershell_as_chrome_copies_it() {
        let p = parse_command(
            "$session = New-Object Microsoft.PowerShell.Commands.WebRequestSession\r\n\
             $session.UserAgent = \"Mozilla/5.0 (Windows NT 10.0; Win64; x64)\"\r\n\
             $session.Cookies.Add((New-Object System.Net.Cookie(\"sid\", \"abc\", \"/\", \"e.x\")))\r\n\
             $session.Cookies.Add((New-Object System.Net.Cookie(\"t\", \"1\", \"/\", \"e.x\")))\r\n\
             Invoke-WebRequest -UseBasicParsing -Uri \"https://e.x/a.zip?k=1\" `\r\n\
             -WebSession $session `\r\n\
             -Headers @{\r\n\
             \"authority\"=\"e.x\"\r\n\
               \"accept\"=\"*/*\"\r\n\
               \"referer\"=\"https://e.x/page\"\r\n\
             }",
        )
        .expect("parse");
        assert_eq!(p.url, "https://e.x/a.zip?k=1");
        assert_eq!(
            h(&p, "user-agent"),
            Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
        );
        assert_eq!(h(&p, "cookie"), Some("sid=abc; t=1"));
        assert_eq!(h(&p, "referer"), Some("https://e.x/page"));
        assert_eq!(h(&p, "accept"), Some("*/*"));
        assert!(!p.post);
    }

    #[test]
    fn powershell_as_people_write_it() {
        let p = parse_command(
            "iwr 'https://e.x/f.msi' -OutFile 'it''s.msi' -UserAgent \"A `\"B`\"\" -Headers @{ Authorization = 'Bearer x'; 'X-Y' = \"z\" }",
        )
        .expect("parse");
        assert_eq!(p.url, "https://e.x/f.msi");
        assert_eq!(p.name.as_deref(), Some("it's.msi"));
        assert_eq!(h(&p, "user-agent"), Some("A \"B\""));
        assert_eq!(h(&p, "authorization"), Some("Bearer x"));
        assert_eq!(h(&p, "x-y"), Some("z"));
    }

    #[test]
    fn what_is_not_a_command() {
        assert!(parse_command("https://e.x/f").is_err());
        assert!(parse_command("curl -H 'a: b'").is_err(), "no address");
        assert!(
            parse_command("curl 'https://e.x/f").is_err(),
            "unclosed quote"
        );
        assert!(parse_command("").is_err());
    }

    #[test]
    fn header_lines() {
        let hs = parse_header_lines("Referer: https://r/\n\n# a note\nCookie: a=1; b=2\nX-Empty:")
            .expect("parse");
        assert_eq!(
            hs,
            vec![
                ("Referer".to_owned(), "https://r/".to_owned()),
                ("Cookie".to_owned(), "a=1; b=2".to_owned()),
                ("X-Empty".to_owned(), String::new()),
            ]
        );
        assert!(parse_header_lines("no colon").is_err());
        assert!(parse_header_lines("Bad Name: x").is_err());
        assert!(parse_header_lines("Range: bytes=1-").is_err());
        assert!(check_headers(&[("X".into(), "a\r\nInjected: 1".into())]).is_err());
        assert!(check_headers(&[("Authorization".into(), "Bearer x".into())]).is_ok());
    }
}
