# Examples

Seven copy-paste `SecretSpec`s. Each file starts with a comment that explains
when to use it. Apply one into the namespace of your application:

```sh
kubectl apply -n <namespace> -f examples/app-admin-password.yaml
kubectl get secretspecs -n <namespace>
```

The generated Secret has the same name as the `SecretSpec`. Every example has
been applied by the chart's end-to-end test in a namespace that enforces the Pod
Security Standard `restricted`.

| File | Secret | What it shows | `deletionPolicy` |
|---|---|---|---|
| [`app-admin-password.yaml`](app-admin-password.yaml) | `app-admin` | A literal `username` plus a generated `password` (32 alphanumeric characters). For an application that stores the credential on first start. | `Retain` |
| [`traefik-basic-auth.yaml`](traefik-basic-auth.yaml) | `dashboard-auth` | `basicAuth` with `keys.htpasswd: users`, the key Traefik's `BasicAuth` middleware reads. | `Delete` |
| [`garage-rpc-secret.yaml`](garage-rpc-secret.yaml) | `garage` | Random bytes in hex (`rpc-secret`), and an S3-style access key id (`prefix: GK`, 24 hex characters) with its secret. | `Retain` |
| [`fernet-key.yaml`](fernet-key.yaml) | `fernet-key` | A Fernet key: `bytes: 32`, `encoding: base64url`. | `Retain` |
| [`ssh-deploy-key.yaml`](ssh-deploy-key.yaml) | `deploy-key` | An ed25519 SSH key pair with Secret type `kubernetes.io/ssh-auth`. | `Delete` |
| [`rotated-shared-token.yaml`](rotated-shared-token.yaml) | `shared-token` | A 48-character token rotated every `30d` (`rotation.interval`). | `Delete` |
| [`rds-compatible-password.yaml`](rds-compatible-password.yaml) | `rds-master` | A password accepted by Amazon RDS: `charset: ascii`, `exclude`, and `require` for each character class. | `Retain` |

## Choosing a `deletionPolicy` for your own copy

With `Delete` (the default), uninstalling and reinstalling your chart, a GitOps
prune or a HelmRelease remediation produces **new values**. Use `Retain` for
anything an application persists. The SSH deploy key example keeps the default
because it is a minimal example; its public key is registered on the Git
server, so add `deletionPolicy: Retain` if that registration must survive a
reinstall. The reasoning is in
[Operations](../docs/operations.md#deletionpolicy-and-reinstalls).

## Reading a generated value

```sh
kubectl get secret app-admin -n <namespace> -o jsonpath='{.data.password}' | base64 -d; echo
kubectl get secret deploy-key -n <namespace> -o jsonpath='{.data.ssh-publickey}' | base64 -d
```

Consumers must reload after a rotation; see
[Operations](../docs/operations.md#consumers-must-reload). The field reference is
in the [API reference](../docs/api-reference.md).
