# Developing Shardload

How the code is laid out and how to build it. What the app does and how it compares is in the
[README](../README.md); how the parts fit together in [ARCHITECTURE.md](ARCHITECTURE.md), and
the reasons behind the main choices in [decisions](decisions).

## Stack

| Layer | Tech |
|---|---|
| Engine, manager, CLI, native host, test server | Rust (tokio, wreq) |
| Desktop app | Tauri 2 + Svelte 5 (SvelteKit, static) |
| Storage | SQLite (sqlx), request headers encrypted with DPAPI |
| Browser extension | TypeScript, Manifest V3 (+ Firefox build) |
| Media | the engine for HLS/DASH; yt-dlp + deno for sites; ffmpeg to join tracks |

## Repository layout

```
crates/
  engine/       download engine (library): HTTP segments, HLS, DASH, subtitles
  manager/      download list, queue, events, saved logins, batch patterns
  media/        HLS and DASH playlist parsing
  bridge/       messages and the named pipe between the app and its local clients
  native-host/  `shardload-host`, the native messaging host the browser starts
  mcp/          `shardload-mcp`, the MCP server for AI assistants
  cli/          `shard` command-line front end
  testserver/   HTTP server reproducing hostile download scenarios (and HLS/DASH)
  bench/        benchmark runner
app/            Tauri 2 + SvelteKit desktop app, installer (`npm run bundle`)
extension/      browser extension (MV3; Chromium and Firefox builds)
docs/           architecture, design decisions, screenshots
```

## Building

Requires Rust 1.99 (pinned in `rust-toolchain.toml`) and, on Windows, the MSVC build tools. The engine uses
[wreq](https://github.com/0x676e67/wreq) (BoringSSL, for browser TLS fingerprints), whose build also needs
CMake, NASM and LLVM (libclang) on `PATH`, e.g. `winget install Kitware.CMake NASM.NASM LLVM.LLVM`.
Keep the checkout path short: MSBuild fails on BoringSSL objects past 260 characters.

```bash
cargo build
cargo test
```

The desktop app (`app/`) also needs Node 24. It embeds the built web UI, so plain `cargo`
commands leave it out:

```bash
cd app
npm ci
npm run tauri dev     # development, with hot reload
npm run bundle        # installer in target/release/bundle/nsis
```

`npm run bundle` builds the browser host (`shardload-host.exe`) first and ships it next to the app.
When the update signing key is on the machine (`%USERPROFILE%\.tauri\idma.key`, its password
DPAPI-encrypted in `idma.key.password`), it also signs the installer and writes `latest.json`;
uploading both to a GitHub release `v<version>` is what the app's daily update check looks for.
The check needs the releases to be public.
The installer is per user (no administrator rights) and silent with `/S`. Uninstalling removes
the app's keys from the browsers and the "start with Windows" entry, and keeps the download
list in `%APPDATA%\io.github.monurpalan.shardload`.

### Linux

CI builds, tests and packages the app on Ubuntu 24.04 (`.deb` and AppImage, kept as the run's
`linux-packages` artifact). To build locally, install the GTK/WebKit stack Tauri needs and
CMake and libclang for BoringSSL:

```bash
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev cmake clang libclang-dev patchelf
```

```bash
cd app && npm ci && npm run bundle -- --bundles deb,appimage
```

On Linux the app talks to the browser host and the MCP server over a Unix socket in
`~/.local/share/io.github.monurpalan.shardload` (readable by the user only), registers the host
by writing its manifest into each browser's folder (not Snap or Flatpak browsers), opens
magnet links through a desktop entry, sleeps and shuts down through `systemctl`, and scans
with ClamAV (`clamscan`) when virus scanning is on. Watching the clipboard is Windows-only
for now. Request headers and the proxy's password are encrypted with a key the app keeps in
the Secret Service (GNOME Keyring, KWallet); without one they are kept as they are, in the
user's own data folder.

## File and function size

So that no file or function grows into one nobody wants to touch, CI checks both:

- **Files:** at most 600 lines of code (`.rs`, `.ts`, `.svelte`, `.mjs`, `.js`); a Rust
  file's own `#[cfg(test)]` module and test files (`tests/`, `e2e/`, `*.test.ts`) at most
  1000. `node scripts/check-size.mjs` says which file is over and by how much. Split a file by
  what its parts do, a module per part (as the engine's `download/`, `hls/` and `zip/`, or the
  app's `add.rs`, `tasks.rs`, `bridge.rs`); moved code stays as it was, no fixes on the way.
- **The allowlist:** a file that cannot be split yet is in `size-allowlist.json` with its
  current size and why. It may not grow, and when it shrinks the script asks for its new size
  (or for it to come off the list once it is within the limit), so the list only goes down.
- **Functions:** clippy's `too_many_lines` (120 lines, `clippy.toml`) and
  `cognitive_complexity`, errors under `-D warnings`. The functions that were longer when the
  rule came in carry an `#[allow]` with a reason; split one when it next changes, and do not
  add new ones.

