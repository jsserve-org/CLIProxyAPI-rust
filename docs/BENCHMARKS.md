# Performance comparison

These results compare this Rust port with the pinned CLIProxyAPI Go revision
`09a29bd345bc44c473abe7fd07859e32df2ea543`. They are provisional until the
full parity contract passes: implementing more providers and background
services can change both memory and CPU use.

## 2026-09-11 baseline

The test host was an Apple M4 with 12 CPU cores and 24 GB RAM, running macOS
arm64. Both projects were built as optimized, stripped native binaries. The Go
server used `commercial-mode: true` to remove the embedded management page
from the comparison. Both servers used the same deterministic loopback
upstream, credentials, payloads, and concurrency levels. Each throughput case
was run three times; the table reports the median.
Go access-log output was redirected away from the measured client path, but
the logger itself could not be disabled in the pinned build.

| Measurement | Pinned Go | Rust | Rust/Go |
| --- | ---: | ---: | ---: |
| Binary size | 57 MB | 9.2 MB | 0.16x |
| Cold idle RSS | about 32 MB | about 3 MB | 0.09x |
| Fresh tokenizer + 64 slow streams RSS | about 86 MB | about 62 MB | 0.72x |
| Extended observed-load RSS | about 96 MB | about 64 MB | 0.67x |

The streaming fixture emitted 32 server-sent events totaling about 9 KB. With
an artificial 2 ms delay before every event, throughput was intentionally
upstream-limited and nearly equal:

| Concurrent streams | Go requests/s | Rust requests/s | Go CPU time | Rust CPU time |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 12.16 | 12.26 | — | — |
| 3 | 36.65 | 36.97 | 0.69 s | 0.21 s |
| 10 | 123.62 | 125.06 | — | — |
| 64 | 839.92 | 852.41 | 1.20 s | 0.43 s |

With the same events delivered immediately, Rust sustained approximately
3.5x, 2.4x, 1.9x, and 1.7x the Go throughput at concurrency 1, 3, 10, and 64,
respectively. The delayed case is more representative when model latency
dominates; its CPU figures show the proxy overhead that remains hidden by
wall-clock latency.

The warmed `/v1/messages/count_tokens` fixture exercised JSON parsing,
Claude-to-Codex translation, tool-schema normalization, and O200k tokenization:

| Concurrency | Go requests/s | Rust requests/s | Rust/Go |
| ---: | ---: | ---: | ---: |
| 1 | 2,310 | 13,540 | 5.9x |
| 3 | 6,985 | 31,424 | 4.5x |
| 10 | 15,194 | 58,590 | 3.9x |
| 64 | 16,032 | 69,507 | 4.3x |

All listed runs completed with zero failed requests. Local token-count parity
for this fixture is now exact at 162 tokens on both implementations.

## Interpretation

The current Rust core has a materially smaller idle footprint and uses less
CPU on the implemented Codex path. Peak memory is not 3 MB under load: TLS,
the tokenizer, request bodies, and live stream buffers grow the process into
the tens of megabytes. A real provider will usually determine latency, so the
most useful benefit is headroom for concurrent agents rather than headline
loopback throughput.

For a machine serving roughly three coding agents, start with:

```yaml
max-concurrency: 8
request-retry: 1
max-body-bytes: 8388608
```

Use 256 MB RAM as the safer initial container limit. A 128 MB limit is
possible but aggressive and should only be used after a soak test with the
actual prompts, tools, and stream concurrency. Set `RUST_LOG=warn`, keep the
admin listener private, and put the public listener behind TLS and rate
limiting.

## Missing measurements

This baseline does not yet satisfy the final resource-acceptance contract. It
still needs allocator-level allocation/copy counts, immediate-stream raw
latency tables, disconnect storms, slow-client backpressure, upstream stalls,
and Linux/container measurements on the target server architecture. Those
must be repeated after full feature parity because otherwise the comparison
would favor the smaller, incomplete implementation.
