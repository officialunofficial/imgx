# Security Policy

## Supported Versions

`imgx` is deployed as a single rolling Docker image (`ghcr.io/officialunofficial/imgx`); only the latest release is supported. There is no LTS branch.

## Reporting a Vulnerability

Please report security vulnerabilities privately via [GitHub Security Advisories](https://github.com/officialunofficial/imgx/security/advisories/new) rather than a public issue.

Include:
- A description of the vulnerability and its potential impact
- Steps to reproduce (a minimal request/config is ideal)
- Affected version/commit

We'll acknowledge reports within a few business days. `imgx` processes untrusted, remotely-fetched image data through libvips — memory-safety issues at that FFI boundary (`crates/imgx-vips`) are taken especially seriously; see `docs/INVARIANTS.md` for the documented safety invariants that boundary must uphold.
