# Installation

The operator is installed with its Helm chart, published as an OCI artifact:

```text
oci://ghcr.io/jabbas/charts/secretspec-operator
```

Release tags follow [semver](https://semver.org) exactly, without a `v` prefix:
the git tag, the chart version, the chart `appVersion` and the image tag are the
same string (`0.1.1`, `0.1.1-rc.1`, …). The container image is
`ghcr.io/jabbas/secretspec-operator:<tag>`, built for `linux/amd64` and
`linux/arm64`. There is no `latest` tag, and the chart refuses
`image.tag=latest`. The chart's default image tag is `<appVersion>`, so a chart
always installs the image of the same release.

> **Legacy tag:** `0.1.0` was released under the tag `v0.1.0` (image
> `:v0.1.0`, signing identity `refs/tags/v0.1.0`; the chart version is still
> `0.1.0`). Later releases use plain semver tags.

## Requirements

- Kubernetes **1.34 or newer**. CI tests 1.34 and 1.37.
- Helm with OCI support (3.8 or newer). Helm 4 works too; see
  [Helm 4 and field conflicts](#helm-4-and-field-conflicts).
- Optional: [`cosign`](https://docs.sigstore.dev/cosign/) to verify the release.

The pod complies with the Pod Security Standard `restricted`: it runs as a
non-root user (UID/GID 65532), with a read-only root file system, all
capabilities dropped, no privilege escalation and the `RuntimeDefault` seccomp
profile. You can enforce `restricted` on the operator's namespace. Create the
namespace first, then label it (`helm install --create-namespace` below accepts
a namespace that already exists):

```sh
kubectl create namespace secretspec-system
kubectl label namespace secretspec-system pod-security.kubernetes.io/enforce=restricted
```

## Verify the release (optional but recommended)

Releases are signed with cosign keyless signing (GitHub OIDC). The certificate
identity is the release workflow run for the tag, so the commands below verify
that the image and the chart were built by this repository's release workflow
for exactly that tag. Set `TAG` to the release you are installing and
`CHART_VERSION` to its chart version. The only published release so far is
`0.1.0`, under its legacy tag:

```sh
TAG=v0.1.0                 # the git/image tag (legacy v prefix)
CHART_VERSION=0.1.0        # the chart version
IDENTITY="https://github.com/jabbas/secretspec-operator/.github/workflows/release.yml@refs/tags/${TAG}"
ISSUER="https://token.actions.githubusercontent.com"
```

From 0.1.1 on, the tag is the plain version and equals the chart version:
`TAG=0.1.1` and `CHART_VERSION=0.1.1` (`0.1.1-rc.1` for a release candidate).

Verify the image:

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

Verify the SBOM attestation (SPDX JSON). It is attached to each platform image
rather than to the multi-arch index, so first look up the digest of your
platform (`amd64` or `arm64`) in the index:

```sh
ARCH=amd64   # or arm64
PLATFORM_DIGEST="$(docker buildx imagetools inspect "ghcr.io/jabbas/secretspec-operator:${TAG}" --raw \
  | jq -r --arg arch "${ARCH}" '.manifests[] | select(.platform.os == "linux" and .platform.architecture == $arch) | .digest')"

cosign verify-attestation "ghcr.io/jabbas/secretspec-operator@${PLATFORM_DIGEST}" \
  --type spdxjson \
  --certificate-identity "${IDENTITY}" \
  --certificate-oidc-issuer "${ISSUER}"
```

A failed verification prints an error and exits non-zero; do not install. The
output of a successful `cosign verify` contains the digest of the multi-arch
image (`critical.image.docker-manifest-digest`), which you can pin with
`image.digest` (see [Digest pinning](#registry-mirror-pull-secrets-and-digest-pinning)).
More background is in [Security](security.md#release-signing-and-sbom).

## Install

Cluster mode, a single replica:

```sh
helm install secretspec-operator \
  oci://ghcr.io/jabbas/charts/secretspec-operator \
  --version 0.1.0 \
  --namespace secretspec-system --create-namespace
```

For a highly available deployment, run two replicas; they use leader election
(see [High availability](#high-availability-and-readiness)):

```sh
helm install secretspec-operator \
  oci://ghcr.io/jabbas/charts/secretspec-operator \
  --version 0.1.0 \
  --namespace secretspec-system --create-namespace \
  --set replicas=2
```

Check the result:

```sh
kubectl --namespace secretspec-system rollout status deployment/secretspec-operator
kubectl get crd secretspecs.secretspec.jabbas.eu
```

The Deployment, the Lease and the RBAC objects are named after the *release
fullname*: the release name when it contains `secretspec-operator`, otherwise
`<release>-secretspec-operator`.

Pass many values at once with a file (`-f values.yaml`) instead of `--set`. The
chart ships a `values.schema.json`, so `helm install` rejects wrong types,
unknown top-level keys and out-of-range values before anything is applied.

## Scope: cluster mode and namespace mode

| `watchNamespaces` | RBAC rendered by the chart | Behaviour |
|---|---|---|
| `[]` (default) | One `ClusterRole` + `ClusterRoleBinding` | Watches every namespace. |
| `[team-a, team-b]` | One `Role` + `RoleBinding` in **each listed namespace**, and no cluster-wide permission at all | Watches only the listed namespaces. |

Namespace mode is a **real RBAC boundary**, not a filter: the operator's
ServiceAccount has no permission to see Secrets or SecretSpecs anywhere else.
**A `SecretSpec` in a namespace that is not listed is ignored**: no Secret is
generated, and no status or Event is written. `kubectl get secretspecs` shows an
empty `READY` column for it. The CRD itself is cluster-scoped, so the schema and
CEL validation still apply to every namespace.

```sh
helm install secretspec-operator \
  oci://ghcr.io/jabbas/charts/secretspec-operator \
  --version 0.1.0 \
  --namespace secretspec-system --create-namespace \
  --set 'watchNamespaces={team-a,team-b}'
```

The listed namespaces must exist when you install or upgrade (Helm creates a
`Role` in each of them). To change the list later, run `helm upgrade` with the
new list; the roles of removed namespaces are deleted. Do not let two releases
watch the same namespace: both would reconcile the same SecretSpecs.

The leader-election Lease lives in the release namespace and is covered by a
separate `Role` there, in both modes.

## Values

| Key | Default | Description |
|---|---|---|
| `global.imageRegistry` | `""` | Overrides `image.registry` when set (mirror / pull-through cache). |
| `image.registry` | `ghcr.io` | Image registry. |
| `image.repository` | `jabbas/secretspec-operator` | Image repository. |
| `image.tag` | `""` | Image tag; defaults to `<appVersion>`. `latest` is rejected. |
| `image.digest` | `""` | `sha256:<64 hex>`; when set it is used instead of the tag. |
| `image.pullPolicy` | `IfNotPresent` | `Always`, `IfNotPresent` or `Never`. |
| `imagePullSecrets` | `[]` | Passed to the pod unchanged, e.g. `[{name: mirror-pull}]`. |
| `replicas` | `1` | Number of replicas. More than 1 requires `leaderElection.enabled`. |
| `leaderElection.enabled` | `true` | Leader election through a Lease named after the release fullname. |
| `watchNamespaces` | `[]` | `[]` = all namespaces; a list = those namespaces only. |
| `crds.install` | `true` | Render the SecretSpec CRD. |
| `crds.keep` | `true` | Add `helm.sh/resource-policy: keep` to the CRD. |
| `resyncInterval` | `1h` | Periodic resync, `<n>s`, `<n>m` or `<n>h`, from 1 minute to 24 hours. |
| `concurrency` | `4` | Maximum concurrent reconciles. |
| `logLevel` | `info` | Log filter directive, e.g. `info` or `secretspec_operator=debug,warn`. See [Operations](operations.md#logging). |
| `logFormat` | `json` | `json` or `text`. |
| `http.port` | `8080` | Port of `/healthz`, `/readyz` and `/metrics`. |
| `serviceAccount.create` | `true` | Create the ServiceAccount. |
| `serviceAccount.name` | `""` | Name; defaults to the release fullname. Required if `create` is `false`. |
| `serviceAccount.annotations` | `{}` | ServiceAccount annotations. |
| `resources` | requests `cpu: 10m`, `memory: 32Mi`; limit `memory: 128Mi` | Container resources. |
| `terminationGracePeriodSeconds` | `30` | Pod termination grace period. |
| `podSecurityContext` | UID/GID 65532, `runAsNonRoot`, `fsGroup: 65532`, seccomp `RuntimeDefault` | Pod security context. |
| `securityContext` | no privilege escalation, read-only root file system, drop `ALL` | Container security context. |
| `podAnnotations`, `podLabels` | `{}` | Extra pod metadata. |
| `nodeSelector`, `tolerations`, `affinity` | `{}`, `[]`, `{}` | Scheduling. |
| `priorityClassName` | `""` | Priority class. |
| `metrics.service.enabled` | `true` | ClusterIP Service exposing the port `http`. |
| `serviceMonitor.enabled` | `false` | Prometheus Operator `ServiceMonitor`; needs `metrics.service.enabled`. |
| `serviceMonitor.interval` | `30s` | Scrape interval. |
| `serviceMonitor.labels` | `{}` | Extra `ServiceMonitor` labels, e.g. to match a Prometheus selector. |

The chart fails at render time for `replicas > 1` without leader election, for
`serviceMonitor.enabled` without `metrics.service.enabled`, and for
`image.tag=latest`. The same table is in the
[chart README](../charts/secretspec-operator/README.md).

## Registry mirror, pull secrets and digest pinning

The image reference is built as follows:

- `<registry>/<repository>@<digest>` when `image.digest` is set;
- otherwise `<registry>/<repository>:<tag>`, with `tag` defaulting to
  `<appVersion>`.

`<registry>` is `global.imageRegistry` when it is non-empty, otherwise
`image.registry`. A pull-through cache or mirror therefore needs one override;
`imagePullSecrets` is passed to the pod unchanged:

```sh
helm install secretspec-operator \
  oci://ghcr.io/jabbas/charts/secretspec-operator \
  --version 0.1.0 \
  --namespace secretspec-system --create-namespace \
  --set global.imageRegistry=mirror.example.com \
  --set 'imagePullSecrets[0].name=mirror-pull'
```

The mirror must serve the repository `jabbas/secretspec-operator` under the same
path; if it uses a different path, also set `image.repository`.

To pin the image by digest (so the tag can never be re-pointed), take the digest
from `cosign verify` or your registry and set `image.digest`. The digest takes
precedence over the tag, and a mirror that preserves digests serves the same
image:

```sh
helm upgrade secretspec-operator \
  oci://ghcr.io/jabbas/charts/secretspec-operator \
  --version 0.1.0 \
  --namespace secretspec-system \
  --reuse-values \
  --set image.digest=sha256:<64-hex-digest-from-cosign-verify>
```

`image.digest` must match `^sha256:[a-f0-9]{64}$` (or be empty).

## CRD lifecycle

The `SecretSpec` CRD is rendered as a normal chart template, not from Helm's
`crds/` directory (Helm never upgrades the latter). `helm upgrade` therefore
upgrades the CRD together with the operator.

- `crds.install=true` (default) renders the CRD; `false` leaves it out.
- `crds.keep=true` (default) adds the annotation `helm.sh/resource-policy: keep`
  to the CRD, so `helm uninstall` **does not delete it**.

> **Deleting the CRD deletes every `SecretSpec` in the cluster.** For
> `deletionPolicy: Delete` objects, Kubernetes then garbage-collects their
> Secrets too, which means new credentials after a reinstall. This is why
> `helm uninstall` keeps the CRD by default. Delete it only on purpose:
> `kubectl delete crd secretspecs.secretspec.jabbas.eu`.

### One Helm release owns the CRD

A CRD is cluster-wide, so exactly one Helm release may own it.

- **A second install of the chart in the same cluster** (for example a second
  namespace-mode release for other teams) must set `crds.install=false`.
  Otherwise Helm refuses it, because the CRD already belongs to the first
  release.

  ```sh
  helm install secretspec-operator-b \
    oci://ghcr.io/jabbas/charts/secretspec-operator \
    --version 0.1.0 \
    --namespace secretspec-b --create-namespace \
    --set crds.install=false \
    --set 'watchNamespaces={team-b}'
  ```

  Upgrade the CRD only through the release that owns it, and keep every
  release on a compatible chart version.
- **A CRD applied without Helm** (for example with `kubectl apply -f`) makes
  `helm install` fail with an "invalid ownership metadata" error: Helm does not
  take over objects it did not create. Either use `crds.install=false` and
  manage the CRD yourself, or adopt it by giving it the metadata Helm checks —
  the label `app.kubernetes.io/managed-by: Helm` and the annotations
  `meta.helm.sh/release-name` and `meta.helm.sh/release-namespace` that name
  the release you are about to install:

  ```sh
  kubectl label crd secretspecs.secretspec.jabbas.eu \
    app.kubernetes.io/managed-by=Helm --overwrite
  kubectl annotate crd secretspecs.secretspec.jabbas.eu \
    meta.helm.sh/release-name=secretspec-operator \
    meta.helm.sh/release-namespace=secretspec-system --overwrite
  ```

  Then run `helm install` with the same release name and namespace.
- **After `helm uninstall`** the kept CRD still carries the metadata of the old
  release. Installing again under the *same* release name and namespace works.
  Installing under a different name or namespace needs the same adoption
  commands (with the new values) or `crds.install=false`.

### Helm 4 and field conflicts

Helm 4 applies chart objects with server-side apply. If another field manager
changed a field that the chart sets — for example someone ran
`kubectl apply --server-side` on the CRD or edited the Deployment — a later
`helm upgrade` fails with a field-conflict error. To let Helm take those fields
back, repeat the upgrade with `--force-conflicts`:

```sh
helm upgrade secretspec-operator \
  oci://ghcr.io/jabbas/charts/secretspec-operator \
  --version 0.1.0 \
  --namespace secretspec-system \
  --reuse-values \
  --force-conflicts
```

Check what changed first (`kubectl get crd secretspecs.secretspec.jabbas.eu
--show-managed-fields -o yaml`): forcing overwrites the other manager's values
for the fields the chart sets. Helm 3 uses client-side apply and does not have
this failure mode.

## Upgrade

```sh
helm upgrade secretspec-operator \
  oci://ghcr.io/jabbas/charts/secretspec-operator \
  --version 0.1.0 \
  --namespace secretspec-system \
  --reuse-values
```

Use `-f values.yaml` instead of `--reuse-values` if you keep your values in a
file. The CRD is upgraded with the chart. Upgrading the operator **never changes
generated values**: defaults and the fingerprint format are frozen within an API
version, and a golden test guards them. Check the [changelog](../CHANGELOG.md)
for breaking `v1alpha1` changes before upgrading.

## Uninstall

```sh
helm uninstall secretspec-operator --namespace secretspec-system
```

With the default `crds.keep=true` the CRD, every `SecretSpec` and every
generated Secret stay in the cluster; nothing is reconciled until the operator
is installed again. To remove everything, including all `SecretSpec`s (and the
Secrets of those with `deletionPolicy: Delete`):

```sh
kubectl delete crd secretspecs.secretspec.jabbas.eu
```

Credentials that are persisted by applications must use `deletionPolicy:
Retain`; see [Operations](operations.md#deletionpolicy-and-reinstalls).

## High availability and readiness

With `replicas: 2` (or more) and `leaderElection.enabled=true`, all replicas
run, but only the **leader** reconciles. Leadership is a Lease in the release
namespace (named after the release fullname): it is valid for 15 seconds, the
leader renews it every 5 seconds, and a standby retries every 5 seconds. On a
graceful shutdown the leader releases the Lease, so a standby takes over within
about 5 seconds; after a crash it takes over once the Lease has expired, within
about 20 seconds. A leader change never changes a generated value.

With `leaderElection.enabled=false` (single replica only) the chart renders the
Deployment with the `Recreate` strategy: the old Pod stops before the new one
starts, so a rollout never runs two operators at once.

Do not delete the Lease while the operator runs; see
[Operations](operations.md#leader-election-lease).

The pod probes use two endpoints on `http.port`:

| Endpoint | Meaning |
|---|---|
| `/healthz` | `200` while the process runs (liveness and startup probe). |
| `/readyz` | `200` after **one successful API discovery call** (the `SecretSpec` API is installed and reachable) **and** either the controllers are running (this instance is the leader, or leader election is disabled) **or** this instance is standing by as a non-leader. Otherwise `503`. |

Two consequences:

- With two replicas, the **non-leader pod is Ready** too. A rollout can proceed
  when the new pod is merely standing by, and a Service endpoint list does not
  tell you which pod reconciles.
- A `PodDisruptionBudget` therefore **does not protect the leader
  specifically**: it counts ready pods, and a disruption may evict the leader
  while the standby is untouched. That is harmless — the standby takes over — but
  expect a short failover rather than uninterrupted reconciliation. The chart
  does not render a PodDisruptionBudget.

If the CRD is missing, the pod stays not ready (`503`) and logs
`API discovery failed; is the CRD installed?` with a growing back-off of up to
30 seconds, until the CRD appears.

`terminationGracePeriodSeconds` (default `30`) bounds a graceful shutdown. On
`SIGTERM` the operator stops taking new work, lets in-flight reconciles finish,
and releases the Lease (best effort, at most 5 seconds). Thirty seconds is
enough for this; raise it only if you also raise `concurrency` substantially.

## Next steps

- Declare your first Secret: [examples](../examples/README.md) and the
  [API reference](api-reference.md).
- Scrape metrics and learn the failure modes: [Operations](operations.md).
