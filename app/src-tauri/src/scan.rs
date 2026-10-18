//! Scanning finished downloads for viruses, like IDM: Microsoft Defender's command-line
//! scanner by default, or a program the user names, with `{file}` where the path goes.

use std::path::{Path, PathBuf};

/// How a scan came out.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Clean,
    /// Defender found something (and, by default, quarantined it).
    Threat,
    /// Another scanner answered with this exit code; what it means is the scanner's.
    Other(i32),
}

/// Defender's scanner, where Windows keeps it.
fn defender() -> Option<PathBuf> {
    let dir = std::env::var_os("ProgramFiles")?;
    let exe = Path::new(&dir)
        .join("Windows Defender")
        .join("MpCmdRun.exe");
    exe.is_file().then_some(exe)
}

/// A command line split into words, keeping quoted parts (`"C:\Program Files\x.exe" {file}`).
pub fn words(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in command.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            c => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(word);
    }
    out
}

/// Scans `file` with `scanner` (empty: Defender).
pub async fn scan(file: &Path, scanner: &str) -> Result<Verdict, String> {
    let path = file.to_string_lossy();
    let (program, args, is_defender) = if scanner.trim().is_empty() && !cfg!(windows) {
        // ClamAV: 1 = a threat found, like Defender's 2.
        let args = ["--no-summary", &*path].map(str::to_owned).to_vec();
        return run_scanner(PathBuf::from("clamscan"), args, Some(1)).await;
    } else if scanner.trim().is_empty() {
        let exe = defender().ok_or("Microsoft Defender's scanner (MpCmdRun.exe) was not found")?;
        let args = ["-Scan", "-ScanType", "3", "-File", &path]
            .map(str::to_owned)
            .to_vec();
        (exe, args, true)
    } else {
        let mut w = words(scanner);
        if w.is_empty() {
            return Err("the scanner command is empty".into());
        }
        let program = PathBuf::from(w.remove(0));
        let mut args: Vec<String> = w.into_iter().map(|a| a.replace("{file}", &path)).collect();
        if !scanner.contains("{file}") {
            args.push(path.to_string());
        }
        (program, args, false)
    };
    run_scanner(program, args, is_defender.then_some(2)).await
}

/// Runs a scanner; `threat` is its exit code for "found something".
async fn run_scanner(
    program: PathBuf,
    args: Vec<String>,
    threat: Option<i32>,
) -> Result<Verdict, String> {
    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let status = cmd
        .status()
        .await
        .map_err(|e| format!("could not run {}: {e}", program.display()))?;
    Ok(match status.code() {
        Some(0) => Verdict::Clean,
        Some(code) if Some(code) == threat => Verdict::Threat,
        Some(code) => Verdict::Other(code),
        None => Verdict::Other(-1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_a_command_line() {
        assert_eq!(
            words(r#""C:\Program Files\AV\scan.exe" /quiet "{file}""#),
            [r"C:\Program Files\AV\scan.exe", "/quiet", "{file}"]
        );
        assert_eq!(words("  scan.exe   {file} "), ["scan.exe", "{file}"]);
        assert_eq!(words(r#"x.exe """#), ["x.exe", ""]);
        assert!(words("   ").is_empty());
    }
}
