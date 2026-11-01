# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Until the API leaves
`v1alpha1`, a breaking change is always listed under its release.

## Unreleased

### Fixed

- The HTTP Helm repository index is updated again for every release (it was
  skipped since the release-candidate promotion change). `0.1.4-rc.1` is
  therefore missing from the index; that is fine, it is superseded by
  `0.1.4-rc.2`. A new check (`release-graph-check.sh`, in the `release-scripts`
  CI job) fails a pull request when a release job after a mode-dependent job has
  no explicit `if:`.

## 0.1.4 - 2026-11-01

### Changed

- **A final release publishes the release candidate's image unchanged.** Until
  now a final tag `X.Y.Z` rebuilt the image, so the digest differed from the
  tested `X.Y.Z-rc.N` (0.1.3 is `da975fd7…`, the home-lab-tested 0.1.3-rc.2 is
  `dd4bd53b…`). Now the release workflow promotes the candidate that is tagged
  on the same commit: the same digest, signed and attested again with the
  identity of the final tag, after the candidate's own signature, platforms and
  labels are verified and the image is scanned again. The chart is packaged
  again and checked to equal the candidate's chart except for its version. A
  final tag without a candidate fails, unless its message has a
  `rebuild: <reason>` line (emergency release). See
  [ADR 0005](docs/adr/0005-promote-release-candidates.md) and
  [Development](docs/development.md#tag).
- **Images are labelled with the base version.** `org.opencontainers.image.version`
  is `X.Y.Z` for release candidates too (it was `X.Y.Z-rc.N`), so a promoted
  image is consistent.
- **Renovate tracks the release and CI tool pins** (cosign, syft, grype, crane,
  yq, helm, kind and its node images, the cargo tools, ziglang, BuildKit). A
  version bump without its pinned SHA-256 now fails in the pull request (the
  new `release-pins` CI job), with an error that names the variable to update.
  Renovate never proposes a runner image change or a cosign major.
- **Release error messages escape registry data.** Values read from the
  registry or the GitHub API (image labels and annotations, tag digests, helm
  output) can no longer inject a workflow command into the run log.
- **The licence bundle tool no longer panics on a malformed SPDX expression.**
  `cargo xtask licenses` reports a parse error for an expression that ends in
  `WITH` instead of crashing.
- Documentation fixes: the lease RBAC in the design (§9), the 0.1.3 home-lab
  report.

## 0.1.3 - 2026-11-01

### Fixed

- **No warnings or retries when a namespace containing SecretSpecs is deleted.**
  While a namespace is terminating, Kubernetes refuses to create the Secret
  (`403 Forbidden`, cause `NamespaceTerminating`). The operator used to log
  about 15 `WARN` lines per namespace (apply failed, "reconcile failed;
  retrying", event publishing failed), write an `ApplyFailed` status and retry
  every 30 s. It now recognises that cause, logs once at `debug`, writes no
  status, publishes no event and does not retry. Every other `403` (for example
  missing RBAC) is reported as `ApplyFailed` as before.
- **The CRD schema no longer uses the non-standard `uint16` format.** schemars
  derived `format: uint16` for the eleven `u16` properties (`length`, `bytes`
  and the `require` counts); Kubernetes ignores it, but Helm 4 printed
  `unrecognized format "uint16"` on every install. The `minimum`/`maximum`
  bounds are unchanged, so validation is unchanged.

### Changed

- **Docs:** [Operations](docs/operations.md#debugging-inside-the-pod) has a
  non-interactive `kubectl debug` example (`-i` plus a command), with the
  harmless `couldn't attach … falling back to streaming logs` message and the
  advice to pin the busybox image by digest.

## 0.1.2 - 2026-10-26

### Added

- **[ADR 0003](docs/adr/0003-scratch-base-image.md)** records the decision to
  build the image from scratch, and
  [Operations](docs/operations.md#debugging-inside-the-pod) documents debugging
  with an ephemeral container (`kubectl debug … --target=operator
  --profile=restricted`; the `restricted` profile is needed in namespaces that
  enforce the Pod Security Standard `restricted`).

### Changed

- **The image is now built `FROM scratch`** instead of
  `gcr.io/distroless/static-debian12:nonroot`: it contains only the static
  operator binary and `/licenses/`. Distroless stopped building its `debian12`
  images, so the digest-pinned base was frozen and unpatched. The binary needs
  nothing from a base image when run by the chart (in-cluster the API server
  CA comes from the mounted ServiceAccount, so no system CA bundle is needed;
  UTC only, no disk writes, numeric user `65532:65532`). Running the image
  against a cluster whose kubeconfig has no explicit CA is not supported, as
  there are no system CA roots. The SBOM and the vulnerability scan now cover
  only the crates compiled into the binary; there is no OS layer. There is
  still no shell: use `kubectl debug` for an ephemeral debug container. The
  `0.1.0` and `0.1.1` images keep the frozen distroless `debian12` base.
  Renovate no longer tracks the Containerfile.

## 0.1.1 - 2026-10-26

### Added

- **HTTP Helm repository.** The chart is also published to the classic Helm
  repository `https://jabbas.github.io/helm-charts` (`helm repo add jabbas
  https://jabbas.github.io/helm-charts`), next to the cosign-signed OCI chart.
  The index points at the release assets of this repository. It is not signed
  and neither Helm nor Flux verifies it; the release workflow asserts that each
  package is byte-identical to the signed OCI chart layer, and
  [Installation](docs/installation.md#what-is-and-is-not-verified) shows how to
  verify the digest chain. `0.1.0`, `0.1.0-rc.1` and `0.1.0-rc.2` are already
  listed.
- **Third-party licence texts in the image** at `/licenses/` (the project
  `LICENSE` and `NOTICE` plus the licence texts of the 243 crates compiled
  in), generated by `cargo xtask licenses`.
- **OCI source annotation** (`org.opencontainers.image.source`) on the image
  index, so registries link the image to this repository.

### Fixed

- **A `Conflict` is re-checked about every minute.** A foreign Secret or a
  type mismatch blocking the target name used to be noticed only at the next
  periodic resync, because the Secret watch sees only labelled Secrets. Status,
  events and fingerprints are unchanged.
- **The leader-election renewal retry wait is clamped to the lease tenure
  deadline.** After a failed renewal the keeper could wait up to a second past
  the deadline before noticing the loss of leadership.

### Changed

- **`ctutils` patched for RUSTSEC-2026-0385** (dependency update, no change in
  behaviour).
- **Release tags and image tags drop the `v` prefix from 0.1.1 on.** Releases
  are tagged `X.Y.Z` and `X.Y.Z-rc.N` (plain semver), the image is
  `ghcr.io/jabbas/secretspec-operator:X.Y.Z`, the signing identity is
  `…/release.yml@refs/tags/X.Y.Z`, and the chart's default image tag is its
  `appVersion` unchanged. `0.1.0` stays published under the legacy tag `v0.1.0`
  (image `:v0.1.0`).

## 0.1.0 - 2026-10-23

First release.

### Added

- **`SecretSpec` custom resource** (`secretspec.jabbas.eu/v1alpha1`, short name
  `ss`), validated at apply time by the CRD schema and CEL rules. One
  `SecretSpec` declares exactly one of:
  - `fields`: random strings (length, `alphanumeric` / `ascii` / `hex` /
    `custom` character sets, `exclude`, per-class `require`, `prefix`,
    `suffix`) and random bytes encoded as `hex`, `base64`, `base64url` or
    `base32`;
  - `sshKeyPair`: ed25519 (default), RSA (2048, 3072, 4096) or ECDSA (P-256,
    P-384, P-521) in OpenSSH format;
  - `basicAuth`: username, generated password and a bcrypt `htpasswd` line.
- **Stable values.** A value is generated once and changes only when its
  definition changes, when the Secret or a key is deleted, or when a configured
  rotation is due. Operator restarts and upgrades, identical re-applies, cache
  loss and leader changes never change a value. Defaults and the fingerprint
  format are frozen for `v1alpha1` and guarded by golden tests.
- **Per-key regeneration, `keys.*` renames that move values, literal `data`**
  copied verbatim, `secret.type` (including `kubernetes.io/basic-auth` and
  `kubernetes.io/ssh-auth`), `secret.labels` and `secret.annotations`.
- **Scheduled rotation** (`rotation.interval`, `1h` to `3650d`). Manual
  rotation by deleting the Secret or a key.
- **`deletionPolicy`:** `Delete` (default, owner reference) or `Retain`
  (no owner, no finalizer), with adoption of a retained Secret by a re-created
  `SecretSpec`.
- **Status and Events** for everything detectable only at runtime:
  `Ready` reasons `Generated`, `Conflict`, `InvalidSpec`, `ApplyFailed`,
  `GenerationFailed` and `WaitingForGarbageCollection`; Events `Generated`,
  `Regenerated`, `Rotated`, `Adopted`, `LowEntropy`, `FingerprintsReset`,
  `AnchorReset`, `PasswordTruncated` and the warnings for the reasons above.
- **Operator** written in Rust: server-side apply, metadata-only watch of
  managed Secrets, lease-based leader election, Prometheus metrics
  (`secretspec_reconcile_total`, `secretspec_reconcile_duration_seconds`,
  `secretspec_generations_total`, `secretspec_secretspecs`,
  `secretspec_next_rotation_timestamp_seconds`), `/healthz` and `/readyz`,
  structured JSON logs that never contain values.
- **Helm chart** `oci://ghcr.io/jabbas/charts/secretspec-operator`: CRD rendered
  as a template (upgraded with the chart, kept on uninstall by default),
  cluster mode (ClusterRole) and namespace mode (a Role per namespace), registry
  mirror, pull secrets and digest pinning, `ServiceMonitor`, Pod Security
  `restricted`, values schema.
- **Image** `ghcr.io/jabbas/secretspec-operator` for `linux/amd64` and
  `linux/arm64`, a static binary on a distroless image. Version tags only, no
  `latest`.
- **Release signing:** cosign keyless signatures for the image and the chart
  and a signed SPDX SBOM attestation per platform image. The SBOM lists the
  Rust crates (the binary embeds its dependency list) and the base image
  packages.
- **Documentation and seven examples.**

### Compatibility

- Kubernetes 1.34 and newer.
- Licence: Apache-2.0.
