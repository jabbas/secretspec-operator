# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Until the API leaves
`v1alpha1`, a breaking change is always listed under its release.

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

### Changed

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
