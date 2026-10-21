# Security

This document describes what the operator protects, what it is allowed to do,
what it keeps in memory, and how to verify and report on it.

## Threat model

**Assets.** The generated values (passwords, tokens, keys), and the integrity of
the Secrets they live in.

**Roles.**

- *Platform team:* installs the operator, owns its RBAC and scope.
- *Application developers:* may create `SecretSpec`s in their namespaces, but do
  not necessarily have access to Secrets.
- *Everyone else in the cluster:* must not get values they were not already
  allowed to read.

**What the design defends against**

- **Accidental credential loss or rotation.** Values change only for the reasons
  in the [behaviour table](api-reference.md#behaviour-table) — never on a
  restart, upgrade, cache loss, leader change or re-apply of an identical
  manifest. A golden test pins the fingerprint format.
- **The operator being talked into touching a Secret it did not create.** It
  writes only Secrets that carry its `managed-by` and `spec-name` labels (or that
  do not exist). Any other Secret with the target name yields a `Conflict` and is
  left alone, never adopted.
- **Privilege escalation through the `SecretSpec` API.** Anyone who may create
  `SecretSpec`s can make the operator create Secrets in that namespace, so
  `secret.type` is limited to `Opaque`, `kubernetes.io/basic-auth`,
  `kubernetes.io/ssh-auth` and custom types outside `kubernetes.io/`; service
  account tokens, bootstrap tokens and other built-in types, whose contracts the
  operator does not implement, are rejected. Labels and annotations under
  `secretspec.jabbas.eu/` and `app.kubernetes.io/managed-by` cannot be set from a
  `SecretSpec`, so a spec cannot forge the markers the operator trusts.
- **Value leaks through observability.** See
  [Value redaction](#value-redaction-guarantees).
- **Weak randomness and bias.** Values come from the OS CSPRNG with rejection
  sampling; the operator warns (`LowEntropy`) below 128 bits.
- **Supply-chain tampering.** Signed multi-arch image and chart, an SBOM
  attestation, pinned CI actions, and `cargo-deny` / `cargo-audit` checks
  ([below](#release-signing-and-sbom)).

**What it does not defend against**

- **A compromised operator.** The pod can read the Secrets in its scope (below)
  and write Secrets there. Namespace mode limits that to the listed namespaces;
  the chart does not request more.
- **Anyone who can read Secrets or etcd.** Generated values are ordinary Secrets.
  Enable [encryption at rest](https://kubernetes.io/docs/tasks/administer-cluster/encrypt-data/)
  and restrict `get`/`list` on Secrets as you would for any Secret.
- **API-server audit logs** configured to record request or response bodies of
  Secrets: they would contain the values the operator writes and reads. Keep
  Secrets at `Metadata` level in your audit policy.
- **Confidentiality of `spec.data`.** Literals in `data` are stored in the
  `SecretSpec`, which is not a Secret: it appears in `kubectl get secretspec -o
  yaml`, in Helm release values and in your git repository. Put only
  non-confidential values (such as a user name) there.
- **Consumers.** What an application does with the value, and rotating it
  everywhere it was copied, is up to the application.

## RBAC

The chart grants exactly the following, and nothing more:

| API group | Resource | Verbs | Why |
|---|---|---|---|
| `secretspec.jabbas.eu` | `secretspecs` | `get`, `list`, `watch` | Read the specs. |
| `secretspec.jabbas.eu` | `secretspecs/status` | `get`, `patch`, `update` | Write conditions and times. |
| `secretspec.jabbas.eu` | `secretspecs/finalizers` | `update` | Needed on clusters with the `OwnerReferencesPermissionEnforcement` admission plugin (for example OpenShift) to set `blockOwnerDeletion` on the owner reference. |
| `""` | `secrets` | `get`, `list`, `watch`, `create`, `patch`, `update` | Read and write the generated Secrets. **No `delete`.** |
| `events.k8s.io` | `events` | `create`, `patch` | Publish Events. |
| `coordination.k8s.io` | `leases` | `get`, `update` (only the operator's own Lease, by `resourceNames`) | Leader election; a `Role` in the release namespace only. |
| `coordination.k8s.io` | `leases` | `create` | Creates the Lease on first start. Kubernetes RBAC cannot restrict `create` by name, so this is a separate rule. |

- **Cluster mode** (`watchNamespaces: []`): a `ClusterRole` and
  `ClusterRoleBinding`, so the permissions above apply in every namespace —
  including `get`/`list`/`watch` on all Secrets, because Kubernetes RBAC cannot
  be scoped by label.
- **Namespace mode** (`watchNamespaces: [a, b]`): a `Role` and `RoleBinding` in
  each listed namespace and no cluster-wide permission. This is a real boundary:
  the operator cannot see anything elsewhere. Choose it when a cluster-wide
  Secret reader is not acceptable. See
  [Installation](installation.md#scope-cluster-mode-and-namespace-mode).
- The operator cannot delete a Secret. Cleaning up after a `SecretSpec` is done
  by Kubernetes garbage collection (`Delete`) or not at all (`Retain`), and
  there are no finalizers.
- There are **no admission webhooks**, no cluster-scoped write permission, and no
  access to nodes, pods or ServiceAccounts.

The pod complies with the Pod Security Standard `restricted`: non-root,
read-only root file system, all capabilities dropped, no privilege escalation,
seccomp `RuntimeDefault`. The image is a static binary on a distroless image.

## What is cached

- **Secrets: metadata only.** The operator watches Secrets through the
  metadata-only API and restricts the watch on the server with the label selector
  `app.kubernetes.io/managed-by=secretspec-operator`. Secret *data* never enters
  the watch cache, and unmanaged Secrets are not even listed.
- **Secret data is read only during a reconcile.** To plan a reconcile the
  operator reads the **one** Secret that has the `SecretSpec`'s name with a
  direct `GET` (also when it turns out not to be managed, in which case it is only
  inspected for ownership and then left alone), keeps it in memory for the
  duration of that reconcile, and drops it. Values are held in a type that is zeroed on drop and redacts itself when
  formatted.
- **`SecretSpec`s are cached in full.** They contain definitions, not generated
  values.

## Value redaction guarantees

Generated values never appear in:

- **Logs.** Values are wrapped in a type whose `Debug` and `Display` print
  `<redacted>`. The Kubernetes client library's own logging, which can print
  response bodies, is forced off whatever `LOG_LEVEL` says
  ([details](operations.md#logging)), and Kubernetes errors are logged as HTTP
  code and reason only. An end-to-end test applies every example, runs the
  operator at `debug` level and fails if any generated value (raw or
  base64-encoded) shows up in its logs.
- **Events and status.** Messages name keys, never values.
- **Metrics.** Only counts, durations and the next rotation time.
- **Error messages.** `ApplyFailed` carries the HTTP code and reason, not the
  API server's message (which can echo submitted data); `GenerationFailed`
  carries a cause that contains no key material.
- **Annotations.** The fingerprints annotation is visible to anyone who can read
  the Secret's *metadata*, including people who cannot read its data. Fingerprints
  are hashes of the generation *parameters*, not of values. The one value-related
  fingerprint, the htpasswd slot, is an HMAC keyed with the stored htpasswd line,
  so it cannot be used to test password guesses without access to the Secret
  data (see [Fingerprint format](fingerprints.md)).

The only place a value is written is the Secret itself.

## Adoption

When a `SecretSpec` is created and a Secret with its name already exists, the
operator **adopts** it only if the Secret carries the operator's labels
(`app.kubernetes.io/managed-by: secretspec-operator` and a `spec-name` equal to
the `SecretSpec`'s name) and has no live controller owner. It then keeps every
value whose stored fingerprint matches, regenerates the rest, sets the owner
reference according to `deletionPolicy`, records the new `spec-uid` and emits an
`Adopted` Event. This is how a `Retain`ed credential survives an uninstall and a
reinstall. A Secret that is still owned by a *deleted* `SecretSpec` is not
adopted until garbage collection has removed that owner
(`WaitingForGarbageCollection`).

Adoption is decided by labels. Anyone who can create Secrets in a namespace could
craft a Secret with those labels and have a later `SecretSpec` of that name adopt
it (and, if its fingerprints do not match, overwrite its values). They already
have write access to Secrets in that namespace, so this is **not a privilege
escalation**; it is the reason for the guidance to treat write access to Secrets
as sensitive. A Secret *without* the labels is never adopted.

## Dependency advisories: the RSA ignore

`cargo audit` and `cargo deny` run in CI. One advisory is deliberately ignored:
`RUSTSEC-2023-0071`, a timing side channel in RSA private-key *operations*
(the "Marvin attack") for which the `rsa` crate has no patched release. The
operator uses RSA only to **generate** SSH keys (through the `ssh-key` crate,
`sshKeyPair.algorithm: rsa`); it never signs, decrypts or otherwise performs a
private-key operation with an RSA key, so the leaking operation is never
executed. The rationale is recorded next to the ignore in `.cargo/audit.toml` and
`deny.toml`, and is revisited when a fixed release exists.

## Release signing and SBOM

Every release (`vX.Y.Z` and `vX.Y.Z-rc.N`) is built by the GitHub Actions
workflow `.github/workflows/release.yml`, which:

- runs the full test and audit gate on the tagged commit, which must be on `main`
  and match the workspace version;
- builds the static binaries and the multi-arch image (`linux/amd64`,
  `linux/arm64`) and packages the chart;
- scans the pushed image with [grype](https://github.com/anchore/grype) and
  fails the release on any **fixable** vulnerability of severity high or
  critical (vulnerabilities for which no fixed version exists yet are ignored,
  so a release is not blocked by something nobody can fix);
- signs the image (the multi-arch index and each platform image) and the chart
  with **cosign keyless signing** — there is no long-lived key; the signing
  certificate is issued to the workflow run through GitHub OIDC and recorded in
  the public Sigstore transparency log;
- generates an **SBOM** (SPDX JSON, with [syft](https://github.com/anchore/syft))
  of each platform image and attaches it to that platform image as a signed
  attestation;
- starts the pushed image on both architectures (`--help`) before the chart and
  the GitHub Release are published.

The GitHub Release carries the chart package, the two SBOMs
(`secretspec-operator-<tag>-linux-amd64.spdx.json` and `…-linux-arm64.spdx.json`)
and a `SHA256SUMS` file. The SBOM files there are copies; the signed
attestations in the registry are the authoritative ones, and `SHA256SUMS`
protects only the download, it is not signed itself.

A valid signature proves that the artifact was produced by that workflow of the
`jabbas/secretspec-operator` repository for exactly that tag. The identity to
check is:

| Property | Value |
|---|---|
| Certificate identity | `https://github.com/jabbas/secretspec-operator/.github/workflows/release.yml@refs/tags/<tag>` |
| OIDC issuer | `https://token.actions.githubusercontent.com` |

Set the variables for the release you are installing (`<tag>` is `v0.1.0`, `v0.1.0-rc.1`, …; the chart
version is the tag without the `v`):

```sh
TAG=v0.1.0
CHART_VERSION="${TAG#v}"
IDENTITY="https://github.com/jabbas/secretspec-operator/.github/workflows/release.yml@refs/tags/${TAG}"
ISSUER="https://token.actions.githubusercontent.com"
```

Verify the image (the tag resolves to the multi-arch index, which is signed):

```sh
cosign verify "ghcr.io/jabbas/secretspec-operator:${TAG}" \
  --certificate-identity "${IDENTITY}" \
  --certificate-oidc-issuer "${ISSUER}"
```

Verify the chart:

```sh
cosign verify "ghcr.io/jabbas/charts/secretspec-operator:${CHART_VERSION}" \
  --certificate-identity "${IDENTITY}" \
  --certificate-oidc-issuer "${ISSUER}"
```

The SBOM attestation is attached to each **platform** image, not to the
multi-arch index, because an SBOM describes the contents of one platform. Look up
the digest of the platform you run (`amd64` or `arm64`) in the index, then verify
the attestation of that digest:

```sh
ARCH=amd64   # or arm64
PLATFORM_DIGEST="$(docker buildx imagetools inspect "ghcr.io/jabbas/secretspec-operator:${TAG}" --raw \
  | jq -r --arg arch "${ARCH}" '.manifests[] | select(.platform.os == "linux" and .platform.architecture == $arch) | .digest')"

cosign verify-attestation "ghcr.io/jabbas/secretspec-operator@${PLATFORM_DIGEST}" \
  --type spdxjson \
  --certificate-identity "${IDENTITY}" \
  --certificate-oidc-issuer "${ISSUER}"
```

(`crane manifest "ghcr.io/jabbas/secretspec-operator:${TAG}"` prints the same
index if you do not have Docker.) The attestation output is a stream of signed
envelopes whose `payload` is the base64-encoded in-toto statement; to read the
SBOM itself:

```sh
cosign verify-attestation "ghcr.io/jabbas/secretspec-operator@${PLATFORM_DIGEST}" \
  --type spdxjson \
  --certificate-identity "${IDENTITY}" \
  --certificate-oidc-issuer "${ISSUER}" \
  | jq -r '.payload | @base64d | fromjson | .predicate'
```

Tags are mutable in principle. For the strongest guarantee, take the digest
printed by `cosign verify` (`critical.image.docker-manifest-digest`, the index
digest) and install with `image.digest`
([Installation](installation.md#registry-mirror-pull-secrets-and-digest-pinning)).
Only version tags are published; there is no `latest`.

## Reporting a vulnerability

Please report suspected vulnerabilities **privately** through GitHub private
vulnerability reporting: open
<https://github.com/jabbas/secretspec-operator/security/advisories/new>. Do not
open a public issue or pull request for a security problem. Include the version,
a description and, if possible, a way to reproduce it; never include real
secrets.
