# Shardload as a Flatpak

`build.sh` makes `Shardload.flatpak` from the `.deb` a release builds, on Ubuntu 24.04 (CI does
this for every change, and installs and smoke-tests the result):

```sh
packaging/flatpak/build.sh target/release/bundle/deb/Shardload_*.deb Shardload.flatpak
flatpak install --user Shardload.flatpak
flatpak run io.github.monurpalan.Shardload
```

It runs on the GNOME 47 runtime from Flathub, which `flatpak install` fetches. The tray icon's
library (libayatana-appindicator) is not in that runtime, so it is taken from Ubuntu's packages.
For Flathub itself the app would have to be built from source inside the manifest; that is not
done yet.

## What is different in the sandbox

- **Browser extension.** Browsers outside the sandbox cannot start a program inside it. The app
  therefore registers a small script as its browser host (in `~/.var/app/io.github.monurpalan.Shardload/data/…`)
  which runs `flatpak run --command=shardload-host io.github.monurpalan.Shardload`; the host
  then reaches the app as usual. This works for Chrome, Chromium, Edge, Brave, Vivaldi and
  Firefox installed from the distribution or their own packages. **Browsers that are themselves
  a Flatpak or a Snap are not reached**: they read native messaging hosts from inside their own
  sandbox, which nothing outside can write to.
- **MCP server.** Point the assistant at
  `flatpak run --command=shardload-mcp io.github.monurpalan.Shardload`.
- **Files.** The app may write anywhere in your home folder (`--filesystem=home`), where
  downloads usually go; another drive needs `flatpak override --user --filesystem=/media/…`.
- **"When done" rules.** A program a rule runs runs inside the sandbox, with what the runtime
  has; programs of the system outside are not there.
