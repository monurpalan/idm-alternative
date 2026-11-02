//! Splitting a command into words the way its shell does: POSIX shells, Windows cmd and
//! PowerShell.

/// Words of a POSIX shell command: `'…'`, `"…"` (with `\` escapes), `$'…'`, and `\` at line
/// ends joining lines.
pub(crate) fn words_sh(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('\n') => {}
                Some('\r') if chars.peek() == Some(&'\n') => {
                    chars.next();
                }
                Some(e) => {
                    cur.push(e);
                    started = true;
                }
                None => {}
            },
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => cur.push(ch),
                        None => return Err("a quote is not closed".into()),
                    }
                }
            }
            '$' if chars.peek() == Some(&'\'') => {
                chars.next();
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => cur.push('\n'),
                            Some('t') => cur.push('\t'),
                            Some(ch) => cur.push(ch),
                            None => return Err("a quote is not closed".into()),
                        },
                        Some(ch) => cur.push(ch),
                        None => return Err("a quote is not closed".into()),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(ch @ ('"' | '\\' | '$' | '`')) => cur.push(ch),
                            Some('\n') => {}
                            Some(ch) => {
                                cur.push('\\');
                                cur.push(ch);
                            }
                            None => return Err("a quote is not closed".into()),
                        },
                        Some(ch) => cur.push(ch),
                        None => return Err("a quote is not closed".into()),
                    }
                }
            }
            c if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(cur);
    }
    Ok(out)
}

/// Words of a Windows cmd command: `^` escapes the next character (`^` at a line end joins
/// lines), `"…"` quotes.
pub(crate) fn words_cmd(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '^' => match chars.next() {
                Some('\r') if chars.peek() == Some(&'\n') => {
                    chars.next();
                }
                Some('\n') => {}
                // Reaches curl as `"`, which quotes there.
                Some('"') => {
                    quoted = !quoted;
                    started = true;
                }
                Some(e) => {
                    cur.push(e);
                    started = true;
                }
                None => {}
            },
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return Err("a quote is not closed".into());
    }
    if started {
        out.push(cur);
    }
    Ok(out)
}

/// Words of PowerShell: `"…"` (backtick escapes, `""`), `'…'` (`''`), a backtick at a line end
/// joining lines; `@{`, `}`, `=`, `;`, `(`, `)` and `,` stand alone, except that a word ending
/// in `(` keeps it (`System.Net.Cookie(`).
pub(crate) fn words_ps(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut chars = text.chars().peekable();
    let flush = |out: &mut Vec<String>, cur: &mut String, started: &mut bool| {
        if *started {
            out.push(std::mem::take(cur));
            *started = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '`' => match chars.next() {
                Some('\r') if chars.peek() == Some(&'\n') => {
                    chars.next();
                }
                Some('\n') => {}
                Some(e) => {
                    cur.push(e);
                    started = true;
                }
                None => {}
            },
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') if chars.peek() == Some(&'"') => {
                            chars.next();
                            cur.push('"');
                        }
                        Some('"') => break,
                        Some('`') => match chars.next() {
                            Some('n') => cur.push('\n'),
                            Some('t') => cur.push('\t'),
                            Some(ch) => cur.push(ch),
                            None => return Err("a quote is not closed".into()),
                        },
                        Some(ch) => cur.push(ch),
                        None => return Err("a quote is not closed".into()),
                    }
                }
            }
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') if chars.peek() == Some(&'\'') => {
                            chars.next();
                            cur.push('\'');
                        }
                        Some('\'') => break,
                        Some(ch) => cur.push(ch),
                        None => return Err("a quote is not closed".into()),
                    }
                }
            }
            '@' if chars.peek() == Some(&'{') => {
                chars.next();
                flush(&mut out, &mut cur, &mut started);
                out.push("@{".into());
            }
            '(' if started => {
                cur.push('(');
                flush(&mut out, &mut cur, &mut started);
            }
            '}' | '=' | ';' | '(' | ')' | ',' => {
                flush(&mut out, &mut cur, &mut started);
                if c != ';' && c != ',' {
                    out.push(c.to_string());
                }
            }
            c if c.is_whitespace() => flush(&mut out, &mut cur, &mut started),
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    flush(&mut out, &mut cur, &mut started);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(text: &str) -> Vec<String> {
        words_sh(text).unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

    #[test]
    fn posix_shell() {
        assert_eq!(sh(r"a\ b 'c d' e"), ["a b", "c d", "e"]);
        // `$'…'` with its escapes; `\` at a line end, also after a carriage return.
        assert_eq!(
            sh(&(r"$'a\tb\nc\'d' x\".to_owned() + "\r\ny")),
            ["a\tb\nc'd", "xy"]
        );
        // In double quotes `\` escapes only what the shell would, and keeps itself otherwise.
        assert_eq!(sh(r#""\$x \"y\" \q \\""#), [r#"$x "y" \q \"#]);
        assert_eq!(sh(&(r#""a\"#.to_owned() + "\nb\"")), ["ab"]);
        assert_eq!(sh("'' x"), ["", "x"]);
        for open in ["'a", "\"a", "$'a", "$'a\\", "\"a\\"] {
            assert!(words_sh(open).is_err(), "{open}");
        }
        assert_eq!(sh("a\\"), ["a"]);
    }

    #[test]
    fn windows_cmd() {
        let w = |t: &str| words_cmd(t).expect("words");
        assert_eq!(w("curl ^\r\n  \"a b\" ^\n c"), ["curl", "a b", "c"]);
        assert_eq!(w("^^x ^&y \"\""), ["^x", "&y", ""]);
        // `^"` reaches curl as a quote.
        assert_eq!(w("-H ^\"a b^\""), ["-H", "a b"]);
        assert_eq!(w("a^"), ["a"]);
        assert!(words_cmd("\"a").is_err());
    }

    #[test]
    fn powershell() {
        let w = |t: &str| words_ps(t).expect("words");
        assert_eq!(w("a`\r\n b `\n c"), ["a", "b", "c"]);
        assert_eq!(w(r#""x ""y"" `"z`" `t`n`$""#), ["x \"y\" \"z\" \t\n$"]);
        assert_eq!(w("'it''s' `$a"), ["it's", "$a"]);
        assert_eq!(w("@{a=1;b=2}"), ["@{", "a", "=", "1", "b", "=", "2", "}"]);
        for open in ["\"a", "'a", "\"a`"] {
            assert!(words_ps(open).is_err(), "{open}");
        }
    }
}
