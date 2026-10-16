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

## Caveats

- **Write to an SSD.** Many connections writing one file on an HDD measure seek time, not the
  client (on the development machine aria2 drops to ~15 MiB/s on the HDD).
- Loopback numbers show engine overhead and resilience, not real-world network speed; the
  internet scenarios add real downloads.
- Clients that hand off to a long-running process need their exit detection checked before
  their numbers are trusted (done for IDM, see above).
