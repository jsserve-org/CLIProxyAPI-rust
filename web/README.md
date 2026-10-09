# Operator Console

A Vite + React management portal for the private admin listener. It uses the existing `/v0/management/*` API and sends the `x-management-key` header; the Rust API surface remains available for custom clients.

```sh
bun install
bun run dev
```

Build with `bun run build` and serve `dist/` from the admin listener or your preferred static host.
