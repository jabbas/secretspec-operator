# ADR 0002: HTTP Helm repository channel

- **Status:** Accepted
- **Date:** 2026-10-25
- **Decided by:** the project owner

## Context

- Charts were published only as OCI artifacts, `oci://ghcr.io/jabbas/charts/secretspec-operator`, signed with cosign. Flux can verify them through the `verify` field of an `OCIRepository`.
- The owner prefers the classic `HelmRepository` + `chart.spec` in Flux and does not want to use `OCIRepository`.
- Flux and Helm do not verify signatures for HTTP Helm repositories.

## Decision

- Add a second channel: a shared HTTP Helm repository for all of the owner's charts at `https://jabbas.github.io/helm-charts`. It is the GitHub repository `jabbas/helm-charts`, served by GitHub Pages, and holds only `index.yaml`.
- The chart `.tgz` files stay as GitHub Release assets of this repository. The index URLs point at `https://github.com/jabbas/secretspec-operator/releases/download/<tag>/`.
- The `helm-index` job in `release.yml` updates the index after every release, release candidates included. It authenticates with a fine-grained personal access token (secret `HELM_CHARTS_TOKEN`) that has Contents read/write on `jabbas/helm-charts` only. The token is stored in the Environment `helm-charts`, whose deployment policy allows only the release tags.
- The OCI channel stays, and remains the signature-verified one.
- There is no separate signature for the `.tgz`. The release package is byte-identical to the layer of the signed OCI chart, and the `helm-index` job asserts this before it touches the index. Users verify the OCI signature and compare digests (recipe in [`docs/installation.md`](../installation.md#what-is-and-is-not-verified)).

## Alternatives considered

- **The index in this repository's own GitHub Pages.** Rejected: the owner wants one shared index for all of the charts.
- **A GitHub App instead of a token.** It does not expire, but it takes more setup for a single target repository.
- **A scheduled pull from `jabbas/helm-charts`.** It needs no secret, but the index lags behind the release, and GitHub disables scheduled workflows after 60 days of repository inactivity.
- **`cosign sign-blob` on the `.tgz`.** Unnecessary, given the byte identity with the signed OCI layer. The byte identity also covers 0.1.0, which predates this decision.
- **Replacing OCI with the HTTP repository.** Rejected: it would remove the only channel that Flux verifies.

## Consequences

- The token must be renewed before it expires. An expired or revoked token makes `helm-index` fail.
- A failed `helm-index` job does not invalidate a release. The job is idempotent (the same version with the same digest is a no-op, a different digest fails) and can be re-run on its own.
- Users of the HTTP channel get no automatic signature check. Whoever can change `jabbas/helm-charts` or its Pages site can re-point an index entry; the digest comparison with the signed OCI chart is manual.
- The index entries for the legacy 0.1.0, 0.1.0-rc.1 and 0.1.0-rc.2 were backfilled from the existing release assets, without touching any tag. Their URLs use the legacy `v0.1.0*` tags (ADR 0001).
- Prereleases are listed in the index. Helm hides them unless `--devel` or a semver range that includes prereleases is used.
