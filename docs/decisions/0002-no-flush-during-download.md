# 0002 — No disk flush during a download

**Status:** accepted

## Context

The first engine version flushed the output file (`FlushFileBuffers`) before every 2-second
checkpoint, so the state file could never claim bytes that were not on disk. On the
development machine (DRAM-less SATA SSD) each flush took 4.7–5.3 s under load, and Windows
blocked every write to the file meanwhile: a download that should take ~2 s took ~10 s.

## Decision

- Checkpoints (every 2 s) save positions captured *before* the state is written, without
  flushing. Bytes handed to the OS survive the process crashing or being killed, which is the
  failure the resume feature exists for.
- A real flush happens only when a download stops early (pause, cancel, error), where a stall
  costs nothing.
- No flush on completion — same as aria2 and IDM.

## Consequences

- After an OS crash or power loss, the state may claim up to a few seconds of bytes that never
  reached the disk, and resuming would leave zeros there. Accepted: rare, and a checksum, when
  the user supplies one, catches it.
- Download speed is no longer capped by flush stalls.
