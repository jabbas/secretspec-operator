<!-- SPDX-License-Identifier: Apache-2.0 -->

# secretspec-operator Helm chart

Installs the SecretSpec Operator: the `SecretSpec` CRD, the operator
Deployment, its ServiceAccount and RBAC, and an optional metrics Service and
ServiceMonitor.

## Install

```sh
helm install secretspec-operator \
  oci://ghcr.io/jabbas/charts/secretspec-operator \
  --version 0.1.0 \
  --namespace secretspec-system --create-namespace
```

Watch only some namespaces (a Role + RoleBinding per namespace, no
cluster-wide RBAC except the CRD):

```sh
helm install secretspec-operator \
  oci://ghcr.io/jabbas/charts/secretspec-operator \
  --version 0.1.0 \
  --namespace secretspec-system --create-namespace \
  --set 'watchNamespaces={team-a,team-b}'
```

In namespace mode, SecretSpecs in any namespace that is not listed are
ignored: the operator has no permission to see them.

Upgrade with `helm upgrade` (the CRD is a template, so it is upgraded too).
Uninstall with `helm uninstall`; with `crds.keep: true` (the default) the
CRD and every SecretSpec stay in the cluster. Deleting the CRD deletes every
SecretSpec.

For high availability install with `--set replicas=2`: the replicas elect a
leader through a Lease, and the non-leader reports Ready while it stands by (so
a PodDisruptionBudget does not protect the leader specifically).

## One release owns the CRD

The CRD is cluster-wide, so exactly one release may own it. A second install of
this chart in the same cluster (for example another namespace-mode release)
must set `--set crds.install=false`. Helm also refuses a CRD that was applied
without Helm (`kubectl apply`): adopt it by labelling it
`app.kubernetes.io/managed-by=Helm` and annotating it with
`meta.helm.sh/release-name` and `meta.helm.sh/release-namespace`, or use
`crds.install=false`. With Helm 4, a field conflict on `helm upgrade` is
resolved with `--force-conflicts`. Details and commands:
[docs/installation.md](https://github.com/jabbas/secretspec-operator/blob/main/docs/installation.md#crd-lifecycle).

## Scope and RBAC

| `watchNamespaces` | RBAC |
|---|---|
| `[]` (default) | ClusterRole + ClusterRoleBinding, all namespaces |
| `[a, b]` | Role + RoleBinding in each listed namespace |

Permissions (complete list): `secretspecs` get/list/watch;
`secretspecs/status` get/patch/update; `secretspecs/finalizers` update;
`secrets` get/list/watch/create/patch/update (no delete); `events.k8s.io`
`events` create/patch. With leader election, a Role in the release namespace
grants `coordination.k8s.io` `leases` get/update on the operator's own Lease
(by `resourceNames`) and create. Without leader election the Deployment uses
the `Recreate` strategy, so a rollout never runs two operators at once.

## Image reference

The image is `<registry>/<repository>@<digest>` when `image.digest` is set,
else `<registry>/<repository>:<tag>`. `<registry>` is `global.imageRegistry`
when set, else `image.registry`, so a registry mirror needs one override:

```sh
--set global.imageRegistry=mirror.example.com \
--set 'imagePullSecrets[0].name=mirror-pull'
```

`image.tag` defaults to `<appVersion>`: the image tag, the chart version and
the git release tag are the same semver string. (`0.1.0` is the one exception:
it was released as `v0.1.0`, with the image `:v0.1.0`.) `latest` is rejected.

## Values

| Key | Default | Description |
|---|---|---|
| `global.imageRegistry` | `""` | Overrides `image.registry` (mirror / pull-through cache). |
| `image.registry` | `ghcr.io` | Image registry. |
| `image.repository` | `jabbas/secretspec-operator` | Image repository. |
| `image.tag` | `""` | Image tag; defaults to `<appVersion>`. |
| `image.digest` | `""` | `sha256:…`; used instead of the tag when set. |
| `image.pullPolicy` | `IfNotPresent` | `Always`, `IfNotPresent` or `Never`. |
| `imagePullSecrets` | `[]` | Passed to the pod unchanged. |
| `replicas` | `1` | Replicas; more than one requires `leaderElection.enabled`. |
| `leaderElection.enabled` | `true` | Leader election through a Lease named after the release. |
| `watchNamespaces` | `[]` | `[]` = all namespaces; a list of namespaces restricts scope and RBAC. |
| `crds.install` | `true` | Render the SecretSpec CRD. |
| `crds.keep` | `true` | Add `helm.sh/resource-policy: keep` to the CRD. |
| `resyncInterval` | `1h` | Periodic resync interval. |
| `concurrency` | `4` | Maximum concurrent reconciles. |
| `logLevel` | `info` | Log filter directive. |
| `logFormat` | `json` | `json` or `text`. |
| `http.port` | `8080` | Port of `/healthz`, `/readyz` and `/metrics` (named `http`). |
| `serviceAccount.create` | `true` | Create the ServiceAccount. |
| `serviceAccount.name` | `""` | Name; defaults to the release fullname. |
| `serviceAccount.annotations` | `{}` | ServiceAccount annotations. |
| `resources` | requests `cpu: 10m`, `memory: 32Mi`; limits `memory: 128Mi` | Container resources. |
| `terminationGracePeriodSeconds` | `30` | Pod termination grace period. |
| `podSecurityContext` | non-root UID/GID 65532, `fsGroup: 65532`, seccomp `RuntimeDefault` | Pod security context (PSA `restricted`). |
| `securityContext` | no privilege escalation, read-only root FS, drop `ALL` | Container security context (PSA `restricted`). |
| `podAnnotations` | `{}` | Extra pod annotations. |
| `podLabels` | `{}` | Extra pod labels. |
| `nodeSelector` | `{}` | Node selector. |
| `tolerations` | `[]` | Tolerations. |
| `affinity` | `{}` | Affinity. |
| `priorityClassName` | `""` | Priority class name. |
| `metrics.service.enabled` | `true` | ClusterIP Service exposing port `http`. |
| `serviceMonitor.enabled` | `false` | Prometheus Operator ServiceMonitor (needs the metrics Service). |
| `serviceMonitor.interval` | `30s` | Scrape interval. |
| `serviceMonitor.labels` | `{}` | Extra ServiceMonitor labels. |

`values.schema.json` validates types and enums; `helm install` rejects
invalid values before anything is applied.

## Documentation

Installation (including signature verification with `cosign verify`), the API
reference, operations and security are documented in the
[repository](https://github.com/jabbas/secretspec-operator#documentation).
