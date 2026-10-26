# ADR 0003: Build the image from scratch

- **Status:** Accepted
- **Date:** 2026-10-26
- **Decided by:** the project owner

## Context

- The 0.1.0 and 0.1.1 images were built on `gcr.io/distroless/static-debian12:nonroot`, pinned by digest and tracked by Renovate.
- The distroless project stopped building all `*-debian12` images on 2026-08-11 (GoogleContainerTools/distroless PR #2156; its support policy listed the static-debian12 EOL as September 2026). The pinned base is therefore frozen and unpatched.
- Renovate tracks the digest of a tag, so it cannot signal an EOL: the tag simply stopped moving.
- The operator binary is statically linked (musl) and takes nothing from the base image:
  - the kube client uses the ServiceAccount CA, and rustls brings its own embedded roots;
  - time is UTC only;
  - it writes nothing (the root filesystem is read-only);
  - it runs as the numeric UID/GID 65532.

## Decision

- From 0.1.2 the image is `FROM scratch`. It contains only `/secretspec-operator` and `/licenses/` (LICENSE, NOTICE and the third-party licence texts), with `USER 65532:65532`.
- Design §10.2 already allowed "distroless static nonroot (or `scratch`)"; this ADR settles that choice.

## Alternatives considered

- **`gcr.io/distroless/static-debian13:nonroot`.** A one-line change and still maintained. Rejected: the same EOL event recurs with every Debian release, and the image ships OS packages the binary never uses.
- **`cgr.dev/chainguard/static`.** Rejected: the free tier offers only `latest`, which must be pinned by digest and bumped, and the `tzdata` contents change on 2026-12-07.
- **Keeping debian12.** Rejected: the base is unpatched.

## Consequences

- There is no base image to maintain or scan. Grype and syft see only the binary's cargo packages (through cargo-auditable). The release still fails if an SBOM lists fewer than 100 cargo packages.
- There is no shell in the image. Debugging uses ephemeral containers (`kubectl debug --profile=restricted`).
- Renovate's dockerfile manager is removed; nothing in the Containerfile is left to track.
- The 0.1.0 and 0.1.1 images keep the frozen debian12 base, and their SBOMs list Debian packages.
- Verified before merging, with the scratch image on kind 1.37.0: e2e 6/6 and operator integration 12/12. The image filesystem contains only the binary and `/licenses/`.
