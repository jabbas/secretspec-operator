# API reference

Group `secretspec.jabbas.eu`, version `v1alpha1`, kind `SecretSpec`,
namespaced. Plural `secretspecs`, short name `ss`; the `status` subresource is
enabled.

```sh
kubectl get secretspecs          # or: kubectl get ss
kubectl explain secretspec.spec  # field documentation, straight from the CRD
```

Printer columns: `READY` (the `Ready` condition status), `KIND`
(`Fields`, `SSHKeyPair` or `BasicAuth`), `NEXT ROTATION` and `AGE`.

One `SecretSpec` describes **exactly one kind of secret**: set exactly one of
`fields`, `sshKeyPair` or `basicAuth`. An application that needs a password and
an SSH key uses two `SecretSpec`s, which produce two Secrets. The generated
Secret always has the **same name and namespace** as the `SecretSpec`, so
`metadata.name` must be a valid Secret name of **at most 63 characters** (it is
also used as a label value).

```yaml
apiVersion: secretspec.jabbas.eu/v1alpha1
kind: SecretSpec
metadata:
  name: app-db
spec:
  deletionPolicy: Retain      # Delete (default) | Retain
  rotation:
    interval: 90d             # optional
  secret:                     # optional: type, labels and annotations of the Secret
    type: Opaque
  data:                       # optional literals, copied verbatim
    username: app
  fields:                     # exactly one of fields | sshKeyPair | basicAuth
    - name: password
```

## Defaults: what is written and what is resolved

Defaults are part of the API contract and **never change within `v1alpha1`**,
because they feed the [fingerprints](fingerprints.md) that decide whether a
value is regenerated. Defaults that the schema can express are written into the
stored object by the API server (`deletionPolicy`, `secret.type`,
`sshKeyPair.algorithm`, every `keys.*`, `basicAuth.password: {}`). The others are
*resolved* by the operator and deliberately **not** written into the object:
`length` (32), `charset` (`alphanumeric`), `rsaBits` (4096), `ecdsaCurve`
(`P-256`) and the SSH `comment` (`<name>@<namespace>`). `kubectl get ss -o yaml`
therefore shows only what you set plus the written defaults.

## `spec`

| Field | Type | Default | Rules |
|---|---|---|---|
| `deletionPolicy` | `Delete` \| `Retain` | `Delete` | What happens to the Secret when the `SecretSpec` is deleted. See [`deletionPolicy`](#deletionpolicy). |
| `rotation.interval` | string | none (no rotation) | `^[1-9][0-9]*(m\|h\|d)$`, between `1h` and `3650d` (`60m` is the minimum in minutes). Examples: `24h`, `90d`. |
| `secret.type` | string | `Opaque` | 1–253 characters. Allowed: `Opaque`, `kubernetes.io/basic-auth`, `kubernetes.io/ssh-auth`, or a custom type that does not start with `kubernetes.io/` and is not `bootstrap.kubernetes.io/token`. **Immutable.** `kubernetes.io/basic-auth` needs `basicAuth` with the default `username`/`password` keys; `kubernetes.io/ssh-auth` needs `sshKeyPair` with the default `ssh-privatekey` key. |
| `secret.labels` | map string→string | `{}` | At most 32 entries; keys at most 253 characters, values at most 4096. Keys under `secretspec.jabbas.eu/` and the key `app.kubernetes.io/managed-by` are **reserved and rejected**. |
| `secret.annotations` | map string→string | `{}` | Same limits and reserved keys as `secret.labels`. |
| `data` | map string→string | `{}` | Literal values copied verbatim. At most 64 entries, values at most 65 536 characters. Keys must be valid Secret key names (below) and must not collide with a generated key. |
| `fields` | list | — | 1–32 entries; `name` is the list key. See [`fields`](#fields). |
| `sshKeyPair` | object | — | See [`sshKeyPair`](#sshkeypair). `sshKeyPair: {}` is valid (ed25519). |
| `basicAuth` | object | — | See [`basicAuth`](#basicauth). |

**Secret key names** (`data` keys, `fields[].name` and every `keys.*` value)
match `[-._a-zA-Z0-9]+`, are at most 253 characters, and are neither `.` nor
start with `..`. Keys must be distinct within one Secret.

### `deletionPolicy`

| Policy | The Secret has | When the `SecretSpec` is deleted |
|---|---|---|
| `Delete` (default) | a controller `ownerReference` to the `SecretSpec` | Kubernetes garbage-collects the Secret. |
| `Retain` | **no** `ownerReference` | The Secret is left untouched. |

There is no finalizer in either mode, so the operator being down never blocks
deleting a namespace. Changing the policy on a live object adds or removes the
`ownerReference` at the next reconcile. A `SecretSpec` that is created while a
**retained** Secret with the same name exists *adopts* it and keeps every value
whose definition is unchanged; see [Security](security.md#adoption). Guidance on
choosing the policy is in [Operations](operations.md#deletionpolicy-and-reinstalls).

### `fields`

Each entry creates one Secret key, in one of two mutually exclusive modes.

**Mode A — characters (default)**

| Field | Default | Rules |
|---|---|---|
| `name` | required | The Secret key; unique within `fields`. |
| `length` | `32` | 1–1024 random characters. Does not count `prefix`/`suffix`. |
| `charset` | `alphanumeric` | `alphanumeric` (a–z, A–Z, 0–9), `ascii` (alphanumeric plus the 32 ASCII punctuation characters), `hex` (0–9, a–f) or `custom`. |
| `alphabet` | — | Required if and only if `charset: custom`. 1–256 printable ASCII characters, no spaces; at least 2 distinct characters must remain after `exclude`. Duplicates are ignored. |
| `exclude` | none | Characters removed from the effective alphabet, e.g. `/@" `. Printable ASCII (a space is allowed), at most 256 characters. |
| `require` | none | Minimum number of characters per class: `lower`, `upper`, `digits`, `symbols`, each 0–1024. The sum must not exceed the effective `length`. Each required class must still have a character in the effective alphabet. |

**Mode B — random bytes**

| Field | Default | Rules |
|---|---|---|
| `name` | required | As above. |
| `bytes` | — | 1–1024 random bytes. Excludes `length`, `charset`, `alphabet`, `exclude` and `require`. |
| `encoding` | — | Required with `bytes` and only allowed with it: `hex`, `base64`, `base64url` or `base32` (standard alphabets, padded). |

**Both modes:** `prefix` and `suffix` (each at most 64 printable ASCII
characters, no spaces) are prepended and appended verbatim.

The `symbols` class is the 32 characters ``!"#$%&'()*+,-./:;<=>?@[\]^_`{|}~``.

Values come from the operating system's CSPRNG, sampled without modulo bias; the
required characters are placed first and the result is shuffled.

**Low entropy** is a warning, never a rejection: when a (re)generated value has
less than 128 bits of entropy (`length × log2(alphabet size)` in mode A,
`bytes × 8` in mode B), the operator emits a Warning Event `LowEntropy`.

Recipes:

| Purpose | Field |
|---|---|
| Fernet key | `bytes: 32`, `encoding: base64url` |
| Garage `rpc_secret` | `bytes: 32`, `encoding: hex` |
| S3-style access key id | `prefix: GK`, `length: 24`, `charset: hex` |
| Amazon RDS password | `charset: ascii`, `exclude: '/@" '`, `require: {lower: 1, upper: 1, digits: 1, symbols: 1}` |

### `sshKeyPair`

| Field | Default | Rules |
|---|---|---|
| `algorithm` | `ed25519` | `ed25519`, `rsa` or `ecdsa`. |
| `rsaBits` | `4096` | `2048`, `3072` or `4096`; only with `algorithm: rsa`. |
| `ecdsaCurve` | `P-256` | `P-256`, `P-384` or `P-521`; only with `algorithm: ecdsa`. |
| `comment` | `<name>@<namespace>` | Printable ASCII, at most 256 characters. `""` means no comment. |
| `keys.privateKey` | `ssh-privatekey` | Secret key for the private key. |
| `keys.publicKey` | `ssh-publickey` | Secret key for the public key. |

The private key is unencrypted OpenSSH format
(`-----BEGIN OPENSSH PRIVATE KEY-----`); the public key is one `authorized_keys`
line including the comment. Changing `comment` rewrites only the public-key line;
renaming `keys.*` moves the value without regenerating it. The two key names
must differ.

### `basicAuth`

| Field | Default | Rules |
|---|---|---|
| `username` | required | Stored verbatim; no implicit default. 1–253 visible ASCII characters, without `:` and without spaces. |
| `password` | `{}` (32 alphanumeric) | Same options as `fields` mode A without `name`: `length`, `charset`, `alphabet`, `exclude`, `require`, `prefix`, `suffix`. |
| `keys.username` | `username` | Secret key for the user name. |
| `keys.password` | `password` | Secret key for the password. |
| `keys.htpasswd` | `auth` | Secret key for the htpasswd line. Use `users` for Traefik's `BasicAuth` middleware. |

The three key names must be distinct. The htpasswd value is
`<username>:<bcrypt hash>` using bcrypt `$2y$` with cost 12. bcrypt is salted, so
the line is recomputed only when the username or the password changes (never on
a plain reconcile). Changing `username` keeps the password and recomputes only
the htpasswd line. **bcrypt only uses the first 72 bytes of a password**; a
(re)generated password longer than that (`prefix` + `length` + `suffix`) triggers
the Warning Event `PasswordTruncated`. The default of 32 is far below the limit.

## Validation and the frozen messages

A bad manifest is rejected by the API server when you apply it (`kubectl apply`,
`helm install`), by the CRD schema and CEL rules. The messages below are
**frozen**: they are part of the API, and you may match on them. Other
violations (type, range, pattern) produce the API server's standard schema
messages.

| Where | Message |
|---|---|
| `spec` | `exactly one of fields, sshKeyPair or basicAuth must be set` |
| `fields[]` | `bytes cannot be combined with length, charset, alphabet, exclude or require` |
| `fields[]` | `encoding is required with bytes and only allowed with bytes` |
| `fields[]`, `basicAuth.password` | `alphabet is required when charset is custom and only allowed then` |
| `fields[]`, `basicAuth.password` | `the sum of require counts must not exceed length (default 32)` |
| `rotation.interval` | `rotation.interval must be between 1h and 3650d` |
| `sshKeyPair` | `rsaBits is only allowed with algorithm rsa` |
| `sshKeyPair` | `ecdsaCurve is only allowed with algorithm ecdsa` |
| `sshKeyPair.keys`, `basicAuth.keys` | `secret key names in keys must be distinct` |
| `spec` | `data keys must not collide with generated keys` |
| `data` | `data keys must be valid Secret key names: [-._a-zA-Z0-9]+, at most 253 characters, not '.' and not starting with '..'` |
| `secret.labels`, `secret.annotations` | `label and annotation keys must be at most 253 characters; keys under secretspec.jabbas.eu/ and app.kubernetes.io/managed-by are reserved` |
| root | `metadata.name must be at most 63 characters because it is used as a label value` |
| `secret.type` | `secret.type must be Opaque, kubernetes.io/basic-auth, kubernetes.io/ssh-auth or a custom type outside kubernetes.io/ (bootstrap tokens are not allowed)` |
| `spec` | `type kubernetes.io/basic-auth requires basicAuth with keys.username "username" and keys.password "password"` |
| `spec` | `type kubernetes.io/ssh-auth requires sshKeyPair with keys.privateKey "ssh-privatekey"` |
| `secret.type` | `secret.type is immutable` |

Two rules cannot be checked at admission and are reported at runtime as
`Ready=False`, reason `InvalidSpec`:

- `alphabet must contain at least 2 characters after exclude`;
- `no <class> characters left in the alphabet`, when a `require` class has no
  character left after `exclude`.

The message is prefixed with the field path, e.g.
`spec.fields[0].exclude: alphabet must contain at least 2 characters after exclude`.

## Status

| Field | Meaning |
|---|---|
| `observedGeneration` | The `metadata.generation` last reconciled. |
| `kind` | `Fields`, `SSHKeyPair` or `BasicAuth`. |
| `secretName` | Name of the generated Secret. |
| `lastGeneratedTime` | Last time any value was (re)generated. |
| `lastRotationTime` | Last scheduled rotation. |
| `nextRotationTime` | Next scheduled rotation; absent without `rotation`. |
| `conditions` | Standard conditions; currently only `Ready`. |

`Ready` reasons (the events and fixes are in
[Operations](operations.md#troubleshooting)):

| Status | Reason | Meaning |
|---|---|---|
| `True` | `Generated` | The Secret matches the spec. |
| `False` | `Conflict` | A Secret with the target name exists and is not managed by this `SecretSpec` (or has a different type). Nothing is written. |
| `False` | `InvalidSpec` | Runtime validation failed; the message says what is wrong. |
| `False` | `ApplyFailed` | Writing the Secret failed. Retried every 30 seconds. |
| `False` | `GenerationFailed` | Generating a value failed inside the operator (SSH library, bcrypt). Retried every 30 seconds. |
| `False` | `WaitingForGarbageCollection` | The Secret still belongs to a deleted `SecretSpec` of the same name; the operator waits (re-checking every 5 seconds) instead of adopting it. |

## The generated Secret

- **Name and namespace:** the same as the `SecretSpec`.
- **Type:** `spec.secret.type` (default `Opaque`).
- **Data:** the `data` literals plus the generated keys.
- **Labels:**
  - `app.kubernetes.io/managed-by: secretspec-operator`
  - `secretspec.jabbas.eu/spec-name: <SecretSpec name>`
  - plus `spec.secret.labels`
- **Annotations:**

  | Annotation | Content |
  |---|---|
  | `secretspec.jabbas.eu/fingerprints` | JSON map from generation slot (`field/<name>`, `ssh/keypair`, `basic-auth/password`, `basic-auth/htpasswd`) to the SHA-256 of the slot's resolved parameters. Decides whether a value is still valid. It is derived from the parameters, not from any generated value (the htpasswd slot uses a keyed MAC so that it cannot be used to test password guesses); see [Fingerprint format](fingerprints.md). |
  | `secretspec.jabbas.eu/generated-at` | RFC 3339 time of the last full generation; the rotation anchor. |
  | `secretspec.jabbas.eu/spec-uid` | UID of the `SecretSpec` that currently owns the Secret. |
  | `secretspec.jabbas.eu/keys` | JSON map from role (`ssh/private-key`, `ssh/public-key`, `basic-auth/username`, `basic-auth/password`, `basic-auth/htpasswd`) to the Secret key that holds it; absent for `fields`. Lets a `keys.*` rename move a value. |
  | plus | `spec.secret.annotations` |

  Do not edit these annotations by hand; the operator reads them back.
- **Owner reference:** a controller reference to the `SecretSpec` for
  `deletionPolicy: Delete`; none for `Retain`.
- **Writes:** server-side apply with field manager `secretspec-operator`. Keys
  that the spec no longer produces are removed; keys added to the Secret by
  other field managers are left alone.

The operator never adopts, overwrites or deletes a Secret that lacks the
`app.kubernetes.io/managed-by: secretspec-operator` label.

## Behaviour table

What the operator does in each situation. "Stable" means the existing values are
kept.

| # | Situation | Result |
|---|---|---|
| 1 | New `SecretSpec` | The Secret is created with generated values. |
| 2 | The definition of one generated key changes (e.g. `length` 32 → 40) | A new value for that key only. |
| 3 | A generated key is added to / removed from the spec | Only the new key is generated / the key is removed. |
| 4 | A literal in `data` changes | The literal is updated; generated values are stable. |
| 5 | `deletionPolicy`, `secret.labels`, `secret.annotations`, the SSH `comment` or `basicAuth.username` changes | Nothing is regenerated. A new comment rewrites the public-key line; a new username recomputes the htpasswd line with the same password. |
| 6 | Labels or annotations of the `SecretSpec` itself change (e.g. a chart-version label bump) | Nothing happens. Only changes to `spec` count. |
| 7 | Operator restart or upgrade, re-apply of an identical manifest, cache loss, leader change | Nothing happens. |
| 8 | A key is deleted from the Secret, or emptied | That key is regenerated. |
| 9 | The Secret is deleted | Everything is regenerated. This is **manual rotation**. Literals are unchanged. |
| 10 | `rotation.interval` has elapsed since `generated-at` | All generated keys (the SSH key pair included) are regenerated at once and the clock restarts. Missed intervals coalesce into one rotation. |
| 11 | Rotation is enabled on an existing Secret, or no rotation is scheduled yet (`status.nextRotationTime` is absent: rotation newly enabled, status lost, or a new `SecretSpec` adopting a retained Secret) | No immediate rotation. The schedule starts when the operator first observes it: the first rotation falls one interval after that moment (or after `generated-at`, if that is not yet due), without a `Rotated` Event. |
| 12 | `rotation` is removed | The schedule is disabled; values are stable. |
| 13 | The `SecretSpec` is deleted | `Delete`: the Secret is garbage-collected. `Retain`: the Secret stays. |
| 14 | A `SecretSpec` is re-created while a retained Secret exists | The Secret is adopted; matching values are kept; no immediate rotation (see row 11). |
| 15 | A Secret with that name exists and is not managed by the operator | `Conflict`; nothing is written. |
| 16 | Fingerprint metadata is lost (annotation missing, not JSON, or an entry invalid) | Every present value is kept, the fingerprints are re-stamped from the spec, and a Warning `FingerprintsReset` is emitted. Lost metadata never rotates. |
| 17 | `keys.*` is renamed | The value moves to the new key name; nothing is regenerated. |
| 18 | The retained Secret's `type` differs from `secret.type` | `Conflict`: the message tells you to delete the Secret; nothing is written. |
| 19 | The `generated-at` annotation cannot be parsed | The anchor is reset to now, no rotation happens, and a Warning `AnchorReset` is emitted. |

There is **no** `regenerate` annotation and no `forceRegenerate` field: a
rotation request committed to git would be replayed forever. To rotate manually,
delete the Secret (or one key); see [Operations](operations.md#manual-rotation).
