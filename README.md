# SecretSpec Operator

A Kubernetes operator that creates `Secret`s with generated values — passwords,
tokens, random bytes, SSH key pairs, basic-auth credentials — from a small
custom resource, `SecretSpec`. You declare *what* the Secret should contain
inside your own Helm chart or manifests; the operator generates the values once
and then leaves them alone. A value changes only when its definition changes,
when you delete the Secret (or one key), or when a rotation you configured is
due. It does not change when the operator restarts, upgrades or loses its
cache, or when you re-apply the same manifest.

```yaml
apiVersion: secretspec.jabbas.eu/v1alpha1
kind: SecretSpec
metadata:
  name: app-admin            # the Secret gets the same name and namespace
spec:
  deletionPolicy: Retain     # keep the Secret if this object is deleted
  data:
    username: admin          # literal value, copied as is
  fields:
    - name: password         # 32 random alphanumeric characters by default
```

## Quickstart

Run the `kubectl` commands from a clone of this repository.

```sh
helm install secretspec-operator oci://ghcr.io/jabbas/charts/secretspec-operator --version 0.1.0 --namespace secretspec-system --create-namespace
kubectl apply -f examples/app-admin-password.yaml
kubectl wait --for=condition=Ready secretspec/app-admin --timeout=60s
kubectl get secretspec app-admin
kubectl get secret app-admin -o jsonpath='{.data.password}' | base64 -d; echo
```

The first command installs the CRD and the operator; the second declares the
Secret `app-admin` in the `default` namespace; the last three show that the
operator made it ready and read the generated password.

## Features

- **Safe by default.** Values are generated once and stay stable; see the
  [behaviour table](docs/api-reference.md#behaviour-table).
- **Three kinds of secret**, one per `SecretSpec`: `fields` (random strings,
  tokens and encoded random bytes), `sshKeyPair` (ed25519, RSA or ECDSA) and
  `basicAuth` (username, password and a bcrypt `htpasswd` line).
- **Per-key control.** Length, character set, excluded characters, minimum
  counts per character class, prefix and suffix; or N random bytes in hex,
  base64, base64url or base32.
- **Optional scheduled rotation** (`rotation.interval`), and manual rotation by
  deleting the Secret.
- **`deletionPolicy`:** `Delete` (default, garbage-collected with the
  `SecretSpec`) or `Retain` (the Secret survives, and a re-created `SecretSpec`
  adopts it and keeps its values).
- **Mistakes surface where you look.** A bad manifest is rejected at
  `kubectl apply` / `helm install` time by the CRD schema and CEL rules; runtime
  problems appear in `status.conditions` and Events.
- **Least privilege.** No `delete` on Secrets, only metadata of managed Secrets
  is cached, values never reach logs, Events, status or metrics, and the
  operator never touches a Secret it did not create. Cluster-wide or
  per-namespace RBAC (a real boundary).
- **Production packaging.** Multi-arch image (`linux/amd64`, `linux/arm64`),
  OCI Helm chart, leader election for a highly available deployment, Prometheus
  metrics, Pod Security `restricted`, cosign-signed releases with an SBOM.

## Status

The API is `secretspec.jabbas.eu/v1alpha1`. Defaults and the fingerprint format
are frozen for this version, so upgrades never rotate your credentials. Breaking
changes before `v1beta1` are possible but are always called out in the
[changelog](CHANGELOG.md). Supported Kubernetes: 1.34 and newer.

## Documentation

| Document | Content |
|---|---|
| [Installation](docs/installation.md) | Install, upgrade, uninstall, values, scope modes, mirrors, CRD lifecycle, signature verification |
| [API reference](docs/api-reference.md) | Every field, default and validation; the generated Secret; behaviour table |
| [Operations](docs/operations.md) | Rotation, reloading consumers, `deletionPolicy`, troubleshooting, metrics, logging |
| [Security](docs/security.md) | Threat model, RBAC, redaction guarantees, release signing and SBOM |
| [Examples](examples/README.md) | Seven copy-paste `SecretSpec`s |
| [Fingerprint format](docs/fingerprints.md) | The frozen format that decides when a value is regenerated |
| [Development](docs/development.md) | Building, testing and running the operator locally |

## Licence

Apache-2.0; see [LICENSE](LICENSE) and [NOTICE](NOTICE).

## Acknowledgement

SecretSpec Operator was inspired by earlier Kubernetes secret-generator projects.
