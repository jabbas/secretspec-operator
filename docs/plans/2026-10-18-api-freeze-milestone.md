# API Freeze Milestone Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Produce the frozen `secretspec.jabbas.eu/v1alpha1` API: Rust CRD types, CEL rules, defaults resolution, fingerprint function with golden tests, and generated CRD YAML validated by a real API server — then stop for owner sign-off.

**Architecture:** Cargo workspace. `secretspec-api` holds the CRD types (kube `CustomResource` + `KubeSchema`) and `resolve()`, which turns a stored spec into fully resolved, re-validated parameters. `secretspec-core` holds the fingerprint format and the redacting `SecretValue` type. `xtask` renders the CRD YAML. Shared YAML fixtures drive client-side CEL tests, `resolve()` tests and a server-side suite run against kind.

**Tech Stack:** Rust (edition 2024), kube 4.x (`derive`, `cel` for tests, `client`+`runtime` only in dev-deps), k8s-openapi 0.28 (`v1_34`), schemars 1, serde/serde_json, a maintained serde YAML crate (not the archived `serde_yaml`), sha2, hex, thiserror, zeroize, tokio (tests only), kind.

**Spec:** `docs/design/2026-10-18-secretspec-operator-design.md` (approved 2026-10-18). Read §4, §4.5, §5, §11 before starting.

**Scope:** This is plan 1 of 4. Plans 2 (generators + planner), 3 (operator binary) and 4 (chart, release pipeline, docs, e2e, home-lab test) are written only after the owner signs off on this milestone (design §4.5 "API freeze gate"). Do not create the operator crate, a Helm `Chart.yaml`, or a release workflow here.

## Global Constraints

- Minimum Kubernetes 1.34; `k8s-openapi` feature `v1_34`; kind node images for tests: 1.34 and the newest stable minor.
- Licence Apache-2.0. Every `.rs` file starts with `// SPDX-License-Identifier: Apache-2.0`.
- Never name any other secret-generator project, fork, company or person in code, comments, docs, fixtures or commit messages.
- `#![forbid(unsafe_code)]` (via workspace lints); clippy `-D warnings`; no `unwrap()`/`expect()` outside tests; rustfmt.
- Defaults are frozen for `v1alpha1`: `deletionPolicy: Delete`, `secret.type: Opaque`, `sshKeyPair.algorithm: ed25519`, `sshKeyPair.keys.privateKey: ssh-privatekey`, `sshKeyPair.keys.publicKey: ssh-publickey`, `basicAuth.keys.{username: username, password: password, htpasswd: auth}` (all in the CRD schema); `length: 32`, `charset: alphanumeric`, `rsaBits: 4096`, `ecdsaCurve: P-256`, `comment: <name>@<namespace>`, bcrypt cost 12 (frozen constants in code).
- Secret values never appear in `Debug`/`Display`/logs/errors.
- Conventional commits (`feat(api): …`, `test(api): …`, `build: …`, `ci: …`, `docs: …`).
- All work on branch `api-freeze`; the milestone ends with a PR the owner approves (= sign-off).

## Review Focus

- **CRD rejected by a real API server for CEL cost or non-structural schema** (unbounded strings/lists/maps). Expected: CRD applies cleanly on 1.34 and newest. Pinned by `schema_is_bounded` (Task 2) and `crd_applies_and_becomes_established` (Task 7).
- **Invalid input reaching the operator despite the CRD** (objects stored before a rule existed, ratcheting, a stale CRD). Expected: `resolve()` rejects it with a readable message, never panics. Pinned by `every_invalid_fixture_is_rejected_by_resolve` (Task 4).
- **Default materialisation surprises.** Expected: `algorithm`, `keys.*`, `deletionPolicy`, `secret.type` are written by the API server; `fields[].length`/`charset`, `rsaBits`, `ecdsaCurve`, `comment` are not. Pinned by `schema_defaults_present_and_absent` (Task 2) and `dry_run_materialises_expected_defaults` (Task 7).
- **Injection through free-text fields** (`basicAuth.username` with `:` or newline breaks htpasswd; newline in `comment` breaks `authorized_keys`; non-ASCII in `alphabet`/`prefix`). Expected: rejected at admission and by `resolve()`. Pinned by fixtures `i23`, `i30`, `i32`, `i33` (Tasks 3, 4, 7).
- **Fingerprint drift from incidental representation** (field order, alphabet order or duplicates, map ordering, SSH comment, key names). Expected: identical resolved parameters → identical fingerprint. Pinned by Task 5 tests listed there.

---

## File Structure

```
Cargo.toml                         # workspace, shared deps, [workspace.lints]
rust-toolchain.toml                # current stable (>= 1.85), rustfmt + clippy
clippy.toml                        # allow-unwrap/expect-in-tests
deny.toml                          # licences/advisories/sources
LICENSE                            # Apache-2.0 (replaces GPL-3.0)
NOTICE
.gitignore
.github/workflows/ci.yml
crates/secretspec-api/
  Cargo.toml
  src/lib.rs                       # re-exports
  src/v1alpha1/mod.rs              # SecretSpec root + SecretSpecSpec
  src/v1alpha1/fields.rs           # FieldSpec, Charset, Encoding, RequireSpec
  src/v1alpha1/ssh.rs              # SshKeyPairSpec, SshAlgorithm, EcdsaCurve, SshKeys
  src/v1alpha1/basic_auth.rs       # BasicAuthSpec, BasicAuthPassword, BasicAuthKeys
  src/v1alpha1/status.rs           # SecretSpecStatus
  src/constants.rs                 # frozen defaults, label/annotation names, messages
  src/resolve.rs                   # resolve(), Resolved* types, InvalidSpec
  tests/fixtures/valid/*.yaml
  tests/fixtures/invalid/*.yaml
  tests/fixtures/update/*/{old,new}.yaml
  tests/common/mod.rs              # fixture loader
  tests/schema.rs                  # Task 2
  tests/cel.rs                     # Task 3
  tests/resolve.rs                 # Task 4
  tests/server_validation.rs       # Task 7 (#[ignore])
crates/secretspec-core/
  Cargo.toml
  src/lib.rs
  src/value.rs                     # SecretValue
  src/fingerprint.rs               # Slot, Fingerprint, FingerprintMap, canonical forms
  tests/fingerprint_golden.rs
xtask/
  Cargo.toml
  src/main.rs                      # `cargo xtask crdgen [--check]`
.cargo/config.toml                 # alias xtask
charts/secretspec-operator/files/crd/secretspecs.secretspec.jabbas.eu.yaml   # generated, committed
docs/fingerprints.md               # normative, frozen
```

---

### Task 1: Workspace, licence, lint and CI skeleton

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, `deny.toml`, `.gitignore`, `NOTICE`, `.cargo/config.toml`, `.github/workflows/ci.yml`, `crates/secretspec-api/{Cargo.toml,src/lib.rs}`, `crates/secretspec-core/{Cargo.toml,src/lib.rs}`, `xtask/{Cargo.toml,src/main.rs}`
- Modify: `LICENSE` (replace with Apache-2.0 full text from apache.org)

**Interfaces:**
- Produces: workspace members `crates/secretspec-api` (lib `secretspec_api`), `crates/secretspec-core` (lib `secretspec_core`), `xtask` (bin); `cargo xtask` alias.

- [ ] **Step 1: Create branch** `git switch -c api-freeze`.
- [ ] **Step 2: Workspace `Cargo.toml`** with `resolver = "3"`, `[workspace.package] edition = "2024", license = "Apache-2.0", repository = "https://github.com/jabbas/secretspec-operator", rust-version = <toolchain version>`, `[workspace.dependencies]` for every crate listed in Tech Stack, and lints:
  ```toml
  [workspace.lints.rust]
  unsafe_code = "forbid"
  [workspace.lints.clippy]
  pedantic = { level = "warn", priority = -1 }
  unwrap_used = "deny"
  expect_used = "deny"
  module_name_repetitions = "allow"
  must_use_candidate = "allow"
  ```
  Each member sets `[lints] workspace = true`.
- [ ] **Step 3: `clippy.toml`**: `allow-unwrap-in-tests = true`, `allow-expect-in-tests = true`. **`deny.toml`**: licences allowed `Apache-2.0, MIT, BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0, Zlib, CC0-1.0`; advisories deny vulnerabilities and unmaintained; sources crates.io only; bans `multiple-versions = "warn"`.
- [ ] **Step 4: Licence files.** `LICENSE` = verbatim Apache-2.0 text. `NOTICE`:
  ```
  SecretSpec Operator
  Copyright 2026 The SecretSpec Operator Authors

  This product is licensed under the Apache License, Version 2.0.
  ```
- [ ] **Step 5: Empty crates** compile (`lib.rs` with SPDX header + crate doc comment; xtask `main` prints usage).
- [ ] **Step 6: CI** `.github/workflows/ci.yml` on `pull_request` and `push` to `main`, actions pinned by full commit SHA (with a `# vX` comment), jobs: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo deny check`, `cargo audit`.
- [ ] **Step 7: Verify** `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check` → all pass.
- [ ] **Step 8: Commit** `build: workspace skeleton, Apache-2.0 licence, lint and CI baseline`.

---

### Task 2: CRD types, schema bounds and schema defaults

**Files:**
- Create: `crates/secretspec-api/src/v1alpha1/{mod.rs,fields.rs,ssh.rs,basic_auth.rs,status.rs}`, `src/constants.rs`, `tests/common/mod.rs`, `tests/schema.rs`, `tests/fixtures/valid/*.yaml` (list in Task 3)
- Modify: `crates/secretspec-api/src/lib.rs`

**Interfaces:**
- Produces (all `pub`, serde `rename_all = "camelCase"`, derive `Clone, Debug, PartialEq, Serialize, Deserialize, KubeSchema`):
  - `SecretSpec` (root, via `#[kube(group = "secretspec.jabbas.eu", version = "v1alpha1", kind = "SecretSpec", plural = "secretspecs", shortname = "ss", namespaced, status = "SecretSpecStatus", printcolumn(...))]`) with spec `SecretSpecSpec { deletion_policy: DeletionPolicy, rotation: Option<RotationSpec>, secret: SecretTemplate, data: BTreeMap<String,String>, fields: Option<Vec<FieldSpec>>, ssh_key_pair: Option<SshKeyPairSpec>, basic_auth: Option<BasicAuthSpec> }`
  - `enum DeletionPolicy { Delete (default), Retain }`
  - `RotationSpec { interval: String }`
  - `SecretTemplate { type_: String (json "type", default "Opaque"), labels: BTreeMap<String,String>, annotations: BTreeMap<String,String> }` — `secret` itself `#[serde(default)]` so the API server materialises `secret: {type: Opaque}`.
  - `FieldSpec { name: String, length: Option<u16>, charset: Option<Charset>, alphabet: Option<String>, exclude: Option<String>, require: Option<RequireSpec>, bytes: Option<u16>, encoding: Option<Encoding>, prefix: Option<String>, suffix: Option<String> }` — no schema defaults on `length`/`charset`.
  - `enum Charset { Alphanumeric, Ascii, Hex, Custom }` serialised lowercase (`alphanumeric|ascii|hex|custom`).
  - `enum Encoding { Hex, Base64, Base64url, Base32 }` serialised `hex|base64|base64url|base32`.
  - `RequireSpec { lower: Option<u16>, upper: Option<u16>, digits: Option<u16>, symbols: Option<u16> }`
  - `SshKeyPairSpec { algorithm: SshAlgorithm (default ed25519), rsa_bits: Option<RsaBits>, ecdsa_curve: Option<EcdsaCurve>, comment: Option<String>, keys: SshKeys (#[serde(default)]) }`; `enum SshAlgorithm { Ed25519, Rsa, Ecdsa }` → `ed25519|rsa|ecdsa`; `enum RsaBits { 2048, 3072, 4096 }` serialised as integers; `enum EcdsaCurve { P256, P384, P521 }` → `P-256|P-384|P-521`; `SshKeys { private_key: String (default "ssh-privatekey"), public_key: String (default "ssh-publickey") }`.
  - `BasicAuthSpec { username: String, password: BasicAuthPassword (#[serde(default)]), keys: BasicAuthKeys (#[serde(default)]) }`; `BasicAuthPassword` = the mode-A options of `FieldSpec` without `name`/`bytes`/`encoding`, no schema defaults; `BasicAuthKeys { username: "username", password: "password", htpasswd: "auth" }` defaults.
  - `SecretSpecStatus { observed_generation: Option<i64>, kind: Option<SecretKind>, secret_name: Option<String>, last_generated_time: Option<Time>, last_rotation_time: Option<Time>, next_rotation_time: Option<Time>, conditions: Vec<Condition> }`; `enum SecretKind { Fields, SSHKeyPair, BasicAuth }` serialised exactly `Fields|SSHKeyPair|BasicAuth`.
  - `constants.rs`: `GROUP`, `LABEL_MANAGED_BY = "app.kubernetes.io/managed-by"`, `MANAGED_BY_VALUE = "secretspec-operator"`, `LABEL_SPEC_NAME = "secretspec.jabbas.eu/spec-name"`, `ANNOTATION_FINGERPRINTS = "secretspec.jabbas.eu/fingerprints"`, `ANNOTATION_GENERATED_AT = "secretspec.jabbas.eu/generated-at"`, `ANNOTATION_SPEC_UID = "secretspec.jabbas.eu/spec-uid"`, `RESERVED_PREFIX = "secretspec.jabbas.eu/"`, `DEFAULT_LENGTH: u16 = 32`, `DEFAULT_RSA_BITS: u16 = 4096`, `DEFAULT_ECDSA_CURVE = EcdsaCurve::P256`, `BCRYPT_COST: u32 = 12`, `MAX_RANDOM_LENGTH: u16 = 1024`, and the rule messages from Task 3.
  - `tests/common/mod.rs`: `fn load(dir: &str) -> Vec<Fixture>`; `struct Fixture { file: String, expect: Option<String>, layer: Layer, value: serde_json::Value }`; `enum Layer { Cel, Schema }` parsed from header comments `# expect: <text>` and `# layer: cel|schema` (default `cel`).

Schema bounds (schemars `length`/`range`/`pattern` attributes; `extend("maxProperties" = N)` for maps): `fields` maxItems 32 and list-type map keyed by `name`; `name`, `keys.*`, map keys ≤ 253 (map keys via CEL in Task 3); `data` maxProperties 64, values maxLength 65536; `secret.labels`/`annotations` maxProperties 32, values maxLength 4096; `secret.type` maxLength 253; `length` and `bytes` 1–1024; `require.*` 0–1024; `alphabet`, `exclude` maxLength 256; `prefix`, `suffix` maxLength 64 pattern `^[!-~]*$`; `alphabet` pattern `^[!-~]+$`; `exclude` pattern `^[ -~]*$`; `comment` maxLength 256 pattern `^[ -~]*$`; `username` minLength 1, maxLength 253 pattern `^[!-9;-~]+$` (visible ASCII without `:`); `name` and `keys.*` pattern `^[-._a-zA-Z0-9]+$`; `rotation.interval` maxLength 8 pattern `^[1-9][0-9]*(m|h|d)$`.

- [ ] **Step 1: Write failing tests in `tests/schema.rs`:**
  - `crd_has_expected_identity`: `SecretSpec::crd()` has group `secretspec.jabbas.eu`, version `v1alpha1` served+storage, kind `SecretSpec`, plural `secretspecs`, short names `["ss"]`, scope `Namespaced`, status subresource, printer columns named exactly `Ready` (`.status.conditions[?(@.type=="Ready")].status`), `Kind` (`.status.kind`), `Next Rotation` (`.status.nextRotationTime`, type `date`), `Age` (`.metadata.creationTimestamp`, type `date`).
  - `schema_is_bounded`: walk the `openAPIV3Schema` of `spec`; every `type: string` node has `maxLength` or `enum`; every `type: array` has `maxItems`; every object with `additionalProperties` has `maxProperties`.
  - `schema_defaults_present_and_absent`: defaults exist at `spec.deletionPolicy = "Delete"`, `spec.secret = {"type":"Opaque"}`, `spec.secret.type = "Opaque"`, `spec.sshKeyPair.algorithm = "ed25519"`, `spec.sshKeyPair.keys` and its two properties, `spec.basicAuth.keys` and its three properties; NO `default` at `fields.items.length`, `fields.items.charset`, `sshKeyPair.rsaBits`, `sshKeyPair.ecdsaCurve`, `sshKeyPair.comment`, `basicAuth.password.length`.
  - `fields_is_list_map_keyed_by_name`: `x-kubernetes-list-type: map`, `x-kubernetes-list-map-keys: ["name"]`.
  - `every_valid_fixture_deserializes`: each `tests/fixtures/valid/*.yaml` deserializes into `SecretSpec` and re-serializes to JSON equal to the input after applying serde defaults (no unknown fields dropped: use `#[serde(deny_unknown_fields)]` on spec structs).
- [ ] **Step 2: Run** `cargo test -p secretspec-api --test schema` → FAIL (types missing).
- [ ] **Step 3: Implement the types and constants** per the Interfaces block. Field docs (`///`) become CRD descriptions — write them for developers (what it does, default, example), because `kubectl explain` is the primary docs surface.
- [ ] **Step 4: Run** `cargo test -p secretspec-api --test schema` → PASS.
- [ ] **Step 5: Commit** `feat(api): SecretSpec v1alpha1 types with bounded schema and schema defaults`.

---

### Task 3: CEL rules and fixtures (client-side)

**Files:**
- Modify: `crates/secretspec-api/src/v1alpha1/*.rs`, `src/constants.rs`
- Create: `tests/cel.rs`, `tests/fixtures/{valid,invalid,update}/…`

**Interfaces:**
- Consumes: Task 2 types, `tests/common`.
- Produces: CEL rules attached via `#[x_kube(validation = Rule::new(..).message(..))]`; message constants `MSG_*` in `constants.rs` (exact strings below, reused by `resolve()` in Task 4).

Rules and their exact messages (CEL expression shape is the builder's; semantics are fixed):

| Const | Attached to | Rule | Message |
|---|---|---|---|
| `MSG_EXACTLY_ONE_KIND` | spec | exactly one of `fields`, `sshKeyPair`, `basicAuth` | `exactly one of fields, sshKeyPair or basicAuth must be set` |
| `MSG_BYTES_EXCLUSIVE` | field | `bytes` set ⇒ none of `length, charset, alphabet, exclude, require` | `bytes cannot be combined with length, charset, alphabet, exclude or require` |
| `MSG_ENCODING_WITH_BYTES` | field | `has(encoding) == has(bytes)` | `encoding is required with bytes and only allowed with bytes` |
| `MSG_ALPHABET_WITH_CUSTOM` | field, basicAuth.password | `has(alphabet) == (charset == 'custom')` | `alphabet is required when charset is custom and only allowed then` |
| `MSG_REQUIRE_SUM` | field, basicAuth.password | Σ require ≤ (length or 32) | `the sum of require counts must not exceed length (default 32)` |
| `MSG_ROTATION_RANGE` | rotation.interval | between 60m/1h and 3650d (m ≥ 60 and ≤ 5256000; h ≥ 1 and ≤ 87600; d ≤ 3650) | `rotation.interval must be between 1h and 3650d` |
| `MSG_RSA_BITS_ONLY_RSA` | sshKeyPair | `has(rsaBits) ⇒ algorithm == 'rsa'` | `rsaBits is only allowed with algorithm rsa` |
| `MSG_CURVE_ONLY_ECDSA` | sshKeyPair | `has(ecdsaCurve) ⇒ algorithm == 'ecdsa'` | `ecdsaCurve is only allowed with algorithm ecdsa` |
| `MSG_KEYS_DISTINCT` | sshKeyPair.keys, basicAuth.keys | all key names distinct | `secret key names in keys must be distinct` |
| `MSG_DATA_COLLISION` | spec | no `data` key equals a field `name`, an `sshKeyPair.keys.*` or a `basicAuth.keys.*` value | `data keys must not collide with generated keys` |
| `MSG_DATA_KEY_FORMAT` | data | every key matches `^[-._a-zA-Z0-9]+$` and size ≤ 253 | `data keys must match [-._a-zA-Z0-9]+ and be at most 253 characters` |
| `MSG_RESERVED_METADATA` | secret | no label/annotation key starts with `secretspec.jabbas.eu/` or equals `app.kubernetes.io/managed-by`; keys ≤ 253 | `labels and annotations under secretspec.jabbas.eu/ and app.kubernetes.io/managed-by are reserved` |
| `MSG_BASIC_AUTH_TYPE` | spec | `secret.type == 'kubernetes.io/basic-auth'` ⇒ `basicAuth` set with `keys.username == 'username'` and `keys.password == 'password'` | `type kubernetes.io/basic-auth requires basicAuth with keys.username "username" and keys.password "password"` |
| `MSG_SSH_AUTH_TYPE` | spec | `secret.type == 'kubernetes.io/ssh-auth'` ⇒ `sshKeyPair` set with `keys.privateKey == 'ssh-privatekey'` | `type kubernetes.io/ssh-auth requires sshKeyPair with keys.privateKey "ssh-privatekey"` |
| `MSG_TYPE_IMMUTABLE` | secret.type | transition rule `self == oldSelf` | `secret.type is immutable` |

Fixtures (one manifest per file, `apiVersion/kind/metadata.name/namespace` present; invalid ones start with `# expect: <message or distinctive substring>` and `# layer: cel` or `# layer: schema`):

- `valid/`: `v01-minimal-fields` (`fields: [{name: password}]`), `v02-app-db` (design §4.0 first example), `v03-ssh-default` (`sshKeyPair: {}`), `v04-ssh-rsa-3072`, `v05-ssh-ecdsa-p384`, `v06-basic-auth-traefik` (`keys.htpasswd: users`), `v07-basic-auth-k8s-type`, `v08-ssh-auth-type`, `v09-bytes-hex` (32, hex), `v10-bytes-base64url` (32), `v11-prefix-hex` (`prefix: GK, length: 24, charset: hex`), `v12-rds-password` (`charset: ascii, exclude: '/@" ', require` 1 each), `v13-custom-alphabet`, `v14-rotation-60m`, `v15-rotation-3650d`, `v16-require-sum-equals-default` (Σ = 32, no length), `v17-ssh-comment-empty` (`comment: ""`).
- `invalid/`: `i01-no-kind`, `i02-two-kinds`, `i03-bytes-with-length`, `i04-bytes-without-encoding`, `i05-encoding-without-bytes`, `i06-custom-without-alphabet`, `i07-alphabet-without-custom`, `i08-require-exceeds-length`, `i09-require-exceeds-default` (Σ = 33), `i10-length-zero` (schema), `i11-length-1025` (schema), `i12-rotation-59m`, `i13-rotation-3651d`, `i14-rotation-0h` (schema), `i15-rotation-1w` (schema), `i16-rsabits-with-ed25519`, `i17-curve-with-rsa`, `i18-data-collides-field`, `i19-data-collides-ssh-default` (`data.ssh-privatekey`), `i20-data-collides-htpasswd` (`data.auth` with basicAuth defaults), `i21-ssh-keys-equal`, `i22-basic-auth-keys-equal`, `i23-username-colon` (schema), `i24-reserved-label`, `i25-reserved-annotation`, `i26-basic-auth-type-with-fields`, `i27-ssh-auth-type-wrong-key`, `i28-data-key-slash`, `i29-field-name-slash` (schema), `i30-prefix-non-ascii` (schema), `i31-duplicate-field-names` (schema; list-map uniqueness), `i32-comment-newline` (schema), `i33-username-newline` (schema).
- `update/u01-type-immutable/{old,new}.yaml`: `Opaque` → `kubernetes.io/basic-auth` (with a basicAuth spec valid for that type); `# expect: secret.type is immutable` in `new.yaml`.

Note for invalid fixtures that rely on schema defaults (`i19`, `i20`): write them *without* the defaulted fields; client-side tests must apply defaults first (see Step 1).

- [ ] **Step 1: Write failing tests in `tests/cel.rs`:**
  - `valid_fixtures_pass_cel`: for each valid fixture, deserialize into `SecretSpec` (serde defaults stand in for schema defaults), `validate_cel(&obj)` → `Ok`.
  - `cel_layer_invalid_fixtures_fail_with_expected_message`: for each invalid fixture with `layer: cel`, deserialize then `validate_cel` → `Err` whose rendered text contains `expect`.
  - `type_change_is_rejected`: `validate_cel_update(&new, &old)` for `u01` → `Err` containing `secret.type is immutable`; `validate_cel_update(&old, &old)` → `Ok`.
  - `every_rule_has_a_fixture`: every `MSG_*` constant appears as the `expect` of at least one fixture (guards against untested rules).
- [ ] **Step 2: Run** `cargo test -p secretspec-api --features kube/cel --test cel` → FAIL.
- [ ] **Step 3: Add the rules** to the types. If the client-side CEL library lacks a function the API server has, keep the rule (server is the authority) and mark that fixture `# layer: schema` with a code comment naming the missing function; Task 7 still covers it.
- [ ] **Step 4: Run** the same command → PASS; also re-run Task 2 tests.
- [ ] **Step 5: Commit** `feat(api): CEL validation rules with fixtures`.

---

### Task 4: Defaults resolution and runtime validation

**Files:**
- Create: `crates/secretspec-api/src/resolve.rs`, `tests/resolve.rs`
- Modify: `src/lib.rs` (re-export)

**Interfaces:**
- Consumes: Task 2 types and constants, Task 3 messages.
- Produces:
  ```rust
  pub fn resolve(spec: &SecretSpecSpec, name: &str, namespace: &str) -> Result<ResolvedSpec, InvalidSpec>;

  pub struct ResolvedSpec { pub kind: ResolvedKind, pub data: BTreeMap<String, String>,
      pub deletion_policy: DeletionPolicy, pub rotation: Option<std::time::Duration>,
      pub secret_type: String, pub labels: BTreeMap<String,String>, pub annotations: BTreeMap<String,String> }
  pub enum ResolvedKind { Fields(Vec<ResolvedField>), SshKeyPair(ResolvedSsh), BasicAuth(ResolvedBasicAuth) }
  pub struct ResolvedField { pub name: String, pub generator: Generator }
  pub enum Generator { Chars(CharsParams), Bytes(BytesParams) }
  pub struct CharsParams { pub length: u16, pub alphabet: Vec<u8> /* sorted ascending, unique, after exclude */,
      pub require: Require, pub prefix: String, pub suffix: String }
  pub struct Require { pub lower: u16, pub upper: u16, pub digits: u16, pub symbols: u16 }
  pub struct BytesParams { pub count: u16, pub encoding: Encoding, pub prefix: String, pub suffix: String }
  pub enum SshAlgorithmParams { Ed25519, Rsa { bits: u16 }, Ecdsa { curve: EcdsaCurve } }
  pub struct ResolvedSsh { pub algorithm: SshAlgorithmParams, pub comment: String, pub private_key_key: String, pub public_key_key: String }
  pub struct ResolvedBasicAuth { pub username: String, pub password: CharsParams,
      pub username_key: String, pub password_key: String, pub htpasswd_key: String }
  impl ResolvedSpec { pub fn secret_kind(&self) -> SecretKind; pub fn entropy_bits(&self) -> Vec<(String /* key */, f64)>; }

  #[derive(Debug, thiserror::Error)] #[error("{}", .errors.join("; "))]
  pub struct InvalidSpec { pub errors: Vec<String> }  // each "<json path>: <message>"
  ```
  All `Resolved*` derive `Clone, Debug, PartialEq, Eq` (except f64 users).

Rules: `resolve` re-checks **every** schema and CEL rule from Tasks 2–3 (using the same `MSG_*` text) plus the runtime-only rules: effective alphabet ≥ 2 characters (`fields[i]: alphabet must contain at least 2 characters after exclude`), each class with `require > 0` non-empty in the effective alphabet (`fields[i].require.<class>: no <class> characters left in the alphabet`). Alphabets: `alphanumeric` = `0-9A-Za-z`, `ascii` = alphanumeric + the 32 punctuation chars, `hex` = `0-9a-f`, `custom` = `alphabet` deduplicated; then remove `exclude`; then sort by byte. `comment` default = `format!("{name}@{namespace}")`; explicit `""` stays empty. Rotation: `m`/`h`/`d` → seconds. Collect all errors, do not stop at the first.

- [ ] **Step 1: Write failing tests in `tests/resolve.rs`:**
  - `minimal_field_resolves_to_frozen_defaults`: `v01` → `Chars { length: 32, alphabet: b"0123456789ABC…xyz" (62 bytes), require: all 0, prefix: "", suffix: "" }`.
  - `bytes_field_resolves`: `v09` → `Bytes { count: 32, encoding: Hex, .. }`.
  - `rds_password_alphabet_excludes_chars`: `v12` → alphabet has 94 − 4 = 90 bytes, contains none of `/ @ "` (space was never in it), require all 1.
  - `custom_alphabet_is_deduplicated_and_sorted`: alphabet `"cbaab"` → `b"abc"`.
  - `ssh_defaults`: `v03` with name `deploy`, namespace `app` → `Ed25519`, comment `deploy@app`, keys `ssh-privatekey`/`ssh-publickey`; `v04` → `Rsa { bits: 3072 }`; `rsa` without `rsaBits` → `4096`; `ecdsa` without curve → `P256`; `v17` → comment `""`.
  - `basic_auth_defaults`: `v06` → password `length 32`, alphanumeric, keys `username`/`password`/`users`.
  - `rotation_parses`: `60m` → 3600 s, `90d` → 7 776 000 s, `3650d` → 315 360 000 s.
  - `runtime_rules`: custom alphabet `"ab"` with `exclude: "a"` → error containing `at least 2 characters`; `charset: hex` with `require.upper: 1` → error containing `no upper characters left`.
  - `every_invalid_fixture_is_rejected_by_resolve`: every `invalid/*` fixture that deserializes → `Err` whose text contains its `expect`; fixtures that cannot deserialize (type errors) are skipped with an explicit allow-list (should be empty or near-empty).
  - `errors_are_collected`: a spec violating two rules reports both.
  - `entropy_estimate`: `v01` ≈ 32·log2(62) ≈ 190.5; `length: 8` → < 128; `bytes: 16` → 128.
- [ ] **Step 2: Run** `cargo test -p secretspec-api --test resolve` → FAIL.
- [ ] **Step 3: Implement `resolve.rs`.** No `unwrap`; never panics on any deserializable input.
- [ ] **Step 4: Run** → PASS; run the whole crate's tests.
- [ ] **Step 5: Commit** `feat(api): defaults resolution with runtime validation`.

---

### Task 5: Fingerprint format, `SecretValue`, golden tests

**Files:**
- Create: `crates/secretspec-core/src/{value.rs,fingerprint.rs}`, `crates/secretspec-core/tests/fingerprint_golden.rs`, `docs/fingerprints.md`
- Modify: `crates/secretspec-core/src/lib.rs`, `crates/secretspec-core/Cargo.toml` (depends on `secretspec-api`)

**Interfaces:**
- Consumes: `secretspec_api::{ResolvedSpec, ResolvedKind, CharsParams, BytesParams, SshAlgorithmParams, Encoding, EcdsaCurve}`.
- Produces:
  ```rust
  pub struct SecretValue(/* Zeroizing<Vec<u8>> */);           // Debug/Display print "<redacted>"
  impl SecretValue { pub fn new(bytes: Vec<u8>) -> Self; pub fn expose(&self) -> &[u8]; }

  #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
  pub enum Slot { Field(String), SshKeyPair, BasicAuthPassword, BasicAuthHtpasswd }
  // Display/FromStr: "field/<name>", "ssh/keypair", "basic-auth/password", "basic-auth/htpasswd"

  #[derive(Clone, Debug, PartialEq, Eq)] pub struct Fingerprint(String); // 64 lowercase hex chars
  pub fn canonical_chars(slot: &Slot, p: &CharsParams) -> String;
  pub fn canonical_bytes(slot: &Slot, p: &BytesParams) -> String;
  pub fn canonical_ssh(p: &SshAlgorithmParams) -> String;
  pub fn canonical_htpasswd(username: &str, password: &SecretValue, cost: u32) -> String;
  pub fn fingerprint_of(canonical: &str) -> Fingerprint;     // sha256, lowercase hex
  pub fn expected_fingerprints(spec: &ResolvedSpec) -> BTreeMap<Slot, Fingerprint>; // excludes htpasswd (value-dependent)

  #[derive(Clone, Debug, Default, PartialEq, Eq)] pub struct FingerprintMap(pub BTreeMap<Slot, Fingerprint>);
  impl FingerprintMap { pub fn to_annotation(&self) -> String;  // compact JSON, keys sorted
                        pub fn from_annotation(s: &str) -> Result<Self, FingerprintParseError>; }
  ```

Canonical format v1 (normative; copy into `docs/fingerprints.md` verbatim with the rationale "changing any byte rotates every credential"). Lines end with `\n`, including the last; hex is lowercase; empty values are written as `key=`:

```
secretspec-fingerprint-v1
slot=field/<name>
mode=chars
length=<n>
alphabet=<hex of sorted unique alphabet bytes>
require=<lower>,<upper>,<digits>,<symbols>
prefix=<hex of prefix bytes>
suffix=<hex of suffix bytes>
```
```
secretspec-fingerprint-v1
slot=field/<name>
mode=bytes
count=<n>
encoding=<hex|base64|base64url|base32>
prefix=<hex>
suffix=<hex>
```
```
secretspec-fingerprint-v1
slot=ssh/keypair
algorithm=ed25519            # or: algorithm=rsa + bits=<n>   or: algorithm=ecdsa + curve=<P-256|P-384|P-521>
```
`basic-auth/password` uses the chars form with `slot=basic-auth/password`.
```
secretspec-fingerprint-v1
slot=basic-auth/htpasswd
username=<hex of username bytes>
password-sha256=<hex of sha256(password)>
cost=<n>
```
Not fingerprinted: SSH `comment`, any `keys.*` name, `data` literals, `deletionPolicy`, `secret.*`, `rotation`.

- [ ] **Step 1: Write failing tests in `tests/fingerprint_golden.rs`:**
  - `canonical_chars_default_field`: for `field/password`, length 32, alphanumeric, no require/prefix/suffix, canonical string equals exactly:
    `"secretspec-fingerprint-v1\nslot=field/password\nmode=chars\nlength=32\nalphabet=303132333435363738394142434445464748494a4b4c4d4e4f505152535455565758595a6162636465666768696a6b6c6d6e6f707172737475767778797a\nrequire=0,0,0,0\nprefix=\nsuffix=\n"`.
  - `canonical_bytes_hex_32`: for `field/rpc-secret` → `"secretspec-fingerprint-v1\nslot=field/rpc-secret\nmode=bytes\ncount=32\nencoding=hex\nprefix=\nsuffix=\n"`.
  - `canonical_hex_prefix`: `field/access-key-id`, length 24, hex, prefix `GK` → alphabet line `alphabet=30313233343536373839616263646566`, prefix line `prefix=474b`.
  - `canonical_ssh_variants`: ed25519 → `"secretspec-fingerprint-v1\nslot=ssh/keypair\nalgorithm=ed25519\n"`; rsa 4096 → `…algorithm=rsa\nbits=4096\n`; ecdsa P-384 → `…algorithm=ecdsa\ncurve=P-384\n`.
  - `canonical_htpasswd`: username `admin`, password bytes `secret`, cost 12 → `"secretspec-fingerprint-v1\nslot=basic-auth/htpasswd\nusername=61646d696e\npassword-sha256=2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b\ncost=12\n"` (sha256 of `secret`; verify with `printf %s secret | shasum -a 256`).
  - `golden_fingerprint_values`: `fingerprint_of` of each canonical string above equals a pinned hex literal. **Pinning procedure:** compute each value independently with `printf '%s' "<canonical with real newlines>" | shasum -a 256`, paste the literal into the test, and confirm the Rust result matches. Never update these literals afterwards; a mismatch is a release blocker.
  - `fingerprint_independent_of_alphabet_order_and_duplicates`: custom `"cba"` and `"abcabc"` → identical fingerprint.
  - `fingerprint_ignores_comment_and_key_names`: two `ResolvedSpec`s differing only in SSH comment and `keys.*` → identical `expected_fingerprints`.
  - `fingerprint_changes_with_each_parameter`: changing length, alphabet, any require count, prefix, suffix, count, encoding, algorithm, bits, curve each changes the fingerprint.
  - `fingerprint_map_annotation_is_sorted_compact_json`: `{field/b, field/a}` → `{"field/a":"<hex>","field/b":"<hex>"}`; round-trips via `from_annotation`; `from_annotation("not json")` and an unknown slot string → `Err`.
  - `secret_value_redacts`: `format!("{:?} {}", v, v)` contains `<redacted>` and not the bytes.
- [ ] **Step 2: Run** `cargo test -p secretspec-core` → FAIL.
- [ ] **Step 3: Implement** `value.rs` and `fingerprint.rs`; write `docs/fingerprints.md` (format above, list of non-fingerprinted inputs, the stability rule, and the slot naming).
- [ ] **Step 4: Run** → PASS (golden literals pinned per Step 1 procedure).
- [ ] **Step 5: Commit** `feat(core): frozen v1 fingerprint format with golden tests`.

---

### Task 6: `cargo xtask crdgen` and committed CRD YAML

**Files:**
- Modify: `xtask/src/main.rs`, `xtask/Cargo.toml`, `.github/workflows/ci.yml`
- Create: `charts/secretspec-operator/files/crd/secretspecs.secretspec.jabbas.eu.yaml` (generated)

**Interfaces:**
- Consumes: `secretspec_api::SecretSpec::crd()`.
- Produces: `cargo xtask crdgen` writes the file; `cargo xtask crdgen --check` exits non-zero with a diff hint when the file is stale. The file starts with `# Generated by "cargo xtask crdgen". Do not edit.` and `---`. (Plan 4 will wrap this file in a chart template with `crds.install`/`crds.keep`.)

- [ ] **Step 1: Write a failing test** in `xtask` (`#[test] fn rendered_crd_is_deterministic()`): rendering twice yields identical bytes, and the YAML parses back to a `CustomResourceDefinition` named `secretspecs.secretspec.jabbas.eu`.
- [ ] **Step 2: Run** `cargo test -p xtask` → FAIL.
- [ ] **Step 3: Implement** `crdgen` (render `SecretSpec::crd()` to YAML; stable key order) and `--check`.
- [ ] **Step 4: Run** `cargo xtask crdgen && cargo xtask crdgen --check && cargo test -p xtask` → PASS; commit the generated file.
- [ ] **Step 5: CI**: add a step `cargo xtask crdgen --check`.
- [ ] **Step 6: Commit** `build: crdgen xtask and generated CRD`.

---

### Task 7: Server-side validation against a real API server

**Files:**
- Create: `crates/secretspec-api/tests/server_validation.rs`, `docs/development.md` (how to run it locally)
- Modify: `crates/secretspec-api/Cargo.toml` (dev-deps: `kube` with `client`,`runtime`; `tokio`), `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: the generated CRD file (Task 6), fixtures (Task 3), `tests/common`.
- Produces: `#[ignore]` tests run with `SECRETSPEC_TEST_KUBECONFIG=<path> cargo test -p secretspec-api --test server_validation -- --ignored --test-threads=1`; the tests panic with a clear message if the variable is unset.

- [ ] **Step 1: Write the tests:**
  - `crd_applies_and_becomes_established`: server-side apply the generated CRD (field manager `secretspec-test`); wait ≤ 60 s for condition `Established=True`; any API error (e.g. CEL cost exceeded) fails with the server message.
  - `valid_fixtures_accepted`: in a fresh namespace `secretspec-crd-test-<random>`, `create` each valid fixture with dry-run → `Ok`.
  - `invalid_fixtures_rejected`: each invalid fixture (both layers) dry-run → `Err` with HTTP 422 whose message contains `expect`.
  - `dry_run_materialises_expected_defaults`: `v01` returns `spec.deletionPolicy == "Delete"`, `spec.secret.type == "Opaque"`, and `spec.fields[0]` has no `length` and no `charset`; `v03` returns `algorithm == "ed25519"`, `keys == {privateKey: ssh-privatekey, publicKey: ssh-publickey}`, no `rsaBits`, no `comment`; `v06` returns `keys.username/password == username/password`.
  - `type_change_rejected_on_update`: create `u01/old.yaml` for real, then replace with `new.yaml` → 422 containing `secret.type is immutable`.
  - Teardown deletes the test namespace (best effort, also on failure); the CRD is left installed (idempotent).
- [ ] **Step 2: Run locally** against kind 1.34: `kind create cluster --name secretspec-api --image kindest/node:v1.34.<latest patch>@sha256:<digest>`; `SECRETSPEC_TEST_KUBECONFIG=$(kind get kubeconfig-path …)` (or export the kubeconfig to a file) and run the command above → expect failures only where rules need adjustment.
- [ ] **Step 3: Fix discrepancies in the source of truth.** If the server rejects the CRD for CEL cost, tighten bounds first; if a rule cannot fit the budget, move it to `resolve()` only, mark its fixture `# layer: runtime` (extend `Layer`), and note it in the rule's doc comment. If server and client CEL disagree, the server wins.
- [ ] **Step 4: Run** again on 1.34 and on the newest stable minor → PASS on both.
- [ ] **Step 5: CI**: add job `server-validation` (matrix: 1.34 and newest stable kind node images pinned by digest) using a SHA-pinned kind action; runs the command from the Interfaces block.
- [ ] **Step 6: `docs/development.md`**: prerequisites (Rust toolchain, Docker or Podman, kind), the exact commands from Step 2, and how to run each test layer.
- [ ] **Step 7: Commit** `test(api): server-side CRD validation on kind`.

---

### Task 8: Sign-off package — STOP

**Files:** none new (PR only).

- [ ] **Step 1: Full verification:** `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check && cargo xtask crdgen --check` and the Task 7 suite on both Kubernetes versions → all green.
- [ ] **Step 2: Open PR** `api-freeze` → `main` titled `API freeze: secretspec.jabbas.eu/v1alpha1`. The description lists, for the owner to review:
  1. the generated CRD YAML path;
  2. `docs/fingerprints.md`;
  3. the frozen defaults (copy the Global Constraints line);
  4. the rule/message table from Task 3;
  5. every rule moved to runtime-only in Task 7 Step 3 (or "none");
  6. the golden fingerprint literals.
- [ ] **Step 3: STOP.** Do not start plan 2. The owner's PR approval is the sign-off required by design §4.5; merge only after it.

---

## Carried forward to plan 2 (not part of this milestone)

- Planner behaviour when the `fingerprints` annotation is missing or unparseable on an operator-labelled Secret: MUST NOT silently regenerate; report `Ready=False` and leave values untouched (exact reason name to be decided in plan 2).
- `entropy_bits` feeds the `LowEntropy` Warning Event.
- `SecretValue` is the only type allowed to carry generated material.
