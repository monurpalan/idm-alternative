# testserver

HTTP server that reproduces the ways real download servers misbehave, so the engine and the
benchmarks can be tested against them deterministically.

File content is generated from `(seed, offset)`, so any size is served without disk access and
a download can be verified byte-for-byte without a reference copy.

```bash
cargo run --release -p idma-testserver -- serve --bind 127.0.0.1:8787
cargo run --release -p idma-testserver -- verify downloads/test.bin --size 100M --seed 1
```

Serves HTTP/1.1 and h2c (HTTP/2 with prior knowledge), or with `--tls` HTTPS on a fresh self-signed
certificate offering HTTP/2 and HTTP/1.1 through ALPN (the PEM is printed on start). In tests, use
`serve_tls` / `serve_plain` from the library. Every finished response is logged with its range,
bytes sent, duration and whether it completed.

Requests in absolute form (`GET http://any.host/file/...`) are served too, so the server can stand
in for a forward proxy.

## `GET /file/{name}?<params>`

Sizes and rates accept `K`, `M`, `G` suffixes (binary units). Flags accept `1/0`, `on/off`, `true/false`.

| Parameter | Default | Effect |
|---|---|---|
| `size` | `100M` | File size |
| `seed` | `1` | Content seed |
| `ranges` | `on` | Honour `Range` and send `Accept-Ranges: bytes` |
| `range_as_200` | `off` | Advertise ranges but answer ranged requests with `200` + whole file |
| `rate` | — | Per-connection limit in bytes/s |
| `slow_conn` | — | `n:rate` — the n-th GET to this URL is served at `rate` |
| `stall_after` | — | Stop sending after this many bytes, keep the connection open |
| `drop_after` | — | Reset the connection after this many bytes |
| `max_conns` | — | `503` + `Retry-After: 1` beyond this many concurrent responses |
| `ttl` | — | `403` from this many seconds after the URL's first request |
| `etag` | `static` | `static`, `rotate` (new validator every response) or `none` |
| `html_error_after` | — | GETs after the first n return `200 text/html` |
| `no_length` | `off` | No `Content-Length`, chunked, no range support |
| `redirect` | `0` | Redirect this many times before serving |
| `redirect_unique` | `off` | Each redirect leads to a new URL (adds `hop=n`), like a CDN signing every link; with `ttl`, each one expires on its own |
| `cd` | — | `Content-Disposition` filename (ASCII fallback + `filename*` UTF-8) |
| `latency_ms` | `0` | Delay before response headers |

`If-Range` is honoured: a validator that no longer matches turns a range request into `200`
with the whole file, as RFC 9110 requires.

Per-URL state (GET counter, TTL clock, validator generation) is keyed by the full request URI,
so each distinct URL is an independent scenario.

## `GET /_stats` · `POST /_reset`

`/_stats` returns, per URL: GET count, current and peak concurrent responses, bytes sent, how many
GETs came over HTTP/2, how many TCP connections they came on (when started through `serve_plain`,
`serve_tls` or the binary), and the last User-Agent.
The benchmark runner uses it to see how many connections a client opened. `/_reset` clears it.
