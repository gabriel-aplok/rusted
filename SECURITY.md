# Security Policy

## Supported versions

Only the latest 0.1.x source tree receives security fixes. The project is pre-1.0, so interfaces and internals can change.

## Reporting a vulnerability

Do not open a public issue for a security problem.

Use the private vulnerability reporting flow on the GitHub repository. If that is unavailable, contact the maintainer listed on the repository profile.

Include:

- The affected command or module.
- A minimal reproduction, ideally a fixture or manifest.
- The expected impact.
- Any known workaround.

You should get an initial response within a few days. Confirmed issues are fixed on `main` and noted in `CHANGELOG.md` without exploit details.

## Scope

The tool downloads crates, reads manifests, writes caches, and can upload artifacts to S3-compatible storage. Relevant issues include:

- Path traversal or unsafe extraction from downloaded archives.
- Checksum or signature bypass on crate downloads or remote cache fetches.
- Command injection through manifest fields, environment variables, or build scripts.
- Credential leakage from remote cache configuration.
- Cache poisoning that lets an unverified artifact satisfy a build.

Build scripts and proc macros execute arbitrary code by design. Treat any dependency you build with this tool as trusted code.
