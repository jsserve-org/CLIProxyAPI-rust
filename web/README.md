# Operator Console

The management UI for the private admin listener, served at `/console`. It is a
Vite + React app built on [shadcn/ui](https://ui.shadcn.com) and covers every
`/v0/management/*` route the Rust core implements. It only talks to that API
(with the `x-management-key` header), so custom clients keep working unchanged.

```sh
bun install
bun run dev    # proxies /v0/management to the admin listener on 127.0.0.1:8318
bun run build  # writes dist/, which the Rust binary embeds at compile time
```

Commit `dist/` after building: `src/portal.rs` includes it with `include_str!`.
Add components with `bunx shadcn@latest add <name>`.
