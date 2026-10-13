# Benchmarks

`idma-bench` starts the [test server](../crates/testserver/README.md) in-process, runs every
enabled client on every scenario in [bench.toml](bench.toml), and writes a Markdown summary to
`results/` (raw per-run JSON goes to `results/raw/`, which is not committed).

```bash
cargo run --release -p idma-bench                         # all scenarios, enabled clients
cargo run --release -p idma-bench -- --scenario slow-tail # one scenario
cargo run --release -p idma-bench -- --client IDM         # also enables disabled clients
cargo run --release -p idma-bench -- --out-dir E:/scratch # where downloads are written
```

## What is measured

| Column | Source | Meaning |
|---|---|---|
| Time, MiB/s | wall clock | Process start → exit |
| Tail | server | Time from 90 % of bytes sent to process exit — exposes slow-connection stragglers |
| Peak conns, GETs | server `/_stats` | Concurrency and how many requests the client needed |
| Overhead | server | Bytes sent beyond the file size (retries, overlapping ranges, discarded bodies) |
| CPU %, RAM | client process | Average CPU over the run (100 = one core), peak resident memory |
| OK | verifier | File is complete and byte-identical to the generated content |

Tail and connection counts come from the server, so they work for any client, including
closed-source ones.

## Caveats

- **Write to an SSD.** Many connections writing one file on an HDD measure seek time, not the
  client (on the development machine aria2 drops to ~15 MiB/s on the HDD).
- Loopback numbers show engine overhead and resilience, not real-world network speed; the
  internet scenarios add real downloads.
- Clients that hand off to a long-running process (IDM) need their exit detection checked
  before their numbers are trusted.
