# idm-alternative

An open-source download manager that aims to match Internet Download Manager feature-for-feature, built by combining the best ideas from existing open-source projects.

> **Status:** early development — the engine is done: `dm get` works from the command line and is
> benchmarked against IDM below. No app yet.

## Goals

- **Engine:** dynamic segmentation with time-based work stealing, a single preallocated output file, crash-safe resume, `If-Range` validation, HTTP/2.
- **Browser integration:** download capture with full cookie/header forwarding over native messaging (no open localhost port), browser-assisted link refresh.
- **Video:** in-page media detection with a floating download button, HLS/DASH, quality and subtitle selection.
- **Measured, not claimed:** every engine milestone is benchmarked against curl, aria2 and IDM on a hostile test server.

## Stack

| Layer | Tech |
|---|---|
| Engine, CLI, native host, test server | Rust (tokio) |
| Desktop app | Tauri 2 + Svelte 5 |
| Storage | SQLite (sqlx) |
| Browser extension | TypeScript, Manifest V3 (+ Firefox build) |
| Media | yt-dlp, N_m3u8DL-RE, ffmpeg (bundled sidecars) |

## Repository layout

```
crates/
  engine/       download engine (library)
  manager/      download list, queue and events on top of the engine
  cli/          `dm` command-line front end
  testserver/   HTTP server reproducing hostile download scenarios
  bench/        benchmark runner
app/            Tauri + Svelte desktop app (planned)
extension/      browser extension (planned)
native-host/    native messaging host (planned)
docs/           architecture and design decisions
```

## Building

Requires Rust 1.99 (pinned in `rust-toolchain.toml`) and, on Windows, the MSVC build tools. The engine uses
[wreq](https://github.com/0x676e67/wreq) (BoringSSL, for browser TLS fingerprints), whose build also needs
CMake, NASM and LLVM (libclang) on `PATH`, e.g. `winget install Kitware.CMake NASM.NASM LLVM.LLVM`.
Keep the checkout path short: MSBuild fails on BoringSSL objects past 260 characters.

```bash
cargo build --workspace
cargo test --workspace
```

## Benchmarks

`dm` (the engine's command-line front end) against IDM 6.43, aria2 and curl, measured with
[`idma-bench`](bench/README.md) on 2026-10-16. Every file was verified byte for byte (local) or by
size and SHA-256 (internet). Medians of 3 runs (internet 1000 MiB: 2), clients in shuffled order.

**Real internet** (Linode Frankfurt, from Turkey, ~470 Mbit/s line):

| File | dm | aria2 (16 conns) | IDM (8 conns) | curl |
|---|---:|---:|---:|---:|
| 1000 MiB | **13.98 s** | 14.32 s | 19.90 s | 28.79 s |
| 100 MiB | **2.68 s** | 3.26 s | 5.48 s | 4.41 s |

**Local test server**, seconds ([full table](bench/results/2026-10-16_111231.md)). `dm x8` uses
IDM's default of 8 connections; *after* is the time between the server sending the last byte and
the client exiting:

| Scenario | dm | dm x8 | IDM | IDM after | aria2 |
|---|---:|---:|---:|---:|---:|
| Clean, 1 GiB | **3.15** | 4.44 | 7.50 | 2.74 | 8.91 |
| 4 MiB/s per connection | **4.04** | 8.05 | 10.08 | 0.42 | 5.27 |
| One connection crawls at 64 KiB/s | **2.40** | 4.43 | 8.80 | 2.37 | 10.28 |
| Server allows 4 connections | 8.35 | **8.15** | 9.29 | 0.39 | 8.23 |
| Connections reset every 4 MiB | **1.50** | 3.10 | 4.94 | 0.44 | 1.58 |
| No range support | **8.03** | 8.03 | 9.12 | 0.31 | 8.12 |

What this does and does not say:

- **Much of IDM's gap is not network speed.** IDM downloads parts into its own temporary folder and
  joins them into the file afterwards (2.7 s for 1 GiB on this disk), and the measurement includes
  IDM starting and exiting. With equal connections and that time left out, dm is 8–17 % faster on
  plain downloads and about 30 % faster when a connection crawls or keeps dropping — the cases the
  engine's work stealing targets.
- **aria2 ties dm on a plain internet download** (the two 1000 MiB rounds split one each).
- **One machine, one line, one server.** IDM was run with default settings; more connections in
  IDM would narrow the gap. Browser integration, site compatibility and video — where IDM is
  strong — are not measured here.

## License

[MIT](LICENSE)
