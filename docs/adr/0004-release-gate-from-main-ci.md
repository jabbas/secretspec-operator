# ADR 0004: Release gate from main CI

- **Status:** Accepted
- **Date:** 2026-10-26
- **Decided by:** the project owner

## Context

- A release took about 14 minutes as one serial chain.
- The `verify` job (about 5 minutes) re-ran the whole gate (fmt, clippy, tests, `test-slow`, crdgen, chart-test, deny, audit) on a commit that CI had already passed as a push to `main`. Its rust-cache never hit, so everything compiled cold.
- It also saved a 460 MB cache under the tag ref. A cache written under a tag ref can only be restored by that same tag, so nothing could ever use it. The repository's Actions cache was at the 10 GB cap, and 3.4 GB of it were such tag-scoped entries.
- The `build` job (about 5 minutes) compiled the two musl targets one after the other, with caching deliberately off because the output is signed.

## Decision

- `verify` no longer re-runs the gate. It keeps the tag format, version (tag, workspace, chart) and "reachable from `main`" checks, and requires that the tagged commit has a successful `ci.yml` run from a push to `main` (`.github/scripts/ci-status.sh`, using the job's `GITHUB_TOKEN` with `actions: read`). A run still in progress is waited for (up to 30 minutes); a failed or missing run, or any API problem, fails the job.
- `verify` re-checks the RUSTSEC advisories at release time (`cargo deny check advisories`, `cargo audit`, the same pinned prebuilt binaries as CI), because CI may be days older than the tag.
- `build` is a matrix of two parallel legs (amd64, arm64), each with its own artifact. The licence bundle is generated once, in a separate `licenses` job; `image` downloads all three artifacts.
- The release workflow uses no caches at all (Rust, QEMU image, buildx binary). `ci.yml` keeps its caches but saves them only from `main`.

## Alternatives considered

- **Keep re-running the gate in `verify`.** Rejected: it duplicates a gate that already passed for the same commit, and costs about 5 minutes per release.
- **Restore a warm cache from `main` into the tag run and save nothing.** Rejected: it would shorten the re-run, but the gate would still run twice, and a cache restored into a release job is more state to trust in a signed pipeline. The build legs would not be allowed to use it either.

## Consequences

- The release procedure is: merge to `main`, wait for the CI run of that push to go green, then tag. A tag on a commit without a successful push run fails `verify`.
- A re-run of a failed CI run counts: success is judged on the latest attempt of each run. If that re-run fails, the gate fails.
- Licences, bans and sources (`cargo deny` beyond advisories) and the test suite are checked in CI only; the release trusts the CI result for the same commit SHA.
- A release is about 4 to 5 minutes shorter at the `verify` step and the two builds overlap. Cache quota is no longer consumed by tag runs; the stale tag-scoped entries must be removed once by hand.
- The pipeline depends on the GitHub Actions API: an outage or rate limit makes `verify` fail (after a few retries) instead of passing; re-run the job.
