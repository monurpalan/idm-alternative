//! What happens when a download finishes: it is moved by a rule, unpacked, handed to the
//! user's command, and the user's webhook is told.
//!
//! A download may ask only to be unpacked or moved ([`After`]): that is what the add dialog,
//! the browser extension and MCP clients can set. A command or a webhook only ever comes from
//! the user's own rules in the settings ([`Rule`]), so a web page or an MCP client cannot make
//! Shardload run a program or send anything anywhere.

mod run;
mod unpack;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::task::{Category, Task};

pub use run::words;
pub use unpack::{Unpacked, can_unpack};

/// What one download asks for itself when it finishes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct After {
    /// Unpack it (ZIP, tar, tar.gz, tar.xz) into a folder next to it, named after it.
    pub unpack: bool,
    /// Move it into this folder.
    pub move_to: Option<PathBuf>,
}

/// One of the user's rules: which finished downloads, and what happens to them. Every
/// condition given must hold; a rule without any applies to every download.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub category: Option<Category>,
    /// The link's host or a domain above it (`example.com` takes `dl.example.com`).
    pub site: Option<String>,
    /// The file name, with `*` and `?` (`*.iso`), letter case aside.
    pub name: Option<String>,
    pub unpack: bool,
    pub move_to: Option<PathBuf>,
    /// A program and its arguments, run without a shell; `{file}` and `{dir}` are replaced by
    /// the file's path and its folder.
    pub command: Option<String>,
    /// An `http://` or `https://` address told about the download (a POST of JSON).
    pub webhook: Option<String>,
}

impl Rule {
    /// Whether the rule takes this download; an empty condition is no condition.
    pub fn matches(&self, task: &Task) -> bool {
        let name = task.name.as_deref().unwrap_or_default();
        self.category.is_none_or(|c| c == task.category)
            && given(&self.site).is_none_or(|s| on_site(&task.url, s))
            && given(&self.name).is_none_or(|p| glob(p, name))
    }

    /// Why the rule cannot be kept, if it cannot: an empty command, a webhook that is not
    /// a web address.
    pub fn check(&self) -> Result<(), String> {
        if let Some(c) = given(&self.command)
            && words(c)
                .first()
                .is_none_or(|program| program.trim().is_empty())
        {
            return Err("the command has no program".into());
        }
        if let Some(w) = given(&self.webhook) {
            let ok = url::Url::parse(w.trim())
                .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some());
            if !ok {
                return Err(format!("the webhook is not a web address: {w}"));
            }
        }
        Ok(())
    }
}

/// How long a command may run before it is stopped, and how long a webhook may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub command: Duration,
    pub webhook: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            command: Duration::from_secs(10 * 60),
            webhook: Duration::from_secs(15),
        }
    }
}

/// Everything to do for one finished download: its own wishes and those of the rules that
/// take it, merged. Its own folder comes before a rule's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) unpack: bool,
    pub(crate) move_to: Option<PathBuf>,
    pub(crate) commands: Vec<String>,
    pub(crate) webhooks: Vec<String>,
}

impl Plan {
    pub(crate) fn of(task: &Task, rules: &[Rule]) -> Self {
        let own = task.after.clone().unwrap_or_default();
        let mut plan = Plan {
            unpack: own.unpack,
            move_to: own.move_to,
            ..Plan::default()
        };
        for r in rules.iter().filter(|r| r.matches(task)) {
            plan.unpack |= r.unpack;
            if plan.move_to.is_none() {
                plan.move_to.clone_from(&r.move_to);
            }
            plan.commands.extend(given(&r.command).map(str::to_owned));
            plan.webhooks.extend(given(&r.webhook).map(str::to_owned));
        }
        plan
    }

    pub(crate) fn is_empty(&self) -> bool {
        *self == Plan::default()
    }
}

/// How one action went, kept with the download for its details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {
    /// `move`, `unpack`, `command` or `webhook`.
    pub action: String,
    pub ok: bool,
    /// The folder it went to or was unpacked into, the program, the webhook's address; or,
    /// when it could not be done at all, why.
    pub detail: String,
    /// Files unpacked, the program's exit code, the webhook's HTTP status.
    #[serde(default)]
    pub count: Option<i64>,
    /// Entries of the archive left out (named in the log).
    #[serde(default)]
    pub left_out: usize,
}

impl Done {
    pub(crate) fn ok(action: &str, detail: impl Into<String>, count: Option<i64>) -> Self {
        Self {
            action: action.into(),
            ok: true,
            detail: detail.into(),
            count,
            left_out: 0,
        }
    }

    pub(crate) fn failed(action: &str, detail: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            ok: false,
            detail: detail.into(),
            count: None,
            left_out: 0,
        }
    }
}

/// A text that is set and not blank.
fn given(text: &Option<String>) -> Option<&str> {
    text.as_deref().filter(|t| !t.trim().is_empty())
}

/// Whether `url`'s host is `site` or below it.
fn on_site(url: &str, site: &str) -> bool {
    let site = site.trim().trim_start_matches('.').to_ascii_lowercase();
    let Some(host) = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
    else {
        return false;
    };
    !site.is_empty() && (host == site || host.ends_with(&format!(".{site}")))
}

/// `*` any run of characters, `?` one; letter case aside.
pub(crate) fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.trim().to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    // The classic two-pointer match with backtracking to the last `*`.
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// The folder a finished file goes into when unpacked: next to it, named after it without
/// its archive extensions, `name (1)` when that is taken.
pub(crate) fn unpack_folder(file: &Path) -> PathBuf {
    let dir = file.parent().unwrap_or(Path::new("."));
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    let stem = [".tar.gz", ".tar.xz", ".tgz", ".txz", ".tar", ".zip"]
        .iter()
        .find_map(|ext| {
            lower
                .ends_with(ext)
                .then(|| &name[..name.len() - ext.len()])
        })
        .unwrap_or(&name);
    let stem = if stem.is_empty() { "unpacked" } else { stem };
    let mut folder = dir.join(stem);
    let mut n = 1;
    while folder.exists() {
        folder = dir.join(format!("{stem} ({n})"));
        n += 1;
    }
    folder
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(url: &str, name: &str) -> Task {
        crate::task::tests::sample(url, name)
    }

    #[test]
    fn rules_match_by_type_site_and_name() {
        let t = task("https://dl.example.com/a/Ubuntu.ISO", "Ubuntu.ISO");
        assert!(Rule::default().matches(&t));
        let r = |category, site: Option<&str>, name: Option<&str>| Rule {
            category,
            site: site.map(str::to_owned),
            name: name.map(str::to_owned),
            ..Rule::default()
        };
        assert!(r(Some(Category::Compressed), None, None).matches(&t));
        assert!(!r(Some(Category::Video), None, None).matches(&t));
        assert!(r(None, Some("example.com"), None).matches(&t));
        assert!(r(None, Some("dl.example.com"), None).matches(&t));
        assert!(!r(None, Some("ample.com"), None).matches(&t));
        assert!(!r(None, Some("other.org"), None).matches(&t));
        assert!(r(None, None, Some("*.iso")).matches(&t));
        assert!(r(None, None, Some("ubuntu?iso")).matches(&t));
        assert!(!r(None, None, Some("*.zip")).matches(&t));
        assert!(r(Some(Category::Compressed), Some("example.com"), Some("u*")).matches(&t));
        assert!(!r(Some(Category::Compressed), Some("example.com"), Some("x*")).matches(&t));
    }

    #[test]
    fn globs() {
        assert!(glob("*", ""));
        assert!(glob("a*b*c", "aXXbYYc"));
        assert!(!glob("a*b*c", "aXXbYY"));
        assert!(glob("*.tar.gz", "src-1.0.TAR.GZ"));
        assert!(!glob("?", ""));
        assert!(glob("**x", "x"));
    }

    #[test]
    fn plans_merge_its_own_wishes_and_the_rules() {
        let mut t = task("https://example.com/a.zip", "a.zip");
        let rules = vec![
            Rule {
                name: Some("*.zip".into()),
                unpack: true,
                move_to: Some("C:/rule".into()),
                command: Some("tool {file}".into()),
                ..Rule::default()
            },
            Rule {
                site: Some("example.com".into()),
                webhook: Some("http://127.0.0.1/hook".into()),
                command: Some("  ".into()),
                ..Rule::default()
            },
            Rule {
                name: Some("*.iso".into()),
                command: Some("never".into()),
                ..Rule::default()
            },
        ];
        let plan = Plan::of(&t, &rules);
        assert!(plan.unpack);
        assert_eq!(plan.move_to, Some("C:/rule".into()));
        assert_eq!(plan.commands, ["tool {file}"]);
        assert_eq!(plan.webhooks, ["http://127.0.0.1/hook"]);
        t.after = Some(After {
            unpack: false,
            move_to: Some("C:/own".into()),
        });
        assert_eq!(Plan::of(&t, &rules).move_to, Some("C:/own".into()));
        assert!(Plan::of(&t, &[]).move_to.is_some());
        t.after = None;
        assert!(Plan::of(&t, &[]).is_empty());
    }

    #[test]
    fn a_download_cannot_carry_a_command_or_a_webhook() {
        // What the extension or an MCP client sends is read as `After`: anything else in it is
        // dropped, not run.
        let sent = r#"{"unpack": true, "command": "calc.exe", "webhook": "http://evil/"}"#;
        let after: After = serde_json::from_str(sent).expect("parse");
        assert_eq!(
            after,
            After {
                unpack: true,
                move_to: None
            }
        );
    }

    #[test]
    fn rules_are_checked() {
        let bad_hook = Rule {
            webhook: Some("file:///etc/passwd".into()),
            ..Rule::default()
        };
        assert!(bad_hook.check().is_err());
        let empty = Rule {
            command: Some(r#""""#.into()),
            ..Rule::default()
        };
        assert!(empty.check().is_err());
        let blank = Rule {
            site: Some(" ".into()),
            name: Some(String::new()),
            command: Some("  ".into()),
            webhook: Some(String::new()),
            ..Rule::default()
        };
        assert_eq!(blank.check(), Ok(()), "blank fields are no fields");
        assert!(blank.matches(&task("https://a.b/c", "c")));
        let fine = Rule {
            command: Some(r#""C:\Tools\x.exe" {file}"#.into()),
            webhook: Some("https://hooks.example.com/x".into()),
            ..Rule::default()
        };
        assert_eq!(fine.check(), Ok(()));
    }

    #[test]
    fn unpack_folders_are_named_after_the_archive() {
        let d = std::env::temp_dir().join(format!("shardload-unpack-names-{}", std::process::id()));
        std::fs::create_dir_all(&d).expect("dir");
        assert_eq!(unpack_folder(&d.join("src-1.0.tar.gz")), d.join("src-1.0"));
        assert_eq!(unpack_folder(&d.join("A.ZIP")), d.join("A"));
        std::fs::create_dir_all(d.join("A")).expect("taken");
        assert_eq!(unpack_folder(&d.join("A.zip")), d.join("A (1)"));
        std::fs::remove_dir_all(&d).ok();
    }
}
