# Benchmark — 2026-10-13 11:41

Local test server, 3 run(s) per cell, medians of successful runs. *Tail* = time spent on the last 10 % of bytes. *Overhead* = bytes sent beyond file size.

## clean · 1 GiB

Fast local server, no limits: raw engine and disk throughput

`size=1073741824&seed=1`

| Client | OK | Time (s) | MiB/s | Tail (s) | Peak conns | GETs | Overhead % | CPU % | RAM (MiB) |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| curl | 3/3 | 4.89 | 209.2 | 0.31 | 1 | 1 | 0.0 | 41 | 4 |
| aria2 x16 | 3/3 | 39.18 | 26.1 | 11.02 | 16 | 16 | 0.9 | 11 | 26 |

## per-conn-limit · 256 MiB

Each connection capped at 4 MiB/s: rewards parallelism

`size=268435456&seed=1&rate=4M`

| Client | OK | Time (s) | MiB/s | Tail (s) | Peak conns | GETs | Overhead % | CPU % | RAM (MiB) |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| curl | 3/3 | 64.04 | 4.0 | 6.41 | 1 | 1 | 0.0 | 0 | 4 |
| aria2 x16 | 3/3 | 8.83 | 29.0 | 5.16 | 16 | 16 | 0.0 | 9 | 26 |

## slow-tail · 256 MiB

One connection crawls at 64 KiB/s: does the client rescue its range?

`size=268435456&seed=1&rate=8M&slow_conn=3:64K`

| Client | OK | Time (s) | MiB/s | Tail (s) | Peak conns | GETs | Overhead % | CPU % | RAM (MiB) |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| curl | 3/3 | 32.05 | 8.0 | 3.18 | 1 | 1 | 0.0 | 1 | 4 |
| aria2 x16 | 3/3 | 10.67 | 24.0 | 8.70 | 16 | 32 | 0.1 | 7 | 25 |

## conn-cap · 256 MiB

Server allows 4 connections and answers 503 beyond that

`size=268435456&seed=1&rate=8M&max_conns=4`

| Client | OK | Time (s) | MiB/s | Tail (s) | Peak conns | GETs | Overhead % | CPU % | RAM (MiB) |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| curl | 3/3 | 32.04 | 8.0 | 3.17 | 1 | 1 | 0.0 | 1 | 4 |
| aria2 x16 | 3/3 | 9.76 | 26.2 | 2.49 | 4 | 22 | 1.4 | 10 | 14 |

## flaky · 256 MiB

Every response is reset after 4 MiB: retry and resume

`size=268435456&seed=1&rate=16M&drop_after=4M`

| Client | OK | Time (s) | MiB/s | Tail (s) | Peak conns | GETs | Overhead % | CPU % | RAM (MiB) |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| curl | 0/3 (ClientError: exit code: 18: ) | – | – | – | – | – | – | – | – |
| aria2 x16 | 3/3 | 6.25 | 40.9 | 4.28 | 16 | 78 | 1.4 | 12 | 25 |

## no-ranges · 128 MiB

Server has no range support: single connection only

`size=134217728&seed=1&ranges=off&rate=16M`

| Client | OK | Time (s) | MiB/s | Tail (s) | Peak conns | GETs | Overhead % | CPU % | RAM (MiB) |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| curl | 3/3 | 8.04 | 15.9 | 0.76 | 1 | 1 | 0.0 | 3 | 4 |
| aria2 x16 | 3/3 | 8.36 | 15.3 | 1.14 | 16 | 16 | 0.7 | 10 | 11 |

## fake-ranges · 128 MiB

Advertises ranges but always returns 200 + full file

`size=134217728&seed=1&range_as_200=1&rate=16M`

| Client | OK | Time (s) | MiB/s | Tail (s) | Peak conns | GETs | Overhead % | CPU % | RAM (MiB) |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| curl | 3/3 | 8.03 | 15.9 | 0.78 | 1 | 1 | 0.0 | 2 | 4 |
| aria2 x16 | 3/3 | 8.31 | 15.4 | 1.09 | 16 | 16 | 0.7 | 9 | 12 |

