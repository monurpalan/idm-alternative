//! `shard zip`: an archive's list of files, and files taken out of it, without downloading the
//! whole archive.

use super::*;

pub(super) async fn zip_list(url: &str, headers: &[(String, String)], opts: &Options) -> ExitCode {
    let zip = match RemoteZip::open(url, headers, opts).await {
        Ok(z) => z,
        Err(e) => {
            eprintln!("shard: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{:>10}  {:>10}  {:<8}  name", "size", "packed", "method");
    let mut total = 0;
    let mut files = 0;
    for e in zip.entries() {
        let note = match e.unsupported() {
            Some(_) if e.encrypted => "  (encrypted)",
            Some(_) => "  (cannot extract)",
            None => "",
        };
        let method = if e.is_dir {
            String::new()
        } else {
            files += 1;
            total += e.size;
            shardload_engine::method_name(e.method)
        };
        println!(
            "{:>10}  {:>10}  {method:<8}  {}{note}",
            if e.is_dir {
                String::new()
            } else {
                human(e.size as f64)
            },
            if e.is_dir {
                String::new()
            } else {
                human(e.compressed_size as f64)
            },
            e.name
        );
    }
    eprintln!(
        "{files} files, {} unpacked; archive {}{}",
        human(total as f64),
        human(zip.size() as f64),
        if zip.is_zip64() { " (ZIP64)" } else { "" }
    );
    ExitCode::SUCCESS
}

pub(super) async fn zip_get(
    url: String,
    patterns: &[String],
    out_dir: PathBuf,
    overwrite: bool,
    headers: Vec<(String, String)>,
    opts: Options,
    quiet: bool,
) -> ExitCode {
    let zip = match RemoteZip::open(&url, &headers, &opts).await {
        Ok(z) => z,
        Err(e @ Error::NoRanges) => {
            eprintln!("shard: {e}");
            eprintln!(
                "shard: downloading the whole archive to {}",
                display_dir(&out_dir)
            );
            let target = Target {
                output: None,
                dir: Some(out_dir),
                overwrite: false,
            };
            return get(url, Vec::new(), headers, opts, target, quiet).await;
        }
        Err(e) => {
            eprintln!("shard: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (chosen, unmatched) = shardload_engine::select_zip_entries(zip.entries(), patterns);
    if !unmatched.is_empty() {
        eprintln!(
            "shard: not in the archive: {} (see `shard zip list`)",
            unmatched.join(", ")
        );
        return ExitCode::from(2);
    }
    let cancel = CancellationToken::new();
    let (tx, rx) = tokio::sync::watch::channel(Progress::default());
    let ctrl_c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            ctrl_c.cancel();
        }
    });
    let printer = (!quiet).then(|| tokio::spawn(print_progress(rx)));
    let result = zip
        .extract(&chosen, &out_dir, overwrite, Some(&tx), &cancel)
        .await;
    if let Some(p) = printer {
        p.abort();
        eprintln!();
    }
    let done = match result {
        Ok(d) => d,
        Err(Error::Cancelled) => {
            eprintln!("shard: cancelled; run the same command again to continue");
            return ExitCode::from(130);
        }
        Err(e) => {
            eprintln!("shard: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = 0;
    for d in done.iter().filter(|d| !d.name.ends_with('/')) {
        match (&d.error, d.skipped) {
            (Some(why), _) => {
                failed += 1;
                eprintln!("failed   {}: {why}", d.name);
            }
            (None, true) => {
                if !quiet {
                    eprintln!("there    {}", d.name);
                }
            }
            (None, false) => {
                if !quiet {
                    eprintln!("saved    {}", d.name);
                }
            }
        }
    }
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
