# Operations

Day-two guide for platform teams and application developers: rotating
credentials, choosing a `deletionPolicy`, reading conditions and Events,
monitoring and logging.

## Manual rotation

To rotate, **delete the Secret**. The operator notices (it watches the Secrets it
manages) and regenerates every generated value within seconds; literals from
`data` are unchanged:

```sh
kubectl delete secret app-admin
kubectl get secret app-admin -o jsonpath='{.metadata.annotations.secretspec\.jabbas\.eu/generated-at}'; echo
```

For a `fields` Secret you can rotate **one key** by removing just that key; only
it is regenerated:

```sh
kubectl patch secret app-admin --type=json \
  -p '[{"op": "remove", "path": "/data/password"}]'
```

For an SSH key pair or `basicAuth`, delete the whole Secret, so that the private
key, public key, password and htpasswd line stay consistent.

There is deliberately no "regenerate" annotation or field: such a request,
committed to git, would be replayed on every sync. Deleting the Secret is the
only manual trigger. The previous value is **not** kept.

### Consumers must reload

The operator changes the Secret and nothing else; it does not restart or notify
workloads. After any rotation, each consumer has to pick up the new value:

- **Secret mounted as a volume.** The kubelet refreshes the files in place after
  a delay (up to about a minute or two). The application must re-read the file;
  one that reads it once at start will not notice. Mounts with `subPath` are
  never refreshed.
- **Secret exposed as environment variables** (`env.valueFrom.secretKeyRef` or
  `envFrom`). Environment variables are fixed when the container starts. The pod
  must be restarted (`kubectl rollout restart deployment/<name>`).
- **Reloader tools.** Generic reloader controllers watch Secrets and restart or
  signal the workloads that use them whenever the Secret changes. They work
  with this operator like with any other Secret writer; pick the one that fits
  your platform.

Remember that some systems keep a copy of the credential and must be updated
too: a database created with the old password does not follow a rotation.

## Scheduled rotation

`spec.rotation.interval` (`90d`, `24h`, …; minimum `1h`) regenerates **every
generated value** of the Secret at once, including both halves of an SSH key
pair. Literals are untouched.

- **The clock starts when the operator first observes the schedule.** When a
  `SecretSpec` with `rotation` is created, `generated-at` is that moment and the
  first rotation is one interval later. When you add `rotation` to an existing
  `SecretSpec`, there is no immediate rotation: if the Secret's `generated-at` is
  not yet one interval old, the schedule counts from it; otherwise (or if it is
  missing) it counts from now. The same holds when the status is lost or a new
  `SecretSpec` adopts a retained Secret — the first rotation can then be up to
  one interval away, never sooner than expected.
- **Missed intervals coalesce.** If the operator was down for several
  intervals, it rotates once on return, and the next rotation is one interval
  after that.
- **Changing the interval** moves the next rotation to `generated-at + interval`.
  Shortening it so that this time is already past rotates immediately.
- **Removing `rotation`** disables the schedule; values stay as they are.
- The next rotation is visible in the `NEXT ROTATION` column (an RFC 3339
  timestamp in UTC, for example `2026-11-22T10:00:00Z`) and in
  `status.nextRotationTime`; `status.lastRotationTime` records the last one. A
  `Rotated` Event lists the keys, never values.

```sh
kubectl get secretspec shared-token
```

## `deletionPolicy` and reinstalls

> **Warning.** With `deletionPolicy: Delete` (the default), anything that deletes
> the `SecretSpec` also deletes the Secret, and the next creation produces **new
> values**. That includes `helm uninstall` followed by `helm install`, a
> HelmRelease remediation (Flux) or a prune (Argo CD, Flux, any GitOps tool),
> and deleting the CRD.
>
> Credentials that applications **persist** — for example a database password
> used at database initialisation, an admin account created on first start, an
> encryption key for stored data — **must use `deletionPolicy: Retain`**.
> Otherwise the application keeps the old value while the Secret holds a new
> one.

Under `Retain` the Secret keeps its labels and annotations but has no owner, so
deleting the `SecretSpec` leaves it untouched. Re-creating a `SecretSpec` with
the same name **adopts** it: values whose definition is unchanged are kept, the
rest are regenerated, and an `Adopted` Event is emitted. This is how a
credential survives a reinstall. See [Security](security.md#adoption) for the
adoption rules.

When to choose which:

| Use `Retain` when… | `Delete` is fine when… |
|---|---|
| An application copies the value into its own state (database init, first-run admin user). | The value is only ever read from the Secret and the consumer can be restarted. |
| An encryption key protects data you cannot lose. | The Secret is a throw-away token or a test fixture. |
| The value is registered on an external system, so that a new value would lock you out. **An SSH deploy key** is the typical case: its public half is registered on the Git server, and a regenerated pair would no longer match. | The whole environment is routinely recreated, including its dependencies. |

The [SSH deploy key example](../examples/ssh-deploy-key.yaml) uses the default
`Delete`; add `deletionPolicy: Retain` if the public key registration must
survive a reinstall. A retained Secret is never removed by the operator; delete
it yourself (`kubectl delete secret <name>`) when you no longer need it.

## Troubleshooting

Start with the object:

```sh
kubectl get secretspecs -A                      # READY / KIND / NEXT ROTATION
kubectl describe secretspec <name> -n <namespace>   # conditions and Events
kubectl get secretspec <name> -n <namespace> \
  -o jsonpath='{.status.conditions[?(@.type=="Ready")].reason}: {.status.conditions[?(@.type=="Ready")].message}{"\n"}'
```

Everything the operator knows that is specific to one `SecretSpec` appears in
its `Ready` condition and in Events — not only in the operator's logs. If a
`SecretSpec` has **no status at all**, the operator has not reconciled it:
check that the operator pods are running (`kubectl -n secretspec-system get
pods`), that the CRD matches the chart, and — in namespace mode — that the
`SecretSpec`'s namespace is in `watchNamespaces`; objects in other namespaces are
[ignored](installation.md#scope-cluster-mode-and-namespace-mode).

### Ready condition

| Reason | Status | Meaning and fix |
|---|---|---|
| `Generated` | `True` | The Secret matches the spec. |
| `Conflict` | `False` | A Secret with the target name exists that this `SecretSpec` may not manage, or has the wrong type. The message says which: `Secret <name> exists and is not managed by this SecretSpec` (no `managed-by`/`spec-name` labels of the operator); `… is controlled by <Kind> <name> (<apiVersion>) and not by a SecretSpec`; or `Secret <name> has type <old>; delete it to recreate it with type <new>`. **Nothing is written.** The operator never adopts an unlabelled Secret. Rename the `SecretSpec`, or delete the foreign Secret if it is yours to delete; for a type mismatch delete the Secret (all values are regenerated). |
| `InvalidSpec` | `False` | Runtime validation failed; the message lists each problem with its field path, e.g. `spec.fields[0].exclude: alphabet must contain at least 2 characters after exclude`. Fix the `SecretSpec`. |
| `ApplyFailed` | `False` | Writing the Secret failed. The message is `apply of Secret failed: <HTTP code> <reason>` (for example `403 Forbidden`); the server's message is deliberately not copied, because it can echo submitted data. Retried every **30 seconds**. Check RBAC, admission policies and quotas for Secrets in that namespace. |
| `GenerationFailed` | `False` | Generating a value failed inside the operator (SSH key library, bcrypt) — not a problem of your spec. Message: `value generation failed: <cause>`. Retried after **30 seconds**. If it persists, report a bug. |
| `WaitingForGarbageCollection` | `False` | The Secret still has an owner reference to a *deleted* `SecretSpec` of the same name (you deleted and re-created it quickly under `Delete`). The operator waits and re-checks every 5 seconds instead of adopting the Secret. It resolves when Kubernetes garbage collection finishes; if it lasts minutes, the garbage collector is stuck (check the controller manager). No Event is emitted for this reason. |

A `Conflict` is re-checked about **every minute** (or at the periodic resync,
if that is shorter): the operator's Secret watch only sees Secrets that carry
its labels, so it cannot notice by itself that a *foreign* blocker is gone.
Delete the foreign Secret (or rename the `SecretSpec`) and the status turns
`Generated` within about a minute, without a restart. Each re-check plans the
same `Conflict` event again; Kubernetes aggregates identical events into a
series, so `kubectl describe` shows one event with a growing count.

An `InvalidSpec` is re-evaluated when the `SecretSpec` changes, when a Secret
the operator manages changes, and at the periodic resync (default one hour,
`resyncInterval`). To make the operator look immediately, restart it; this is
safe, values never change on restart:

```sh
kubectl --namespace secretspec-system rollout restart deployment/secretspec-operator
```

### Events

Events are published for the `SecretSpec` (`kubectl describe`). Messages name
keys and never contain values.

| Type | Reason | When | Message |
|---|---|---|---|
| Normal | `Generated` | The Secret was created. | `generated keys: <names>` |
| Normal | `Regenerated` | Values were regenerated because their definition changed, or a key was removed from the Secret. | `regenerated keys: <names>` |
| Normal | `Rotated` | A scheduled rotation ran. | `rotated keys: <names>` |
| Normal | `Adopted` | A retained Secret was adopted. | `adopted existing Secret <name>` |
| Warning | `Conflict` | See the table above. | as the condition |
| Warning | `InvalidSpec` | See the table above. | as the condition |
| Warning | `ApplyFailed` | See the table above. | as the condition |
| Warning | `GenerationFailed` | See the table above. | as the condition |
| Warning | `LowEntropy` | A (re)generated value has less than 128 bits of entropy. The spec is **not** rejected. | `keys with less than 128 bits of entropy: <names>` |
| Warning | `FingerprintsReset` | The fingerprint annotation was missing, not valid JSON or had an invalid entry (for example after someone edited or stripped annotations). Present values were **kept** and the fingerprints re-stamped; nothing was rotated. | `kept existing values for keys: <names> (fingerprint metadata was missing or invalid)` |
| Warning | `AnchorReset` | The `generated-at` annotation was not a valid RFC 3339 time. The rotation anchor was reset to now; no rotation happened. | `annotation secretspec.jabbas.eu/generated-at was not a valid RFC 3339 time; the rotation anchor was reset to <time>` |
| Warning | `PasswordTruncated` | A (re)generated `basicAuth` password is longer than 72 bytes. | `password for key <key> is longer than 72 bytes; the htpasswd hash covers only the first 72 bytes` |

On `ApplyFailed`, `FingerprintsReset` and `AnchorReset` are not emitted for that
attempt (the state they describe was not saved); they are planned again on the
retry.

### bcrypt and the 72-byte limit

bcrypt, which produces the `htpasswd` line, only uses the **first 72 bytes** of
a password. With the default `length: 32` this never matters. If `prefix` +
`length` + `suffix` of `basicAuth.password` exceeds 72, the plain `password` key
holds the full value but the htpasswd hash covers only its first 72 bytes, so a
server that checks the htpasswd line accepts any password that begins with those
72 bytes. The operator warns with `PasswordTruncated`; keep passwords at or
below 72 bytes.

### Leader-election Lease

The Lease (named after the release fullname, in the release namespace) is
owned by the operator. **Do not delete it while the operator runs.** A leader
that finds its Lease gone treats it as lost leadership: it stops reconciling and
exits, and the pod restarts (a short pause, no value changes). To hand over
leadership on purpose, restart the leader pod instead. The Lease has no
effect on generated values.

## Using SecretSpec in a Helm chart

Application charts usually share a "common labels" helper. The standard one
emits `app.kubernetes.io/managed-by: Helm`, and **`secret.labels` rejects that
key** (and keys under `secretspec.jabbas.eu/`), because the operator owns the
label on the Secrets it manages. The `SecretSpec`'s own `metadata.labels` may
carry it — only `spec.secret.labels` and `spec.secret.annotations` are checked.

Remove the key before passing the labels on to the Secret:

```yaml
# templates/secretspec.yaml
apiVersion: secretspec.jabbas.eu/v1alpha1
kind: SecretSpec
metadata:
  name: {{ include "myapp.fullname" . }}-admin
  labels:
    {{- include "myapp.labels" . | nindent 4 }}        # managed-by: Helm is fine here
spec:
  deletionPolicy: Retain
  secret:
    labels:
      {{- omit (include "myapp.labels" . | fromYaml) "app.kubernetes.io/managed-by" | toYaml | nindent 6 }}
  data:
    username: admin
  fields:
    - name: password
```

Replace `myapp.fullname` and `myapp.labels` with your chart's helpers. Without
the `omit`, `helm install` fails with `label and annotation keys must be at most
253 characters; keys under secretspec.jabbas.eu/ and app.kubernetes.io/managed-by
are reserved`.

Also keep in mind the [reinstall warning](#deletionpolicy-and-reinstalls): the
Secret generated for a chart that is uninstalled and installed again is new
unless the `SecretSpec` uses `Retain`.

## Metrics

`/metrics` is served on the HTTP port (`http.port`, default `8080`) in the
OpenMetrics text format. The chart creates a Service for it
(`metrics.service.enabled`) and, optionally, a Prometheus Operator
`ServiceMonitor` (`serviceMonitor.enabled`).

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `secretspec_reconcile_total` | counter | `result` = `ok`, `error` | Finished reconciles. |
| `secretspec_reconcile_duration_seconds` | histogram | — | Duration of one reconcile (buckets from 5 ms). |
| `secretspec_generations_total` | counter | `reason` = `Generated`, `Regenerated`, `Rotated` | Generation events. |
| `secretspec_secretspecs` | gauge | `ready` = `true`, `false`, `unknown` | Number of `SecretSpec`s by `Ready` status. |
| `secretspec_next_rotation_timestamp_seconds` | gauge | `namespace`, `name` | Unix time of the next scheduled rotation. **One series per `SecretSpec` that has `status.nextRotationTime`**, i.e. with `rotation`. |

The two gauges are computed at scrape time from the controller's cache, so a
deleted `SecretSpec` disappears from them. Readiness is a count by state, not a
per-object series, so that cardinality does not grow with the number of
`SecretSpec`s; use `kubectl get secretspecs -A` to see which ones are not ready.
Only the **leader** has a cache: a standby replica reports zero for the gauges
and its counters stay at zero, so aggregate across pods (`max`, `sum`).

Examples:

```promql
# SecretSpecs that are not ready
max(secretspec_secretspecs{ready="false"}) > 0

# reconcile errors
sum(rate(secretspec_reconcile_total{result="error"}[10m])) > 0

# a rotation is overdue by more than an hour
max by (namespace, name) (secretspec_next_rotation_timestamp_seconds) < time() - 3600
```

## Logging

The operator logs to standard error, as JSON by default (`logFormat: json`;
`text` for humans). Entries carry the namespace, name and reason, and **never a
secret value**.

`logLevel` (env `LOG_LEVEL`) is a tracing filter directive: `info` (default),
`debug`, or per-target such as `secretspec_operator=debug,warn`.

**`LOG_LEVEL` can never turn on `kube_client` output.** The Kubernetes client
library logs, at debug level, HTTP response bodies when decoding fails and API
error messages. For a Secret, a response body contains its data, and error
messages can echo what was submitted. The operator therefore forces
`kube_client=off` whatever the filter says, and drops any user directive that
could re-enable it (a `kube_client…` target, or a span filter such as
`[HTTP]=trace` that would override the static `off`). It logs a warning
`ignoring log directive for kube_client: it may log Secret contents` at start-up
for each dropped directive, and its own logs report Kubernetes errors as
HTTP code and reason only. Raising `logLevel` to `debug` or `trace` for
troubleshooting therefore does not expose Secret contents through the client
library.

## Running the operator outside the chart

The binary takes flags, each with an environment variable (the chart sets the
variables):

| Flag | Env | Default | Meaning |
|---|---|---|---|
| `--watch-namespaces` | `WATCH_NAMESPACES` | empty (all) | Comma-separated namespaces. |
| `--leader-election` | `LEADER_ELECTION` | `true` | `true` or `false`. |
| `--lease-namespace` | `POD_NAMESPACE` | — | Lease namespace; **required** with leader election. |
| `--pod-name` | `POD_NAME` | hostname | Lease holder identity. |
| `--lease-name` | `LEASE_NAME` | `secretspec-operator` | Lease name. |
| `--resync-interval` | `RESYNC_INTERVAL` | `1h` | `<n>s`, `<n>m` or `<n>h`, 1 minute to 24 hours. |
| `--http-bind` | `HTTP_BIND` | `0.0.0.0:8080` | Address of the health and metrics server. |
| `--log-level` | `LOG_LEVEL` | `info` | Filter directive. |
| `--log-format` | `LOG_FORMAT` | `json` | `json` or `text`. |
| `--concurrency` | `CONCURRENCY` | `4` | Concurrent reconciles. |

To run it locally against your current kubeconfig, **disable leader election**:
otherwise it exits at once because there is no lease namespace (and, on macOS,
no hostname file to fall back to for the identity). Apply the CRD first:

```sh
kubectl apply --server-side \
  -f charts/secretspec-operator/files/crd/secretspecs.secretspec.jabbas.eu.yaml
cargo run -p secretspec-operator -- \
  --leader-election false --log-format text --http-bind 127.0.0.1:8080
```

With leader election you must also pass `--lease-namespace` and `--pod-name`.
Run this way the operator has your own (probably cluster-admin) rights, not the
chart's minimal RBAC. More in [Development](development.md).
