# Why this port? 

Mainly for fun, since I have some usage left on my GPT subs. I decided to rewrite CLIProxyAPI to rust. However, I did realize that the Go version of CLIProxyAPI uses a lot of CPU and memory on my tiny VM, so this port also fixes that too, plus a few extra features that I want!

# CLIProxyAPI Rust core

This repository is a resource-conscious Rust port of the Codex hot path from
[CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI). It reads existing
`type: "codex"` JSON auth files and exposes a small, deliberately bounded
compatibility surface for Codex clients and CPA Manager Plus (CPAMP).

## Performance at a glance

Against pinned CLIProxyAPI Go commit `09a29bd` on an Apple M4/macOS arm64,
using optimized native binaries and the same deterministic local upstream:

| Measurement | Original Go | This Rust port | Difference |
| --- | ---: | ---: | ---: |
| Binary size | 57 MB | 9.2 MB | 84% smaller |
| Cold idle memory (RSS) | about 32 MB | about 3 MB | about 91% lower |
| Extended observed-load memory | about 96 MB | about 64 MB | about 33% lower |
| CPU, 64 delayed concurrent streams | 1.20 s | 0.43 s | about 64% lower |
| Warm token count, 64 concurrent requests | 16,032 req/s | 69,507 req/s | 4.3x faster |

Immediate loopback streams measured between 1.7x and 3.5x faster in Rust,
depending on concurrency. When every upstream event was delayed by 2 ms,
throughput was almost identical because upstream latency became the limit, but
Rust still consumed substantially less proxy CPU.

These are provisional results for the currently implemented Codex core, not a
claim that an incomplete port beats the full Go application. They will be
rerun after the parity contract passes. See
[`docs/BENCHMARKS.md`](docs/BENCHMARKS.md) for methodology, concurrency tables,
limitations, and constrained-server recommendations.

## Current compatibility

Implemented:

- `GET /v1/models`
- `POST /v1/responses` and `/v1/responses/compact`
- Streaming and non-streaming `POST /v1/chat/completions` translation to the Codex
  backend, including multimodal content, function/custom tool calls, tool-name
  shortening/restoration, structured outputs, reasoning summaries, generated images,
  and usage accounting; legacy `POST /v1/completions` is adapted on top
- Streaming and non-streaming `POST /v1/messages` translation for Claude Code text, images,
  tool calls/results, and basic reasoning settings
- Local `POST /v1/messages/count_tokens` using the same O200k input-segment
  policy as the pinned Go Codex executor
- `/backend-api/codex/{responses,responses/compact,alpha/search}`
- Round-robin, smooth weighted round-robin, or fill-first Codex account
  selection and bounded retry/failover
- Opt-in bounded session affinity for explicit Claude/Codex/client session
  signals and stable initial-message fallback, with automatic failover release
- On-demand refresh of existing Codex OAuth refresh tokens after an upstream 401
- Streaming upstream responses without buffering them in memory
- CPAMP essentials: config validation, auth-file list/upload/download/delete,
  enable/disable, reload, usage queue, and allowlisted `api-call`
- Existing CLIProxyAPI Codex auth-file shape and plaintext or bcrypt management keys

Not yet a one-to-one replacement. The pinned upstream revision, completion
criteria, subsystem status, and porting order are tracked in
[`docs/PARITY.md`](docs/PARITY.md):

- The full set of Anthropic beta/content-block extensions
- Interactive OAuth login/device authorization (existing refresh tokens are supported)
- Gemini, Anthropic, realtime, image/video, plugins, and Home
- Full usage accounting and the remaining CLIProxyAPI management routes
- The complete upstream hierarchical/LCP session-affinity behavior

Unknown management paths return `501 Not Implemented`. Public unknown paths
and unsupported methods always return an empty `404`. Do not replace a full
CLIProxyAPI deployment until the parity contract passes.

## Two-port security model

| Listener | Default | Routes | Intended exposure |
| --- | --- | --- | --- |
| Public | `0.0.0.0:8317` | API + health only | Reverse proxy / internet |
| Admin | `127.0.0.1:8318` | API + `/v0/management/*` | Loopback or private VPN |

The public router does not register management handlers. This is stronger than
checking a permission flag after a request reaches the handler. The admin port
still requires the management key for every management request.

Copy `config.example.yaml` to the ignored `config.yaml`, replace both example
keys, then run:

```sh
cargo build --release
./target/release/cliproxyapi-rs --config config.yaml
```

Point CPAMP at `http://127.0.0.1:8318`; point API clients at port `8317`.

Private multi-architecture images are published for `linux/amd64` and
`linux/arm64` by GitHub Actions:

```sh
docker pull ghcr.io/jsserve-org/cliproxyapi-rust:latest
```

## Resource choices

- One Tokio process and one shared rustls connection pool
- Response streaming; request bodies are bounded at 16 MiB by default
- A global concurrency ceiling (64 by default)
- No request-body logging, metrics aggregation, file watcher, embedded UI, or
  background polling
- Thin LTO, stripped release binaries, and abort-on-panic

Measure under your real workload before claiming a particular saving. Network
TLS and JSON handling usually dominate proxy CPU; the best additional language
for an even smaller binary/runtime is Zig, followed by C. Rust is the safer
choice for an internet-facing service because it retains memory safety while
remaining compact. Go is simpler but its garbage collector and per-connection
runtime overhead are exactly what this port is intended to reduce.

The measured baseline, methodology, limitations, and suggested
restricted-server settings are recorded in
[`docs/BENCHMARKS.md`](docs/BENCHMARKS.md).

## Internet deployment checklist

- Keep the admin listener on loopback/WireGuard/Tailscale; never publish port 8318.
- Terminate TLS at Caddy, nginx, or Cloudflare and add per-key/IP rate limits.
- Store `config.yaml` and the auth directory outside the image with mode `0600`.
- Use long, independent random values for API and management keys.
- Keep `management-allowed-hosts` narrow. The `api-call` feature is otherwise an
  SSRF primitive by design.
- Run as an unprivileged user with a read-only root filesystem and dropped
  capabilities; the included Compose file does this.
- Pin image digests and run dependency/container scanning before production.

No implementation can promise “no exploits,” and no proxy can guarantee that a
provider will not suspend an account. Use only accounts and access methods whose
terms and authorization you have verified. A social-media post is not a durable
policy exception.

## Attribution

The route and file compatibility behavior was studied from CLIProxyAPI and is
distributed under its MIT license. The retained upstream copyright notices are
in [LICENSE](LICENSE).
