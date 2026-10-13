# 0001 — Download progress lives outside the database

**Status:** accepted

## Context

Segment progress changes thousands of times per second on a fast connection. Writing it to SQLite on every chunk (as some existing projects do) serialises all download threads behind one lock and makes the database the bottleneck.

## Decision

- Each download keeps its own small state file (segment map) next to the output file.
- Only progress that has already been flushed to disk is recorded in it, so a crash can never mark unwritten bytes as done.
- SQLite stores the download list, history, queues and settings, and is written at most about once per second per download.

## Consequences

- Resume works even if the database is lost; the state file is the source of truth for bytes on disk.
- The UI reads live progress from the engine's event stream, not from the database.
