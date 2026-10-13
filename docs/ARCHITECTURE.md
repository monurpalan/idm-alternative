# Architecture

Living document; filled in as each part lands. The reasoning behind larger choices is in [decisions](decisions).

## Components

```
browser extension ──native messaging──▶ native-host ──named pipe──▶ app (Tauri)
                                                                      │
                                                                      ▼
                                                               engine (Rust lib)
                                                                      │
                                                     per-download state file + SQLite (list/history)
```

## Design decisions

Recorded as short ADRs in [decisions/](decisions/).
