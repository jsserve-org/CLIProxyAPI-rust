# Security policy

Please report vulnerabilities privately to the repository owner. Do not include
access tokens, management keys, auth files, prompts, or customer data in an
issue.

The supported security baseline is the latest release built from a locked
`Cargo.lock`. Public deployments must keep the admin listener private and put a
TLS-terminating, rate-limiting reverse proxy in front of the public listener.

Security properties covered by tests include path-traversal rejection for auth
uploads and an exact-host allowlist for privileged outbound management calls.
OAuth refresh and protocol translation are not implemented yet and are outside
the current support boundary.
