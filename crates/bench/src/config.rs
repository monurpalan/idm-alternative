use std::path::Path;

use anyhow::Context;
use idma_testserver::scenario::Size;
use serde::{Deserialize, Deserializer};

#[derive(Deserialize)]
pub struct Config {
    #[serde(default = "default_repeat")]
    pub repeat: u32,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    pub client: Vec<Client>,
    pub scenario: Vec<Scenario>,
}

fn default_repeat() -> u32 {
    3
}

fn default_timeout() -> u64 {
    300
}

#[derive(Deserialize)]
pub struct Client {
    pub name: String,
    #[serde(deserialize_with = "expand_env")]
    pub program: String,
    pub args: Vec<String>,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    /// Judge the run by the file alone: some clients (IDM) exit non-zero after success.
    #[serde(default)]
    pub ignore_exit_code: bool,
}

fn enabled_default() -> bool {
    true
}

#[derive(Deserialize)]
pub struct Scenario {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub size: Size,
    #[serde(default = "default_seed")]
    pub seed: u64,
    #[serde(default)]
    pub query: String,
    /// A real download URL instead of the local test server. Such runs have no server-side
    /// numbers (tail, GETs, overhead), and are verified by size and by comparing every
    /// client's SHA-256 with the first successful download.
    pub url: Option<String>,
    /// Disabled scenarios only run when selected with `--scenario` (internet ones are, so
    /// a plain run stays offline).
    #[serde(default = "enabled_default")]
    pub enabled: bool,
}

fn default_seed() -> u64 {
    1
}

/// Expands `${VAR}` occurrences from the environment; unknown variables are left as is.
fn expand_env<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let var = &after[..end];
                match std::env::var(var) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => out.push_str(&rest[start..start + 3 + end]),
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

pub fn load(path: &Path) -> anyhow::Result<Config> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}
