# SecretSpec Operator — Design

| | |
|---|---|
| **Status** | Approved by owner (2026-10-18) |
| **Date** | 2026-10-18 |
| **API** | `secretspec.jabbas.eu/v1alpha1` |
| **Repository** | `github.com/jabbas/secretspec-operator` |

**Conventions.** The key words MUST, MUST NOT, SHOULD, SHOULD NOT and MAY are normative (RFC 2119 sense).
Every decision carries its origin tag:

- **[owner]** — decided by the project owner.
- **[architect]** — architect's call; the owner may override it during review.
- **[architect assumption]** — a fact the architect assumed because no information was available; the owner should confirm or correct it.

Section 16 lists what is deliberately left to the builder.

---

## 1. Intent

- A Kubernetes operator, written in Rust, that creates Kubernetes `Secret`s with generated values (passwords, tokens, random bytes, SSH key pairs, basic-auth credentials) from a custom resource `SecretSpec`. **[owner]**
- A new, independent, public project. Not a fork. Inspired by earlier secret-generator tools. People using those tools may move to it if they want. No compatibility or migration machinery is provided. **[owner]**
- Why it exists: earlier secret-generator tools are effectively unmaintained. Their semantics lose or rotate credentials unexpectedly:
  - values regenerated on every controller restart;
  - Secrets garbage-collected together with their CR, with no way to keep them;
  - every Secret in the cluster cached (memory use, blast radius);
  - unsafe defaults, such as insecure regeneration turned on by default and truncated hex lengths that give half the entropy;
  - CRD upgrades skipped by Helm;
  - single-arch images;
  - broken release pipelines.
- Users and roles **[owner]**:
  - **Platform team** (3 people) installs the operator once per cluster through infrastructure-as-code and owns its operation.
  - **Application developers** declare `SecretSpec`s inside their own Helm charts. They do not know how the operator works internally. In practice the hardest part is explaining to developers how to define things. The API MUST therefore be self-explanatory and copy-paste friendly.
- Target environments: AWS EKS (arm64/Graviton nodes may exist), Talos on bare metal, and a Talos home lab managed by Flux. **[owner]**

## 2. Success criteria

- Long-term failure means the operator does not meet the owner's expectations in his home cluster and in the work clusters he operates. **[owner]**
- v0.1.0 is ready to publish when the CRD exists, Secrets are generated from it, and tests pass. **[owner]**
- Architect's interpretation of "ready" **[architect]**. v0.1.0 is done when all of the following hold:
  1. the complete v1alpha1 API in this document is implemented;
  2. the unit and e2e suites are green in CI;
  3. the release pipeline publishes a signed multi-arch image and an OCI chart;
  4. the home-lab test procedure (§12) passes against a release candidate (`v0.1.0-rc.N`);
  5. the README and `examples/` are present.

  The final `v0.1.0` tag is created only after a release candidate has passed §12. The work itself starts with the API freeze milestone (§4.5).

## 3. Goals, non-goals, anti-goals

### 3.1 Goals

- **Safe by default.** A value is generated once. It MUST NOT change unless one of these happens: its spec changes, the Secret or the key is deleted, or a configured rotation is due.
- **Self-explaining API.** The simplest case takes a few lines.
- **Mistakes surface where developers look.** A bad manifest is rejected at `helm install` / `kubectl apply` time by the CRD schema and CEL validation. Anything only detectable at runtime appears in `SecretSpec` status conditions and in Events, never only in operator logs.
- **Least privilege and a small footprint.** The operator caches only the Secrets it manages.

### 3.2 Non-goals for v0.1 (may come later)

- Templates and derived keys (connection strings, the same value under several keys). The API MUST NOT make adding them later impossible. No field name needs to be reserved for them. **[owner]**
- Migration tooling or migration docs from other tools. Deferred to later. **[owner]**
- Restarting or notifying the consumers of a Secret. Consumers must reload on change. The docs MAY mention generic reloader tools. **[owner]**
- Keeping the previous value after rotation. **[owner]**
- Scoping namespaces by label selector. v0.1 supports only an explicit namespace list. **[architect]**
- Overriding the Secret name. The Secret name always equals the SecretSpec name. **[architect]**
- Admission webhooks of any kind. **[architect]**

### 3.3 Anti-goals (a failure even if it "works")

- Annotation-driven generation on plain `Secret` objects. This is never supported. The only input is `SecretSpec`, and `Secret` is the output. **[owner]**
- Deterministic or seed-derived secrets. **[architect]**
- Any change of generated values caused by any of the following **[architect]**:
  - operator restart or upgrade;
  - re-apply of an identical manifest (Helm, Flux, Argo);
  - label or annotation changes on the SecretSpec;
  - cache loss;
  - leader change.
- Touching any Secret the operator did not create. **[architect]**
- Operator logic that knows about Helm, Flux or any other deployment tool. **[owner]**
- Secret values appearing in logs, Events, status, metrics or error messages. **[architect]**

## 4. API — `SecretSpec` (`secretspec.jabbas.eu/v1alpha1`)

| Property | Value | Origin |
|---|---|---|
| Group | `secretspec.jabbas.eu` | [owner] |
| Kind | `SecretSpec` | [owner] |
| Version | `v1alpha1` | [architect] |
| Scope | Namespaced | [owner] |
| Plural / short name | `secretspecs` / `ss` | [architect] |
| Subresources | `status` | [architect] |

Versioning **[architect]**:

- The API starts at `v1alpha1`.
- It moves to `v1beta1` or `v1` only after its shape has survived use in the owner's clusters.
- Breaking changes are allowed before then, but each one MUST be called out in the release notes.

One SecretSpec describes exactly one kind of secret:

- Exactly one of `fields`, `sshKeyPair`, `basicAuth` MUST be set. This is enforced by CEL. **[owner]**
- An application that needs a password and an SSH key uses two SecretSpecs, which produce two Secrets. **[owner]**
- The block-per-kind shape is an **[architect]** decision. The owner's first sketch had `algorithm` directly under `spec`. It was moved into `sshKeyPair` so that the CEL "exactly one of" rule is simple and `kubectl explain` output is clear.

### 4.0 Example manifests

```yaml
apiVersion: secretspec.jabbas.eu/v1alpha1
kind: SecretSpec
metadata:
  name: app-db              # Secret gets the same name, same namespace
spec:
  deletionPolicy: Retain    # Delete (default) | Retain
  rotation:
    interval: 90d           # optional
  secret:                   # optional metadata for the generated Secret
    type: Opaque
    labels: {}
    annotations: {}
  data:                     # literal values copied verbatim
    username: app
  fields:
    - name: password        # 32 random alphanumeric chars by default
---
# apiVersion / kind / metadata omitted for brevity
spec:
  data:
    username: john
  sshKeyPair: {}            # ed25519, comment "<name>@<namespace>"
---
# apiVersion / kind / metadata omitted for brevity
spec:
  basicAuth:
    username: admin
    password: { length: 32 }
    keys: { htpasswd: users }   # default "auth"; "users" for Traefik
```

### 4.1 Top-level `spec` fields

| Field | Type | Default | Rules | Origin |
|---|---|---|---|---|
| `deletionPolicy` | enum `Delete` \| `Retain` | `Delete` | See §6. | [owner] |
| `rotation.interval` | string, pattern `^[1-9][0-9]*(m\|h\|d)$` | none (no rotation) | Minimum `1h`, maximum `3650d`. | [owner: feature; architect: format/limits] |
| `secret.type` | string, 1–253 characters | `Opaque` | Allowed (CEL): `Opaque`, `kubernetes.io/basic-auth`, `kubernetes.io/ssh-auth`, or any custom type that does not start with `kubernetes.io/` and is not `bootstrap.kubernetes.io/token`. This keeps the operator from minting service-account or bootstrap tokens and other built-in types whose contracts it does not implement. Frozen message: "secret.type must be Opaque, kubernetes.io/basic-auth, kubernetes.io/ssh-auth or a custom type outside kubernetes.io/ (bootstrap tokens are not allowed)". Immutable after creation (CEL `self == oldSelf`), because the Kubernetes Secret type is immutable. `kubernetes.io/basic-auth` is allowed only with `basicAuth` where `keys.username` = `username` and `keys.password` = `password`. `kubernetes.io/ssh-auth` is allowed only with `sshKeyPair` where `keys.privateKey` = `ssh-privatekey`. | [architect] |
| `secret.labels`, `secret.annotations` | map string→string | `{}` | Keys under the `secretspec.jabbas.eu/` prefix are reserved and rejected (CEL). `app.kubernetes.io/managed-by` is rejected. | [architect] |
| `data` | map string→string | `{}` | Keys MUST be valid Secret key names: `^[-._a-zA-Z0-9]+$`, at most 253 characters, not `.` and not starting with `..` (the same rule applies to `fields[].name` and every `keys.*` value). Keys MUST NOT collide with any generated key. This is checked by CEL where it can be expressed; otherwise at runtime as `InvalidSpec`. | [owner: literals; architect: rules] |
| `fields` \| `sshKeyPair` \| `basicAuth` | see §4.2–§4.4 | — | Exactly one MUST be set (CEL). | [owner] |

**Name length [architect]:** `metadata.name` MUST be at most 63 characters (root CEL rule `size(self.metadata.name) <= 63`, re-checked by `resolve()`), because the name is copied into the `secretspec.jabbas.eu/spec-name` label and label values are limited to 63 characters. Frozen message: "metadata.name must be at most 63 characters because it is used as a label value".

**Label/annotation keys:** at most 253 characters; the reserved-key rule and the length limit share one frozen message: "label and annotation keys must be at most 253 characters; keys under secretspec.jabbas.eu/ and app.kubernetes.io/managed-by are reserved".

### 4.2 `fields[]` — generated strings and tokens

The `name` + `length` shape is **[owner]**. All other options are **[architect]**.

`fields` is a list with `x-kubernetes-list-type: map` and `x-kubernetes-list-map-keys: [name]`. Each entry uses one of two mutually exclusive modes.

**Mode A — characters (default)**

| Field | Default | Notes |
|---|---|---|
| `name` | required | The Secret key. MUST match the key regex and be unique. |
| `length` | `32` | Number of random characters, 1–1024. Does NOT include `prefix` / `suffix`. |
| `charset` | `alphanumeric` | `alphanumeric` (a-z A-Z 0-9) \| `ascii` (alphanumeric + ASCII punctuation) \| `hex` (0-9a-f) \| `custom` |
| `alphabet` | — | Required if and only if `charset: custom`. At least 2 distinct printable ASCII characters (no space), at most 256 characters; duplicates are ignored. |
| `exclude` | `""` | Characters removed from the effective alphabet (e.g. `/@" ` for AWS RDS). |
| `require` | none | `{lower, upper, digits, symbols}`: minimum counts per class (integers ≥ 0). The sum MUST be ≤ `length`. |

**Mode B — random bytes**

| Field | Default | Notes |
|---|---|---|
| `name` | required | As in mode A. |
| `bytes` | — | 1–1024 random bytes. |
| `encoding` | — | Required with `bytes`. One of `hex` \| `base64` \| `base64url` \| `base32`. Standard alphabets, padded. |

**Both modes:** `prefix` and `suffix` are optional, each ≤ 64 printable ASCII characters. They are prepended/appended verbatim.

**CEL rules (rejected at admission):**

- `bytes` excludes `length`, `charset`, `alphabet`, `exclude` and `require`.
- `has(self.encoding) == has(self.bytes)`.
- `charset == 'custom'` if and only if `has(self.alphabet)`.
- The sum of the `require` counts MUST be ≤ the effective `length` (32 when `length` is unset).

**Runtime validation (reported as `Ready=False`, reason `InvalidSpec`):**

- The effective alphabet after `exclude` MUST contain at least 2 characters.
- Every class with `require > 0` MUST have at least one character left in the effective alphabet.

**Character classes:**

| Class | Characters |
|---|---|
| `lower` | `a`–`z` |
| `upper` | `A`–`Z` |
| `digits` | `0`–`9` |
| `symbols` | the 32 ASCII punctuation characters ``!"#$%&'()*+,-./:;<=>?@[\]^_`{\|}~`` |

**Generation algorithm** **[architect]** (normative):

1. Use only the OS CSPRNG.
2. Sample uniformly without bias, using rejection sampling. Modulo reduction MUST NOT be used.
3. For each class with `require > 0`, draw that many characters from the class, limited to the effective alphabet.
4. Fill the remaining `length − Σrequire` positions from the full effective alphabet.
5. Shuffle the result with Fisher–Yates using the same CSPRNG.
6. Prepend `prefix` and append `suffix`.

There is deliberately no `allowRepeat`-style option. Forbidding repeated characters only lowers entropy.

**Entropy warning** **[architect]:**

- Estimated entropy is `length · log2(|effective alphabet|)` in mode A and `bytes · 8` in mode B.
- If the estimate is below 128 bits, emit Event `Warning LowEntropy`.
- Low entropy is never a reason to reject the spec.

**Documented recipes:**

| Purpose | Field spec |
|---|---|
| Fernet key | `bytes: 32, encoding: base64url` |
| Garage `rpc_secret` | `bytes: 32, encoding: hex` |
| S3-style access key id | `prefix: GK, length: 24, charset: hex` |

### 4.3 `sshKeyPair`

| Field | Default | Notes | Origin |
|---|---|---|---|
| `algorithm` | `ed25519` | `ed25519` \| `rsa` \| `ecdsa` | [architect default; owner asked whether a default exists] |
| `rsaBits` | `4096` | `2048` \| `3072` \| `4096`. Allowed only with `rsa` (CEL). | [architect] |
| `ecdsaCurve` | `P-256` | `P-256` \| `P-384` \| `P-521`. Allowed only with `ecdsa` (CEL). | [architect] |
| `comment` | `<SecretSpec name>@<namespace>` | Optional. `""` means no comment. | [owner] |
| `keys.privateKey` | `ssh-privatekey` | Secret key name. | [architect] |
| `keys.publicKey` | `ssh-publickey` | Secret key name. | [architect] |

**Output and semantics** **[architect]:**

- The private key is in OpenSSH format (`-----BEGIN OPENSSH PRIVATE KEY-----`) and is unencrypted.
- The public key is a single `authorized_keys` line, including the comment.
- The comment is metadata and is excluded from the fingerprint. Changing it rewrites only the public-key line; the key material stays the same.
- Renaming `keys.privateKey` or `keys.publicKey` moves the existing value to the new key. It does not regenerate.

### 4.4 `basicAuth`

| Field | Default | Notes | Origin |
|---|---|---|---|
| `username` | required | Literal value. There is no implicit `admin`. | [architect] |
| `password` | `{length: 32, charset: alphanumeric}` | Same options as `fields` mode A, without `name`. | [architect] |
| `keys.username` | `username` | Secret key name. | [architect] |
| `keys.password` | `password` | Secret key name. | [architect] |
| `keys.htpasswd` | `auth` | Secret key name. Use `users` for Traefik's BasicAuth middleware. | [architect] |

**htpasswd** **[architect]:**

- The value is `<username>:<bcrypt hash>`, using bcrypt `$2y$` with cost 12.
- bcrypt is salted, so the hash MUST be recomputed only when the username or the password changes (tracked via the fingerprint). It MUST NOT be recomputed on every reconcile.
- Changing `username` keeps the password and recomputes only the htpasswd line.

### 4.5 Defaults — stability rule **[architect]**

- Defaults are part of the API-version contract. They MUST NOT change within an apiVersion.
- If the OpenAPI schema can express a default without ambiguity, the CRD schema MUST declare it, so the API server writes it into stored objects. Examples: `deletionPolicy`, `sshKeyPair.algorithm`, `keys.*`.
- If the schema cannot express a default, the operator resolves it from frozen constants. These cases are:
  - mode-dependent defaults such as `fields[].length` and `charset`, which MUST NOT be materialised when `bytes` is used;
  - `rsaBits` and `ecdsaCurve`, which depend on `algorithm`;
  - `comment`, which depends on name and namespace.
- Fingerprints are always computed from fully resolved parameters.
- A golden test (§11) MUST guard that fingerprints of fixed inputs never change across releases. Otherwise an operator upgrade would rotate every credential in every cluster.

#### API freeze gate **[architect]**

The defaults and the fingerprint format become permanent for `v1alpha1`. Changing either one later would rotate every credential. They are therefore settled before anything is built on top of them.

- The FIRST milestone of the implementation plan is:
  1. the CRD types;
  2. the CEL rules;
  3. defaults resolution;
  4. the fingerprint function;
  5. the golden fingerprint tests;
  6. the generated CRD YAML.
- The owner reviews and signs off on that milestone.
- Work on the controller, the chart and the release pipeline MUST NOT start before that sign-off.

### 4.6 Status **[architect]**

| Field | Meaning |
|---|---|
| `observedGeneration` | `metadata.generation` last reconciled. |
| `kind` | `Fields` \| `SSHKeyPair` \| `BasicAuth` (used by the printer column). |
| `secretName` | Name of the generated Secret. |
| `lastGeneratedTime` | Last time any value was (re)generated. |
| `lastRotationTime` | Last scheduled rotation. |
| `nextRotationTime` | Next scheduled rotation (absent without `rotation`). |
| `conditions[]` | Standard `metav1.Condition` list. |

The `Ready` condition:

| Status | Reason | Meaning |
|---|---|---|
| `True` | `Generated` | The Secret matches the spec. |
| `False` | `Conflict` | A Secret with the target name exists and is not managed by the operator. |
| `False` | `InvalidSpec` | Runtime validation failed. The message says what is wrong. |
| `False` | `ApplyFailed` | The write to the API server failed. |

Printer columns: `Ready`, `Kind`, `Next Rotation`, `Age`.

## 5. Generated Secret **[architect]**

- **Name:** the SecretSpec name, in the same namespace.
- **Labels:**
  - `app.kubernetes.io/managed-by: secretspec-operator`
  - `secretspec.jabbas.eu/spec-name: <name>`
  - plus `spec.secret.labels`
- **Annotations:**
  - `secretspec.jabbas.eu/fingerprints`: a JSON map from generation *slot* to the SHA-256 hex of that slot's resolved generation parameters. Slots are stable logical names independent of Secret key names (e.g. `field/password`, `ssh/keypair`, `basic-auth/password`, `basic-auth/htpasswd`), so renaming `keys.*` can move a value without regenerating it. The `basic-auth/htpasswd` fingerprint never contains a plain hash of the password: it covers `HMAC-SHA256(key = stored htpasswd line, message = password)`, because annotations are visible to people who cannot read Secret data. Readers are tolerant: unknown slots are ignored but preserved by writers, and an invalid entry invalidates only its own slot. The exact canonical format and the reading rules are frozen in `docs/fingerprints.md` (API freeze milestone).
  - `secretspec.jabbas.eu/generated-at`: RFC 3339 time of the last full generation. This is the rotation anchor.
  - `secretspec.jabbas.eu/spec-uid`: the UID of the SecretSpec that currently owns the Secret.
  - `secretspec.jabbas.eu/keys`: JSON map from role (`ssh/private-key`, `ssh/public-key`, `basic-auth/username`, `basic-auth/password`, `basic-auth/htpasswd`) to the Secret key currently holding it; absent for `fields`. Used only to move values when `keys.*` is renamed; a missing or unreadable map means "the keys named in the spec".
  - plus `spec.secret.annotations`
- **Data:** the literals from `data` plus the generated keys.
- **Writes:** the operator owns its keys through server-side apply with field manager `secretspec-operator` and `force: true`.
  - Keys the operator no longer applies are removed by SSA.
  - Keys added by other field managers are left alone.
- **Conflict:** if a Secret with the target name exists and lacks the managed-by label, the operator:
  - writes nothing;
  - sets `Ready=False`, reason `Conflict`;
  - emits a Warning Event.

  The operator MUST NEVER adopt unlabelled Secrets.

## 6. Lifecycle: `deletionPolicy` and adoption

The policy and its semantics are **[owner]**. The mechanics are **[architect]**.

| Policy | Mechanism | On SecretSpec deletion |
|---|---|---|
| `Delete` (default) | The Secret has a controller ownerReference to the SecretSpec (`controller: true`, `blockOwnerDeletion: true`). No finalizer. | Kubernetes GC deletes the Secret. |
| `Retain` | The Secret has NO ownerReference, only the operator's labels and annotations. No finalizer: finalizers block namespace deletion when the operator is down. | The Secret is left untouched. |

- **Policy change on a live object:** the next reconcile adds or removes the ownerReference.
- **Adoption.** It applies when a SecretSpec is (re)created and a Secret with the same name exists that meets all of these conditions:
  - it carries the operator's labels;
  - its `spec-name` equals `<name>`;
  - it has no live controller owner.

  The operator then:
  1. keeps every value whose stored fingerprint matches the resolved parameters;
  2. regenerates the rest;
  3. sets the ownerReference according to the policy;
  4. updates `spec-uid`;
  5. emits the `Adopted` Event.

  Adoption never rotates on its own: the new SecretSpec has no `nextRotationTime` yet, so a past-due `generated-at` restarts the rotation clock at now (§7.2 row 11).

  This is how a credential survives uninstall and reinstall under `Retain`.
- **Pending GC:** if the Secret still has an ownerReference to a different SecretSpec UID that has been deleted, the operator MUST requeue and wait. It MUST NOT adopt yet. This applies only to a controller ownerReference whose `apiVersion` group is `secretspec.jabbas.eu` and whose kind is `SecretSpec`; a controller ownerReference to anything else is a `Conflict` (row 15). **[architect]**
- **Security note (for the docs):** adoption keys off labels. Anyone who can create Secrets in the namespace could craft one to be adopted. They already have write access to Secrets there, so this is not a privilege escalation.
- **Docs MUST warn:** with `Delete`, an uninstall/reinstall, a HelmRelease remediation or a GitOps prune produces new values. Examples for credentials that applications persist (e.g. at database init) MUST use `Retain`.

## 7. Reconciliation contract

This section is the heart of the design.

### 7.1 Pure planner

The decision logic MUST be a pure function with no I/O:

```rust
fn plan(
    spec: &SecretSpec,
    spec_metadata: &ObjectMeta,
    existing_secret: Option<&Secret>,
    now: DateTime<Utc>,
) -> Plan; // Plan { desired_secret, status, events, requeue_after }
```

The controller only does the following:

1. Read state.
2. Call `plan`.
3. Write via SSA if `desired != existing`. The API call MUST be skipped when they are equal.
4. Patch status.
5. Emit Events.

### 7.2 Behaviour table

Each row MUST be covered by exactly one table-driven unit test, and vice versa.

| # | Situation | Result | Origin |
|---|---|---|---|
| 1 | New SecretSpec | Secret created with generated values. | [owner] |
| 2 | Generation parameters of one key changed (e.g. `length` 32→40) | New value for that key only. | [owner: change → new; architect: per-key] |
| 3 | Generated key added / removed in the spec | Generate only the new key / remove the key. | [architect] |
| 4 | Literal in `data` changed | Literal updated; generated values kept. | [architect] |
| 5 | `deletionPolicy`, `secret.labels`/`annotations`, SSH `comment` or `basicAuth.username` changed | No regeneration. A comment change rewrites the public-key line. A username change recomputes htpasswd. | [architect] |
| 6 | SecretSpec labels/annotations changed (e.g. a Helm chart-version label bump) | Nothing. Only `metadata.generation` changes count. | [architect] |
| 7 | Operator restart / upgrade, re-apply of an identical manifest, cache loss, leader change | Nothing. | [architect] |
| 8 | A key deleted from the Secret (or emptied) | That key regenerated. | [architect] |
| 9 | Secret deleted | Full regeneration, which is the **manual rotation** mechanism. Literals unchanged. | [owner] |
| 10 | `rotation.interval` elapsed since `generated-at` | All generated keys regenerated (including the SSH key pair); anchor = now. Missed intervals coalesce into one rotation. | [owner: feature; architect: mechanics] |
| 11 | Rotation enabled on an existing Secret, or no rotation scheduled yet (`status.nextRotationTime` absent: rotation newly enabled or re-enabled, status lost, or a new SecretSpec adopting a retained Secret) | No immediate rotation: rotation is scheduled from the moment the operator first observes it. The anchor is the existing `generated-at` while it is not yet due; if it is absent or already past due, the anchor (`generated-at`) becomes now and `nextRotationTime` = now + interval, without a `Rotated` Event. Row 10 applies only once `nextRotationTime` is recorded. After status loss a rotation may therefore be delayed by up to one interval. | [architect] |
| 12 | `rotation` removed | Schedule disabled; values kept. | [architect] |
| 13 | SecretSpec deleted | `Delete`: Secret garbage-collected. `Retain`: Secret kept. | [owner] |
| 14 | SecretSpec recreated while a retained Secret exists | Adopted; matching values kept; no immediate rotation (§6, row 11). | [architect] |
| 15 | A Secret with that name exists and is not ours | `Conflict`; nothing written. | [architect] |
| 16 | Fingerprint metadata lost (annotation missing, not JSON, or an entry invalid) | Every present value is kept, the fingerprints are re-stamped from the spec, and a Warning `FingerprintsReset` is emitted. Lost metadata never rotates. | [architect] |
| 17 | `keys.*` renamed in the spec | The value moves to the new key name; nothing is regenerated. | [architect] |
| 18 | The retained Secret's `type` differs from the spec's `secret.type` (the field is immutable) | `Conflict` telling the user to delete the Secret; nothing written. | [architect] |
| 19 | The rotation anchor (`generated-at`) is unparseable | The anchor is reset to now, no rotation happens, and a Warning `AnchorReset` is emitted. | [architect] |

There is no `regenerate` annotation and no `forceRegenerate` field. A rotation request committed to git would be replayed forever. To rotate manually, delete the Secret. **[owner]**

### 7.3 Triggers **[architect]**

| Trigger | Detail |
|---|---|
| SecretSpec watch | Filtered to `metadata.generation` changes, so status writes MUST NOT retrigger a reconcile. |
| Secret watch | Restricted server-side by label selector `app.kubernetes.io/managed-by=secretspec-operator`. Mapped to the SecretSpec via the `secretspec.jabbas.eu/spec-name` label. This mapping works for both policies; ownerReference mapping would not work for `Retain`. |
| Rotation timer | `requeue_after` = time until `nextRotationTime`. |
| Safety resync | Periodic, default `1h`, configurable (`resyncInterval`). |

### 7.4 Events **[architect]**

| Type | Reasons |
|---|---|
| Normal | `Generated`, `Regenerated` (lists key names, never values), `Rotated`, `Adopted` |
| Warning | `Conflict`, `InvalidSpec`, `LowEntropy`, `ApplyFailed`, `FingerprintsReset`, `AnchorReset`, `PasswordTruncated` (a (re)generated `basicAuth` password longer than 72 bytes: the htpasswd hash covers only the first 72) |

## 8. Architecture **[architect]**

### 8.1 Workspace layout

The code is a Cargo workspace, edition 2024.

| Crate | Responsibility |
|---|---|
| `crates/secretspec-api` | CRD types (`kube` `CustomResource` + `KubeSchema`, with CEL rules via `#[x_kube(validation = Rule::new(...).message(...))]`); defaults resolution. |
| `crates/secretspec-core` | Pure logic: generators (charset, bytes, SSH, htpasswd), fingerprints, `plan`. No Kubernetes I/O. |
| `crates/secretspec-operator` | The binary: controller wiring, Kubernetes I/O, leader election, metrics/health server, configuration. |
| `xtask` | `cargo xtask crdgen` renders the CRD YAML into the chart. CI MUST fail if the committed YAML differs. |

### 8.2 Stack

| Concern | Choice |
|---|---|
| Kubernetes client/runtime | `kube` 4.x (`runtime`, `derive`) |
| API types | `k8s-openapi` 0.28, feature pinned to `v1_34`, the minimum supported Kubernetes minor (§10.5) |
| Schema | `schemars` 1 |
| Async runtime | `tokio` |
| Logging | `tracing` + `tracing-subscriber` (JSON) |
| Errors | `thiserror` in libraries; `anyhow` in the binary only |
| Config | `clap` (flags + env) |
| HTTP (`/metrics`, `/healthz`, `/readyz`) | `axum` |
| Metrics | `prometheus-client` |
| Crypto | RustCrypto: `rand` (OS RNG), `ssh-key`, `sha2`; plus `bcrypt` |
| Leader election | A maintained Lease-based crate, chosen by the builder |

### 8.3 Code rules

- Every crate MUST use `#![forbid(unsafe_code)]`.
- Clippy runs with `-D warnings` plus selected pedantic lints. Code is formatted with `rustfmt`.
- No `unwrap()` / `expect()` outside tests.
- Secret values MUST be wrapped in a newtype whose `Debug` and `Display` implementations redact the value. Values SHOULD be zeroized on drop where practical.

### 8.4 Namespace mode

When `watchNamespaces` is non-empty, the operator runs one namespaced watcher per namespace and merges the streams. It MUST NEVER issue a cluster-wide list in that mode.

### 8.5 Diagrams

Components and data flow:

```d2
direction: right

dev: "Developer Helm chart" {shape: person}

api: "Kubernetes API server" {
  crd: "SecretSpec CRD + CEL validation"
  ss: SecretSpec
  sec: Secret
}

op: "secretspec-operator" {
  ctrl: "Controller (kube-runtime)"
  plan: "plan() - pure"
  gen: "Generators (CSPRNG, SSH, bcrypt)"
  ctrl -> plan -> gen
}

app: "Application Pod"

dev -> api.ss: "helm install"
api.ss -> op.ctrl: "watch (generation changes)"
api.sec -> op.ctrl: "watch (label selector managed-by)"
op.ctrl -> api.sec: "server-side apply"
op.ctrl -> api.ss: "status + Events"
api.sec -> app: "volume / env"
```

One reconcile:

```d2
shape: sequence_diagram

ctrl: Controller
k8s: "API server"
plan: "plan()"

ctrl -> k8s: "get SecretSpec + Secret (cache)"
ctrl -> plan: "spec, existing, now"
plan -> ctrl: "desired Secret, status, events, requeue_after"
ctrl -> k8s: "SSA Secret (only if changed)"
ctrl -> k8s: "patch status, emit events"
```

## 9. Scope, RBAC, security

The default of all namespaces plus an optional namespace list is **[owner]**. Everything else in this section is **[architect]**.

| `watchNamespaces` | RBAC rendered by the chart | Operator behaviour |
|---|---|---|
| `[]` (default) | ClusterRole + ClusterRoleBinding | Watches all namespaces. |
| `[a, b]` | A Role + RoleBinding in each listed namespace. No cluster-wide Secret permissions. | Watches only the listed namespaces. This is a real RBAC boundary. SecretSpecs in other namespaces are invisible, and the docs MUST say so. |

Namespace label-selector scoping is rejected for v0.1. RBAC cannot be scoped by labels, so that mode would still need cluster-wide rights.

**Permissions** (complete list; nothing more MAY be granted):

| API group | Resource | Verbs | Note |
|---|---|---|---|
| `secretspec.jabbas.eu` | `secretspecs` | get, list, watch | |
| `secretspec.jabbas.eu` | `secretspecs/status` | get, patch, update | |
| `secretspec.jabbas.eu` | `secretspecs/finalizers` | update | Needed on clusters with the OwnerReferencesPermissionEnforcement admission plugin (e.g. OpenShift) when setting `blockOwnerDeletion`. |
| `""` | `secrets` | get, list, watch, create, patch, update | No `delete`. |
| `events.k8s.io` | `events` | create, patch | |
| `coordination.k8s.io` | `leases` | get, create, update | Operator namespace only. |

**Pod security:** the pod MUST comply with PSA `restricted`:

- `runAsNonRoot`;
- `readOnlyRootFilesystem`;
- drop `ALL` capabilities;
- seccomp `RuntimeDefault`;
- `allowPrivilegeEscalation: false`.

**Other:** no webhooks. Values are never logged, put in Events, or exported as metrics.

## 10. Packaging, release, licence

Everything in this section is **[architect]** unless tagged otherwise.

### 10.1 Licence

- The licence is **Apache-2.0**. **[owner]**
- The first implementation commit MUST replace the existing GPL-3.0 `LICENSE` and add a `NOTICE` file.
- Source files carry the header `// SPDX-License-Identifier: Apache-2.0`.
- The `cargo-deny` licence allow-list MUST be compatible with Apache-2.0.
- The README carries a single acknowledgement sentence for prior art. It is the only place other projects are named.

### 10.2 Image

| Property | Value |
|---|---|
| Reference | `ghcr.io/jabbas/secretspec-operator:vX.Y.Z` (or `:vX.Y.Z-rc.N` for a release candidate) |
| Platforms | `linux/amd64`, `linux/arm64` |
| Contents | Static musl binary on distroless static `nonroot` (or `scratch`) |
| Metadata | OCI labels |
| Tags | Version tags only: `vX.Y.Z` and `vX.Y.Z-rc.N`. No `latest`, including for release candidates. |

### 10.3 Helm chart

- **Location:** `charts/secretspec-operator` in this repository. Published to `oci://ghcr.io/jabbas/charts/secretspec-operator`.
- **Version:** chart version = app version.
- **CRDs:** rendered as templates. The `crds/` directory MUST NOT be used, because Helm never upgrades it.
  - `crds.install: true` controls whether the CRD is rendered.
  - `crds.keep: true` adds `helm.sh/resource-policy: keep` to the CRD, so uninstalling the chart does not delete every SecretSpec.
- **Work-cluster constraints:** the owner reported no extra registry rules or admission-policy rules for the work clusters. The design assumes there are none. **[architect assumption]**
- **Mirror-friendly anyway:** the image reference is split so that a pull-through cache or mirror needs a single override.
  - The image reference is `<registry>/<repository>@<digest>` when `image.digest` is set, else `<registry>/<repository>:<tag>`.
  - `<registry>` is `global.imageRegistry` when non-empty, else `image.registry`.
  - `imagePullSecrets` is passed to the pod unchanged.

Values:

```yaml
global:
  imageRegistry: ""     # overrides image.registry when set (mirror / pull-through cache)
image:
  registry: ghcr.io
  repository: jabbas/secretspec-operator
  tag: ""               # defaults to chart appVersion
  digest: ""            # sha256:...; when set, used instead of tag
  pullPolicy: IfNotPresent
imagePullSecrets: []    # e.g. [{name: mirror-pull}]
replicas: 1
leaderElection:
  enabled: true
watchNamespaces: []     # [] = all namespaces (ClusterRole); list = Role per namespace
crds:
  install: true
  keep: true
resources: {}           # small sensible defaults, chosen by the builder
podAnnotations: {}
podLabels: {}
nodeSelector: {}
tolerations: []
affinity: {}
priorityClassName: ""
podSecurityContext: {}  # restricted defaults
securityContext: {}     # restricted defaults
# startup / liveness / readiness probes on /healthz and /readyz
metrics:
  service:
    enabled: true
serviceMonitor:
  enabled: false
logLevel: info
logFormat: json
resyncInterval: 1h
```

### 10.4 CI/CD (GitHub Actions)

- The project is public, so CI MUST be public.
- Keyless signing uses GitHub OIDC.
- Actions MUST be pinned by SHA.

| Trigger | Steps |
|---|---|
| Pull request | `fmt`, `clippy`, tests, `cargo-deny`, `cargo-audit`, crdgen diff check, `helm lint` / `helm template`, e2e on kind against Kubernetes 1.34 and the newest stable minor. |
| Tag `vX.Y.Z` | Multi-arch build; vulnerability scan (fail on high); SBOM attestation; cosign keyless signing of the image and the chart; push image and chart; GitHub Release with notes generated from conventional commits. |
| Tag `vX.Y.Z-rc.N` | The full tag pipeline above, unchanged: multi-arch build, scan, SBOM, cosign signing, image and chart push. The GitHub Release is marked as a *pre-release*. |
| Maintenance | Renovate or Dependabot for Cargo, Actions and base images. |

- The release workflow triggers only on tags matching `vX.Y.Z` and `vX.Y.Z-rc.N`.
- No `latest` tag is pushed, for either final releases or release candidates.

**Verifying signatures.** Signatures are keyless. They are bound to the identity of the release workflow run for the tag:

| Property | Value |
|---|---|
| Certificate identity | `https://github.com/jabbas/secretspec-operator/.github/workflows/<release workflow>@refs/tags/<tag>` |
| OIDC issuer | `https://token.actions.githubusercontent.com` |

The docs (`docs/installation.md` and `docs/security.md`) MUST show the exact `cosign verify` command, with the real workflow file name filled in. Its shape is:

```sh
cosign verify ghcr.io/jabbas/secretspec-operator:<tag> \
  --certificate-identity 'https://github.com/jabbas/secretspec-operator/.github/workflows/<release workflow>@refs/tags/<tag>' \
  --certificate-oidc-issuer 'https://token.actions.githubusercontent.com'
```

The same command, with the chart reference `ghcr.io/jabbas/charts/secretspec-operator:<chart version>`, verifies the chart.

### 10.5 Supported Kubernetes versions

- The minimum supported Kubernetes version is **1.34**, regardless of upstream end-of-life. EKS extended support keeps clusters on older minors for a long time. **[owner]**
- The supported range is 1.34 up to the newest stable minor at release time. **[architect]**
- The `k8s-openapi` feature is pinned to `v1_34` (§8.2). **[architect]**
- E2E on kind runs against 1.34 and the newest stable minor (§10.4). **[architect]**
- Every Kubernetes feature this design relies on is GA well before 1.34, so newer minors bring no required benefit. **[architect]**

  | Feature | GA in |
  |---|---|
  | CRD CEL validation rules (`x-kubernetes-validations`), including `messageExpression` and transition rules with `oldSelf` | 1.29 |
  | CRD validation ratcheting (on by default since 1.30) | 1.33 |
  | CRD `selectableFields` | 1.32 |

- Raising the minimum later is a minor-version decision. It MUST be recorded in the release notes. **[architect]**

## 11. Testing **[architect]**

| Layer | Coverage |
|---|---|
| Unit — `plan` | Table-driven, one test per row of §7.2. |
| Unit — generators | Length, every charset, `exclude`, `require` counts, `bytes` with every encoding, `prefix`/`suffix`, entropy-warning threshold. |
| Unit — SSH | Parse the key back; the public key matches the private key; comment handling; every algorithm and size. |
| Unit — htpasswd | Verifies with bcrypt. Not recomputed when inputs are unchanged. |
| Unit — fingerprints | Golden values: fixed inputs produce fixed hashes. These MUST never change. |
| CEL | Client-side `validate_cel` tests for every rule. E2E asserts that the API server rejects invalid manifests. |
| E2E (kind) | See the scenario list below. |

E2E scenarios:

1. Install the chart and apply every example; assert that the Secrets exist.
2. Simulate a restart (delete the pod); assert values are unchanged.
3. Delete one key; assert only that key changes.
4. Delete the Secret; assert new values.
5. `Retain`: uninstall and reinstall; assert values are preserved.
6. Namespace-restricted mode: assert other namespaces are ignored.
7. Rotation with a short interval.

## 12. Home-lab test procedure **[owner]**

This procedure is part of the project. It is not a production rollout.

- **Cluster:** the owner's Talos home lab. Deploy directly with helm/kubectl, NOT via Flux. The deployment is disposable.
- **Install:** namespace `test-secretspec`. Install the chart there with `watchNamespaces: [test-secretspec]`, which exercises the restricted mode.
- **Release:** a published release candidate from ghcr (image and OCI chart), e.g. `v0.1.0-rc.1`. Using an rc also validates signing and multi-arch publishing; verify the signature with the `cosign verify` command from §10.4 before installing. **[architect]**
- **Who runs it:** the builder runs the procedure from the owner's workstation, which already has `kubectl` access to the home-lab cluster. The builder starts only after explicit confirmation from the owner. The owner MAY run it himself instead. **[architect]**
- **Run:** go through the e2e scenario list (§11) manually, using the examples.
- **Outcome:** if the rc passes, the final `vX.Y.Z` is tagged (§2). If it fails, the fix ships as the next rc (`-rc.N+1`), which repeats this procedure. **[architect]**
- **Teardown:**
  1. `helm uninstall`;
  2. delete the namespace;
  3. delete the CRD. Because of `crds.keep`, it MUST be deleted explicitly.

## 13. Documentation deliverables **[architect]**

All documentation is in English.

| Deliverable | Content |
|---|---|
| `README.md` | What and why; a 5-line quickstart; the single acknowledgement sentence. |
| `docs/installation.md` | Chart install, values (incl. registry mirror, `imagePullSecrets`, digest pinning), namespace modes, CRD lifecycle, the exact `cosign verify` commands (§10.4), supported Kubernetes versions (§10.5). |
| `docs/api-reference.md` | Every field, its default and its validation. |
| `docs/operations.md` | Manual rotation (delete the Secret); consumers must reload; metrics; troubleshooting via conditions and Events. |
| `docs/security.md` | RBAC, threat model, the adoption note, value-redaction guarantees, release signing and the `cosign verify` commands (§10.4). |
| `examples/` | Copy-paste SecretSpecs (list below). |

Examples:

1. App admin password (`Retain`).
2. Traefik basic auth (`keys.htpasswd: users`).
3. Garage `rpc_secret` plus access key id.
4. Fernet key.
5. SSH deploy key.
6. Rotated shared token.
7. RDS-compatible password (`exclude` + `require`).

There is no migration guide in v0.1.

## 14. Observability **[architect]**

Metric names are indicative; the builder may refine them.

| Metric | Type | Labels |
|---|---|---|
| `secretspec_reconcile_total` | counter | `result` |
| `secretspec_reconcile_duration_seconds` | histogram | — |
| `secretspec_generations_total` | counter | `kind`, `reason` |
| `secretspec_ready` | gauge | `namespace`, `name` |
| `secretspec_next_rotation_timestamp_seconds` | gauge | `namespace`, `name` |

Logs are structured JSON. They include namespace, name and reason. They MUST NEVER include values.

## 15. Future work (not v0.1)

- Templates and derived keys.
- A migration guide.
- Namespace label-selector scoping.
- Additional generators.
- `v1beta1`.

## 16. Left to the builder

- Exact crate versions and the leader-election crate.
- Module layout inside the crates.
- Exact CEL expressions and messages.
- Metric names.
- Resource defaults.
- The release-notes tool.
- The release workflow file name. It fills `<release workflow>` in the signing identity (§10.4), and the docs then show it verbatim.
- The image base (distroless vs `scratch`).
- The order of work after the API freeze milestone. The milestone itself, and the owner's sign-off before any controller, chart or release-pipeline work, are NOT left to the builder (§4.5).
