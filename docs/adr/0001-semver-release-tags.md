# ADR 0001: Semver release tags without the `v` prefix

- **Status:** Accepted
- **Date:** 2026-10-24
- **Decided by:** the project owner

## Context

- `0.1.0` was released with `v`-prefixed tags: git `v0.1.0`, `v0.1.0-rc.1` and `v0.1.0-rc.2`, and the image `ghcr.io/jabbas/secretspec-operator:v0.1.0`.
- The chart version and `appVersion` were already plain `0.1.0`, so the chart had to prepend `v` to `appVersion` to build its default image tag.
- semver.org states that "v1.2.3" is not a semantic version.
- Tags are immutable in this project: they are never moved, deleted or recreated. The cosign keyless signing identity embeds the tag (`https://github.com/jabbas/secretspec-operator/.github/workflows/release.yml@refs/tags/<tag>`).

## Decision

- From the next release (0.1.1), git tags and image tags are exactly the semantic version: `X.Y.Z` and `X.Y.Z-rc.N`.
- The chart version, `appVersion` and image tag are identical.
- `v0.1.0` and its two release candidates stay as they are, as the only legacy `v`-prefixed release. They get no alias tags.

## Alternatives considered

- **Retagging 0.1.0.** Rejected: it would delete published tags, break existing pins and break the signature identity.
- **An alias image tag `0.1.0` pointing at the same digest.** Rejected: `cosign verify` for it would need the `refs/tags/v0.1.0` identity, which contradicts the documented command.
- **Keeping the `v` prefix.** Rejected by the owner.

## Consequences

- The documented verify commands use `refs/tags/<X.Y.Z>`, and the docs carry a one-line legacy note for 0.1.0.
- The release workflow triggers only on unprefixed tags.
- The release-notes lookup treats both `vX.Y.Z` and `X.Y.Z` as final releases, so the 0.1.1 notes start at `v0.1.0`.
- Tag protection rules (when added) must cover both patterns.
- Until the next version bump, the source-tree chart on `main` renders `:<current appVersion>` without a `v`; for 0.1.0 that image tag does not exist (0.1.0 is published only as `:v0.1.0`). This is harmless: installs come from the OCI chart, and the e2e suite overrides the image.
