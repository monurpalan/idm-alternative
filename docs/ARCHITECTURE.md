# Architecture

Living document; filled in as each part lands. The reasoning behind larger choices is in [decisions](decisions).

## Components

```
browser extension ──native messaging──▶ native-host ──named pipe──▶ app (Tauri)
                                                                      │
                                                                      ▼
                                                        manager (list, queue, events)
                                                                      │
                                                                      ▼
                                                               engine (Rust lib)
                                                                      │
                                                     per-download state file + SQLite (list/history)
```

- **engine** (`crates/engine`): one download, start to finish — probing, segmentation,
  resume, limits, checksum. Knows nothing about lists.
- **manager** (`crates/manager`): the download list in SQLite, the queue, pause/resume/remove,
  and an event stream (`Added`, `Updated`, `Progress` ~4/s, `Removed`). The app is a thin
  shell over it; the native host will add downloads through the same API.

## Design decisions

Recorded as short ADRs in [decisions/](decisions/).
