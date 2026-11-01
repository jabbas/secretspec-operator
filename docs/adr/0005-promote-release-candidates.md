# ADR 0005: Promote release candidates

- **Status:** Accepted
- **Date:** 2026-11-01
- **Decided by:** the project owner

## Context

- A final tag `X.Y.Z` rebuilt the image from scratch on the same commit as the tested `X.Y.Z-rc.N`. The build is not reproducible, so the digests differ: `0.1.3` is `sha256:da975fd7…`, while the home-lab-tested `0.1.3-rc.2` is `sha256:dd4bd53b…`. The artifact that was tested is not the artifact that ships.
- A final release repeated all of the work of the release candidate: two builds, the licence bundle, the image push, the smoke test. It took about 8 minutes (excluding the CI wait) for an image that already existed.

## Decision

A final release **promotes the release candidate's image**: the same index digest, signed and attested again with the identity of the final tag, never rebuilt.

- **Mode selection.** `verify` runs `.github/scripts/release-mode.sh`.
  - A release candidate tag (`X.Y.Z-rc.N`) is `mode=build`: the existing pipeline.
  - A final tag looks for release candidate tags `X.Y.Z-rc.N` that point at the tagged commit (the tag is peeled to its commit) and takes the highest `N`, numerically. Found: `mode=promote` from that candidate.
  - No such candidate: the final tag fails, unless its annotation has a line `rebuild: <reason>`. Then it is `mode=build` (the emergency path), and the reason is shown as a warning and in the release notes.
  - The `rebuild:` line is an explicit choice, never an automatic fallback. It wins even when a release candidate exists.
  - The tag, its annotation and the candidates are read through the GitHub API, not from the checkout (which can lose annotated-tag data). Every lookup is fail-closed: an API error or an unexpected answer fails the job and is never taken as "no candidate".
- **`promote` job** (replaces `build`, `licenses`, `image` and `smoke` for finals). Fail-closed, in this order:
  1. resolve the digest of the candidate's tag;
  2. `cosign verify` it with the candidate's identity (`release.yml@refs/tags/<rc tag>`): the bytes come from this workflow at that tag;
  3. assert exactly the platforms linux/amd64 and linux/arm64;
  4. assert the image labels: `revision` is the tagged commit and `version` is `X.Y.Z`;
  5. scan both platforms again with grype (`--fail-on high --only-fixed`); the vulnerability database is newer than at candidate time, and a finding means "fix it and cut a new candidate";
  6. generate the SBOMs with syft (still at least 100 `pkg:cargo` entries) and attest them on the per-platform digests with the final identity;
  7. sign the index and the per-platform digests with the final identity;
  8. tag `X.Y.Z` with the candidate's digest, with the existing guard (an existing tag must already be this digest) and assert the tag resolves to exactly that digest;
  9. verify by tag with the final identity (and that the candidate's signature is still on the digest).
  There is no smoke test: the identical digest was smoke-tested in the candidate's run and in the home lab.
- **Image labels.** All builds, release candidates included, set `org.opencontainers.image.version` to the base version `X.Y.Z`, so a promoted image is consistent. The binary already reports the crate version without the candidate suffix. The candidate identity is in the tag and the signatures; `revision` stays the commit SHA.
- **Chart.** The chart is packaged again from the tagged commit with `--version X.Y.Z --app-version X.Y.Z`, then asserted to equal the candidate's chart (pulled and verified with the candidate's identity) except for the version: the same files, byte for byte, and `Chart.yaml` differing only in `version` and `appVersion` (`.github/scripts/chart-compare.sh`). It is then pushed, signed and verified as before; `helm-index` is unchanged.
- **Release.** The GitHub Release takes the SBOMs from `promote` or `image`, depending on the mode, and says "Image promoted from `<rc tag>` without rebuilding (digest `sha256:…`)" or, for an emergency rebuild, "Rebuilt without promoting a release candidate: <reason>".

## Alternatives considered

- **Reproducible builds.** Rejected: not achieved, and a second build, even if bit-identical, would still be a different build to trust. Promotion makes "tested equals shipped" true by construction.
- **Fall back to a build when there is no candidate.** Rejected: it would silently ship an artifact that nobody tested. The emergency path is a deliberate act that is visible in the tag.
- **A `workflow_dispatch` input for emergency rebuilds.** Rejected: the trigger would not be recorded in the immutable tag, and the final tag would not say how its image was made.

## Consequences

- A promoted image carries both signatures and attestations: the candidate's (`…@refs/tags/X.Y.Z-rc.N`) and the final one (`…@refs/tags/X.Y.Z`). Both identities verify against the same digest.
- A final release needs a release candidate on the same commit; cutting a final tag without one fails `verify` unless the tag message has a `rebuild:` line.
- A fresh scan at promotion can block a final: a newly published fixable high or critical vulnerability needs a fix and a new candidate (or an explicit `rebuild:` release after the fix).
- Images carry `X.Y.Z` as their version label, also release candidates. A candidate built before this change is labelled with its own tag and cannot be promoted (the label check fails); the first promotable candidate is the first one built with this workflow.
- The chart is re-packaged, not promoted: its package is not byte-reproducible and carries the final version. The comparison asserts that nothing else differs.
- A final release is much shorter: no builds, no QEMU, no smoke test.
- Releases 0.1.0 to 0.1.3 were rebuilt, so their final images differ from the candidates that were tested.
