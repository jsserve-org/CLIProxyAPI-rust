# CLIProxyAPI Rust core

This repository is a resource-conscious Rust port of the Codex hot path from
[CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI). It reads existing
`type: "codex"` JSON auth files and exposes a small, deliberately bounded
compatibility surface for Codex clients and CPA Manager Plus (CPAMP).

## Current compatibility

Implemented:

- `GET /v1/models`
- `POST /v1/responses` and `/v1/responses/compact`
- Streaming `POST /v1/messages` translation for Claude Code text, images,
  tool calls/results, and basic reasoning settings
- `/backend-api/codex/{responses,responses/compact,alpha/search}`
- Round-robin Codex account selection and bounded retry/failover
- On-demand refresh of existing Codex OAuth refresh tokens after an upstream 401
- Streaming upstream responses without buffering them in memory
- CPAMP essentials: config validation, auth-file list/upload/download/delete,
  enable/disable, reload, usage queue, and allowlisted `api-call`
- Existing CLIProxyAPI Codex auth-file shape and plaintext or bcrypt management keys

Not yet a one-to-one replacement:

- Non-streaming Claude messages, token counting, and the full set of Anthropic
  beta/content-block extensions
- Interactive OAuth login/device authorization (existing refresh tokens are supported)
- Chat Completions, Gemini, Anthropic, realtime, image/video, plugins, and Home
- Full usage accounting and the remaining CLIProxyAPI management routes

Unknown management paths return `501 Not Implemented`. Do not replace a full
CLIProxyAPI deployment until the endpoints used by your clients are covered by
contract tests.

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
