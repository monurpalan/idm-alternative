# idm-alternative

An open-source download manager for Windows (and Linux) that aims to match Internet Download Manager
feature for feature, built by combining the best ideas from existing open-source projects.

> **Status:** usable day to day on Windows. The engine, the desktop app, browser integration,
> video, torrents and an MCP server work and are installed from a per-user installer.

![The download list with the details of a running download: speed graph and segment map](docs/screenshots/main.png)

## What it does

- **Fast, resilient downloads.** Up to 16 connections per file with time-based work stealing:
  a connection that crawls has its range taken over, so the last percent does not drag. One
  preallocated file (no joining parts at the end), resume after a crash or `kill -9`, checksum
  verification, HTTP/2, browser TLS fingerprints, proxies, global and per-download speed limits.
- **Mirrors and Metalink.** Other addresses of the same file are used at the same time once
  each is shown to serve the same bytes; new connections go to the source that gives a
  connection the most, and a mirror that fails or crawls is dropped. A `.meta4` / `.metalink`
  link brings its mirrors, name and checksum by itself (`dm get URL -m MIRROR`, or "Add
  mirrors" in the app).
- **Browser integration** for Chrome, Edge and Firefox: downloads are taken over with the
  browser's cookies, referrer and user agent, over native messaging (no open localhost port).
  An expired link is refreshed by starting the download again in the browser.
- **Video.** A download button over videos in the page; HLS and DASH downloaded by the engine
  itself (parallel segments, AES-128) with quality, audio track and subtitle choice; sites such
  as YouTube and X through yt-dlp, which the app installs and keeps up to date. Tried on
  2026-10-18 with public pages: YouTube, X, TikTok, Instagram, Facebook, Twitch, Dailymotion,
  Bilibili, VK, Bluesky, SoundCloud, Bandcamp, Internet Archive, PeerTube, Streamable and Odysee
  worked without a login; Vimeo and Reddit asked for one.
- **Site grabber:** reads a page and, to a chosen depth, the pages below it on the same site,
  and lists the files they link to (archives, documents, video, pictures) to pick from;
  nothing is downloaded to find them. Also an MCP tool (`find_site_files`).
- **Torrents:** magnet links and `.torrent` files (librqbit) in the same list and queue, with
  a choice of files; nothing seeds after a download is done. Opening magnet links and
  `.torrent` files from Windows is a setting, off by default.
- **Like IDM:** categories, a queue with automatic retries, a scheduler, "when done" sleep or
  shut down, batch downloads (`photo[001-100].jpg`), clipboard monitoring, saved logins per
  site, a virus scan when a download finishes, a small progress window per download, and a
  tray icon. Interface in English and Turkish.
- **For AI assistants:** `idma-mcp`, an [MCP](https://modelcontextprotocol.io) server installed
  with the app, lets any MCP client add downloads (files, torrents with a choice of files,
  metalinks, video pages at a chosen quality), find the files a site links to, list downloads
  with their progress, wait for them to finish, pause, resume, reorder and remove them, limit
  their speed and choose what happens when all are done. It reaches the app through the same
  per-user pipe as the browser; nothing listens on a network port. Register it in the client
  as a stdio server whose command is `%LOCALAPPDATA%\idm-alternative\idma-mcp.exe`.
- **Where dm stands out is a bad connection.** When one connection crawls, dm finishes in 2.4 s
  because a finished connection takes over the slow one's range; Gopeed and aria2 wait for it
  (~10 s), AB DM much longer (31.6 s). The same holds, less sharply, when connections keep dropping.
- **Gopeed 1.9.3 wrote a corrupt file** when a server advertises ranges but answers every request
  with the whole file (all three runs, even with one connection); everyone else noticed and fell
  back to one connection. It is Gopeed's known issue
  [#1458](https://github.com/GopeedLab/gopeed/issues/1458): 2.0.0-beta.3 gets it right (checked
  here, 6 of 6), but 1.9.3 is still its latest stable release.
- **Gopeed's times swing between sessions, and 2.0 is not faster.** A second session with both
  versions and dm side by side ([local](bench/results/2026-10-18_014929.md),
  [internet](bench/results/2026-10-18_015011.md)):

  | Scenario | dm | Gopeed 1.9.3 | Gopeed 2.0.0-beta.3 |
  |---|---:|---:|---:|
  | Internet, 100 MiB | **2.55** | 5.09 | 5.64 |
  | Clean, 1 GiB, local | **3.37** | 33.81 | 16.39 |
  | One connection crawls | **2.48** | 13.14 | 23.26 |
  | Connections reset every 4 MiB | **1.50** | 9.06 | 12.43 |
  | Server allows 4 connections | **8.35** | 14.17 | 17.17 |
  | Says it has ranges, sends the whole file | **8.03** | corrupt | 8.13 |

  1.9.3 took 12.78 s on the internet run in the first session and 5.09 s here, so read Gopeed's
  plain-download numbers loosely; its slowness with a bad connection repeats in both.
- **On a plain download the gap is smaller.** On 100 MiB dm, aria2 and AB DM are within about a
  second of each other; on 1000 MiB dm is 16 % ahead of aria2 and 35 % ahead of IDM. Part of
  IDM's gap is not network: it downloads parts into its own folder and joins them at the end
  (~1–2.7 s per GiB here), and its start and exit are in the measurement.
- **Looking for losses found one.** A server that forbids extra connections with 403 made dm stop
  (fixed, see above) and makes Gopeed hang; aria2, IDM and AB DM drop to 4 connections. Where
  connections do not help (a per-client limit) everyone ties; the tiny file shows IDM's and AB
  DM's start-up cost, not their engines.
- **Not measured:** a destination on a hard disk or USB stick (IDM's download-then-join should do
  better there; this machine has no HDD), many small files in one go, and weak hardware.
- **One machine, one line, one server.** Everyone ran with default settings. Browser integration,
  site compatibility and video are not measured here.

## License

[MIT](LICENSE)
