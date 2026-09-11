# Upstream parity contract

The compatibility target is CLIProxyAPI at commit
[`09a29bd345bc44c473abe7fd07859e32df2ea543`](https://github.com/router-for-me/CLIProxyAPI/commit/09a29bd345bc44c473abe7fd07859e32df2ea543).
That revision is the behavioral specification for the Rust port. Moving this
pin requires reviewing upstream changes and updating this document and the
contract fixtures in the same pull request.

The Rust port is **not feature-complete yet**. A route existing is not enough
to mark a row complete: accepted inputs, status codes, response headers and
bodies, streaming event order, retries, credential selection, and persisted
state must agree with the pinned Go implementation.

## Definition of parity

A subsystem is complete only when all of the following are true:

1. Every route and supported method in the pinned implementation exists on the
   admin listener, except that management routes are intentionally absent from
   the public listener.
2. Differential tests send the same fixtures to Go and Rust and compare the
   normalized response and upstream request.
3. Upstream Go tests relevant to the subsystem have equivalent Rust coverage.
4. Streaming and realtime tests cover disconnects, backpressure, malformed
   frames, cancellation, and maximum configured sizes.
5. Security tests cover authentication, path traversal, SSRF, secret leakage,
   request smuggling boundaries, and resource exhaustion.
6. Release benchmarks record idle RSS and CPU plus sustained and burst traffic
   on both implementations. Regressions require an explicit explanation.

No image or release may be described as a drop-in replacement before every
row below is complete and the full parity suite passes.

## Compatibility matrix

| Area | Upstream behavior in scope | Rust status |
| --- | --- | --- |
| Process and configuration | CLI flags, YAML schema, environment behavior, reloads, file watching, SDK configuration | Partial |
| Listener separation | Public API-only listener; private API + management listener | Implemented extension; needs deployment tests |
| OpenAI models | Unified and provider-aware model listing | Partial; static Codex list only |
| Responses | HTTP streaming/non-streaming, compact, WebSocket, Codex direct aliases, Alpha Search | Partial; HTTP forwarding and compact/Alpha aliases |
| Chat/completions | OpenAI chat completions and legacy completions | Missing |
| Anthropic Messages | Streaming, non-streaming, token counting, beta blocks, thinking/signatures, tool behavior | Partial; basic streaming/non-streaming translation and local Codex token counting |
| Gemini | Models, generate/stream content, interactions and compatible actions | Missing |
| Realtime | WebSocket, WebRTC/SIP calls, sideband control, sessions, transcription and translation | Missing |
| Images and video | OpenAI-compatible images plus xAI/OpenAI video create/edit/extend/retrieve/content | Missing |
| Providers | Codex, Claude, Gemini/AI Studio, Antigravity, Vertex, Kimi, xAI, OpenAI-compatible backends | Partial; Codex OAuth files only |
| OAuth | Provider login flows, callbacks, sessions, refresh, cancellation, relogin | Partial; existing Codex refresh tokens only |
| Credential routing | Round-robin/fill-first, weights, model aliases, exclusions, session affinity, cooldowns and bounded retries | Partial; three core strategies, bounded retry and bounded basic session affinity |
| Usage and quota | Per-key/provider accounting, usage queue, quota refresh/reset, cooldown state | Missing; empty compatibility response only |
| Management API | Full built-in route set and exact CPAMP behavior | Partial; core auth-file/config/API-call routes |
| Logging | Request/error logs, rotation, lookup/download and redaction | Missing |
| Plugins | Auth, executor, model router, management/resource routes, store lifecycle and native plugin ABI | Missing |
| Home | Home protocol, assets, model capabilities and Home-specific management behavior | Missing |
| Storage | Filesystem plus configured Git/SQL/object-store behavior | Partial; local auth directory only |
| Model registry | Dynamic definitions, aliases, capabilities and provider availability | Missing |

At the pinned revision, the surface includes roughly 29 primary model/API
routes and 128 built-in management route registrations. Dynamic plugin routes
increase that surface. Counts are useful drift alarms, not evidence of parity.

## Porting order

Work proceeds in dependency order while keeping each merged slice deployable:

1. Claude Code and Codex request/response translators, including non-streaming,
   token counting, tool/thinking semantics, session affinity and cooldowns.
2. Exact credential routing, retry, refresh, quota and usage behavior.
3. CPAMP's complete management and OAuth surface.
4. OpenAI chat/completions and the remaining Responses transports.
5. Gemini and all remaining providers.
6. Realtime, images/video, plugins, Home, logging and alternate stores.
7. Full differential, soak, fuzz, security and resource testing.

## Resource acceptance

Benchmarks must use identical credentials replaced by a deterministic mock
upstream and identical request traces. Record at minimum:

- idle and peak resident memory;
- CPU time and requests/second at 1, 3, 10 and 64 concurrent agent streams;
- p50/p95/p99 time-to-first-byte and end-to-end latency;
- allocations and bytes copied per request;
- behavior with slow clients, upstream stalls and disconnect storms.

The optimized implementation must remain bounded by configured request size,
concurrency, retry count, connection-pool size and streaming-frame size. A
lower idle RSS alone does not satisfy the goal.
