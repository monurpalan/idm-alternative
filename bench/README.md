# Benchmarks

`idma-bench` starts the [test server](../crates/testserver/README.md) in-process, runs every
enabled client on every scenario in [bench.toml](bench.toml), and writes a Markdown summary to
`results/` (raw per-run JSON goes to `results/raw/`, which is not committed).

```bash
cargo run --release -p idma-bench                         # all scenarios, enabled clients
cargo run --release -p idma-bench -- --scenario slow-tail # one scenario
cargo run --release -p idma-bench -- --client "dm,dm x8,IDM" # these clients only (enables disabled ones)
cargo run --release -p idma-bench -- --scenario internet-linode-1g # real internet (off by default)
cargo run --release -p idma-bench -- --out-dir E:/scratch # where downloads are written
```

## What is measured

| Column | Source | Meaning |
|---|---|---|
| Time, MiB/s | wall clock | Process start → exit |
| Tail | server | Time from 90 % of bytes sent to process exit — exposes slow-connection stragglers |
| After | server | Time from the whole file sent to process exit — assembling parts, flushing, shutting down (– = under the 50 ms sampling interval) |
| Peak conns, GETs | server `/_stats` | Concurrency and how many requests the client needed |
| Overhead | server | Bytes sent beyond the file size (retries, overlapping ranges, discarded bodies) |
| CPU %, RAM | client process | Average CPU over the run (100 = one core), peak resident memory |
| OK | verifier | File is complete and byte-identical to the generated content |

Tail and connection counts come from the server, so they work for any client, including
closed-source ones.

Clients run in a different shuffled order every round, so effects that drift over time (network
load, disk write-back) do not always land on the same client.

## Internet scenarios

Scenarios with a `url` download a real file instead (`enabled = false`, so a plain run stays
offline). A unique `?run=` query defeats caches and IDM's duplicate detection. There are no
server-side columns; a run is OK when its size matches and its SHA-256 equals the first successful
download of that scenario. Mind the data: 1000 MiB × clients × rounds adds up quickly, and some
speed-test hosts (proof.ovh.net) block an IP that opens many connections.

## IDM

IDM 6.43 is driven by `IDMan.exe /d url /p dir /f file /n /q`. `/q` (exit when done) only works
for the first instance, so close IDM, tray icon included, before a run. It exits with code 1
even after a successful download (`ignore_exit_code = true`), and never exits after a failed one
(the timeout catches it). It downloads parts into `%APPDATA%\IDM\DwnlData` and joins them at the
end, which shows in the *After* column.

## Gopeed and AB Download Manager

Neither has a command that downloads and waits, so `bench/clients/*.py` add the download to
the running app and exit when it reports it done (`--client "Gopeed,AB DM"`; off by default).
Their *CPU %* and *RAM* columns measure the script, not the app: ignore them.

- **Gopeed** (1.9.3, `winget install monkeyWie.Gopeed.Web`): start its server first, allowing
  the output folder: `gopeed -A 127.0.0.1 -P 9999 -d <data dir> -w C:\tmp\idma-bench`. Tasks
  are added and polled through its REST API (`/api/v1/tasks`); default 16 connections.
- **Gopeed 2.0.0-beta.3** (`gopeed-web` zip from its GitHub release): the same, on port 9998;
  this version names the option `-white-download-dirs` instead of `-w`.
- **AB Download Manager** (1.11.1, from its GitHub release): the app must be running. Downloads
  are added with `ABDownloadManagerCli download add http --start`, and the end is read from the
  app's own record (`~/.abdm/config/download_db/downloadlist/<id>.json`). The CLI takes about a
  second to start, which the measured time includes; the script prints AB DM's own time
  (`completeTime - dateAdded`) as `abdm-ms`. Default 8 connections.
- **Free Download Manager** has no command line or local API to add a download (`fdm.exe`
  with any argument only opens the window), so it is not measured.

## Caveats

- **Write to an SSD.** Many connections writing one file on an HDD measure seek time, not the
  client (on the development machine aria2 drops to ~15 MiB/s on the HDD).
- Loopback numbers show engine overhead and resilience, not real-world network speed; the
  internet scenarios add real downloads.
- Clients that hand off to a long-running process need their exit detection checked before
  their numbers are trusted (done for IDM, see above).
