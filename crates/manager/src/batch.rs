//! Batch downloads, like IDM's: one address with ranges in square brackets stands for many
//! (`photo[001-100].jpg`, `disk[a-e].iso`, `part[0-100:10].bin`).

/// At most this many addresses from one pattern.
pub const MAX: usize = 10_000;

/// One `[...]` range.
#[derive(Debug, Clone, PartialEq)]
enum Range {
    /// From, to, step, and the width to pad with zeros to (`[001-100]`), 0 for none.
    Number(u64, u64, u64, usize),
    Letter(char, char),
}

impl Range {
    fn parse(inner: &str) -> Option<Self> {
        let (span, step) = match inner.split_once(':') {
            Some((s, st)) => (s, st.parse::<u64>().ok().filter(|&n| n > 0)?),
            None => (inner, 1),
        };
        let (a, b) = span.split_once('-')?;
        let digits =
            |s: &str| !s.is_empty() && s.len() <= 18 && s.bytes().all(|c| c.is_ascii_digit());
        if digits(a) && digits(b) {
            let width = if a.len() > 1 && a.starts_with('0') {
                a.len()
            } else {
                0
            };
            return Some(Range::Number(a.parse().ok()?, b.parse().ok()?, step, width));
        }
        let letter = |s: &str| {
            let mut c = s.chars();
            match (c.next(), c.next()) {
                (Some(ch), None) if ch.is_ascii_alphabetic() => Some(ch),
                _ => None,
            }
        };
        let (a, b) = (letter(a)?, letter(b)?);
        (step == 1 && a.is_ascii_lowercase() == b.is_ascii_lowercase())
            .then_some(Range::Letter(a, b))
    }

    fn len(&self) -> u64 {
        match *self {
            Range::Number(a, b, step, _) => a.abs_diff(b) / step + 1,
            Range::Letter(a, b) => u64::from((a as u8).abs_diff(b as u8)) + 1,
        }
    }

    fn values(&self) -> Vec<String> {
        match *self {
            Range::Number(a, b, step, width) => {
                let n = self.len();
                (0..n)
                    .map(|i| if a <= b { a + i * step } else { a - i * step })
                    .map(|v| format!("{v:0width$}"))
                    .collect()
            }
            Range::Letter(a, b) => {
                let (a, b) = (a as u8, b as u8);
                let it: Box<dyn Iterator<Item = u8>> = if a <= b {
                    Box::new(a..=b)
                } else {
                    Box::new((b..=a).rev())
                };
                it.map(|c| (c as char).to_string()).collect()
            }
        }
    }
}

/// The parts of `text`: plain text between the ranges, and the ranges.
fn split(text: &str) -> (Vec<&str>, Vec<Range>) {
    let mut plain = Vec::new();
    let mut ranges = Vec::new();
    let mut rest = text;
    let mut start = 0;
    let mut from = 0;
    while let Some(open) = rest[from..].find('[') {
        let open = from + open;
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        let close = open + close;
        match Range::parse(&rest[open + 1..close]) {
            Some(r) => {
                plain.push(&rest[start..open]);
                ranges.push(r);
                rest = &rest[close + 1..];
                start = 0;
                from = 0;
            }
            // Not a range (an IPv6 host, a bracket in a query): keep it as text.
            None => from = open + 1,
        }
    }
    plain.push(&rest[start..]);
    (plain, ranges)
}

/// Whether `text` has at least one range.
pub fn is_pattern(text: &str) -> bool {
    !split(text).1.is_empty()
}

/// The addresses a pattern stands for, in order (the last range changes fastest). `Ok` with
/// one address when there is no range.
pub fn expand(text: &str) -> Result<Vec<String>, String> {
    let (plain, ranges) = split(text);
    let total = ranges
        .iter()
        .try_fold(1u64, |n, r| n.checked_mul(r.len()))
        .filter(|&n| n <= MAX as u64)
        .ok_or_else(|| format!("that is more than {MAX} downloads"))?;
    let mut out = Vec::with_capacity(total as usize);
    let values: Vec<Vec<String>> = ranges.iter().map(Range::values).collect();
    let mut pick = vec![0usize; values.len()];
    loop {
        let mut s = String::from(plain[0]);
        for (i, v) in values.iter().enumerate() {
            s.push_str(&v[pick[i]]);
            s.push_str(plain[i + 1]);
        }
        out.push(s);
        // Next combination, like counting.
        let mut i = values.len();
        loop {
            if i == 0 {
                return Ok(out);
            }
            i -= 1;
            pick[i] += 1;
            if pick[i] < values[i].len() {
                break;
            }
            pick[i] = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_keep_their_zeros() {
        let urls = expand("https://x.org/img[001-100].jpg").expect("expand");
        assert_eq!(urls.len(), 100);
        assert_eq!(urls[0], "https://x.org/img001.jpg");
        assert_eq!(urls[99], "https://x.org/img100.jpg");
        let urls = expand("https://x.org/p[8-11].png").expect("expand");
        assert_eq!(
            urls,
            [
                "https://x.org/p8.png",
                "https://x.org/p9.png",
                "https://x.org/p10.png",
                "https://x.org/p11.png"
            ]
        );
    }

    #[test]
    fn steps_letters_and_several_ranges() {
        assert_eq!(expand("a[0-20:10]").expect("step"), ["a0", "a10", "a20"]);
        assert_eq!(
            expand("d[c-a].iso").expect("letters"),
            ["dc.iso", "db.iso", "da.iso"]
        );
        assert_eq!(
            expand("s[1-2]/e[01-02]").expect("two"),
            ["s1/e01", "s1/e02", "s2/e01", "s2/e02"]
        );
    }

    #[test]
    fn other_brackets_stay() {
        assert!(!is_pattern("http://[::1]:8080/file.zip"));
        assert!(!is_pattern("https://x.org/a?q=[x]"));
        assert_eq!(
            expand("http://[::1]/f[1-2]").expect("ipv6"),
            ["http://[::1]/f1", "http://[::1]/f2"]
        );
        assert_eq!(
            expand("https://x.org/a.zip").expect("plain"),
            ["https://x.org/a.zip"]
        );
        assert!(!is_pattern("x[a-Z]"));
        assert!(!is_pattern("x[1-5:0]"));
    }

    #[test]
    fn too_many_is_refused() {
        assert!(expand("x[0-9999]").is_ok());
        assert!(expand("x[0-10000]").is_err());
        assert!(expand("x[0-999][0-99]").is_err());
    }
}
