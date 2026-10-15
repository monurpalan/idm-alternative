# idm-alternative

An open-source download manager that aims to match Internet Download Manager feature-for-feature, built by combining the best ideas from existing open-source projects.

> **Status:** early development — the engine first. `dm get` works from the command line; no app yet.

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

## License

[MIT](LICENSE)
