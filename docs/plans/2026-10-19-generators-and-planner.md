# Generators and Pure Planner Implementation Plan (plan 2 of 4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement every value generator and the pure reconciliation planner `plan()` in `secretspec-core`, covering the design's behaviour table 1:1 with table-driven unit tests — no Kubernetes I/O.

**Architecture:** `secretspec-core` gains `generate/` (characters, bytes, SSH key pairs, htpasswd — all generic over an injected CSPRNG) and `plan/` (slot inventory of an existing Secret, per-slot keep/regenerate decisions, rotation, adoption/conflict, desired Secret metadata, status and events). `plan()` is a pure function of the `SecretSpec`, the existing `Secret`, the current time and an RNG; production passes the OS RNG, tests pass a seeded RNG so results are reproducible.

**Tech Stack:** Rust 2024 (MSRV 1.89), existing workspace crates (`secretspec-api`, `secretspec-core`), k8s-openapi 0.28 (`v1_34`) types `Secret`, `ObjectMeta`, `OwnerReference`, `Condition`, `Time`; `ssh-key` (features for ed25519, ecdsa p256/p384/p521, rsa, OpenSSH encoding), `bcrypt`, `data-encoding`, a `rand_core`-compatible CSPRNG API consistent with the chosen `ssh-key` release (OS RNG in production), `rand_chacha` (dev only, seeded tests).

**Spec:** `docs/design/2026-10-18-secretspec-operator-design.md` (approved; read §4.2–§4.4, §5, §6, §7). Frozen inputs from plan 1: `docs/fingerprints.md`, `crates/secretspec-api/src/{resolve.rs,constants.rs}`, `crates/secretspec-core/src/fingerprint.rs`.

**Scope:** Plan 3 (operator binary: controller wiring, SSA, status patch, events, leader election, metrics) and plan 4 (chart, release pipeline, docs, e2e, home-lab test) come after. Do not add `kube` client/runtime features to normal dependencies here.

## Global Constraints

- Every `.rs` file starts with `// SPDX-License-Identifier: Apache-2.0`. Never name any other secret-generator project, fork, company or person anywhere.
- `unsafe_code = forbid`; clippy `-D warnings` (pedantic warn); no `unwrap()`/`expect()`/`panic!`/`unreachable!` reachable from production paths outside tests (`unreachable!` already in `password_mac` may stay: HMAC accepts any key length).
- Frozen and MUST NOT change: everything in `docs/fingerprints.md`, all `MSG_*` constants, CRD schema and defaults. This plan only adds code and new constants.
- Production randomness comes only from the OS CSPRNG; all generator APIs take `rng: &mut R` where `R` is a cryptographic RNG trait object/generic — never a global RNG inside generators.
- Generated material lives only in `SecretValue` (or in the `Secret` being built); it never appears in `Debug`/`Display`, errors, events, status or logs. Event messages list key names only.
- bcrypt: `$2y$`, cost `BCRYPT_COST` (12). The stored htpasswd line is `<username>:<bcrypt hash>` with NO trailing newline (it is the HMAC key of the frozen htpasswd fingerprint).
- SSH private key: OpenSSH format, unencrypted, PEM label `OPENSSH PRIVATE KEY`, trailing newline as produced by the library. Public key: one `authorized_keys` line `<type> <base64>[ <comment>]` with trailing `\n`.
- Encodings (`fields[].encoding`): RFC 4648 standard alphabets, padded: `hex` lowercase, `base64`, `base64url`, `base32` (upper-case, padded).
- `cargo deny` / `cargo audit` stay green; if the RSA crate pulled by `ssh-key` triggers RUSTSEC-2023-0071 (timing side channel in RSA private-key *operations*), add a targeted ignore with the justification "the operator only generates RSA keys and never performs private-key operations" in both `deny.toml` and `.cargo/audit.toml`.
- Tests that generate RSA 4096 keys are slow in debug builds; keep exactly one such test and mark it `#[ignore = "slow: RSA 4096"]`, run it in CI with `--release -- --ignored rsa_4096`. All other RSA tests use 2048.
- Conventional commits; branch `generators-planner` from `main`; finishes with a PR the owner merges.

## Review Focus

- **Metadata loss must never rotate.** A managed Secret whose fingerprints annotation is missing, not JSON, or has a bad entry keeps every present value (fingerprints re-stamped from the spec) and emits Warning `FingerprintsReset`. Pinned by `fingerprints_lost_keeps_values` (Task 6).
- **Re-applying the planner's own output is a no-op.** Feeding `plan()`'s desired Secret back as `existing` yields `SecretAction::UpToDate` and no events for every valid fixture. Pinned by `planner_is_idempotent_for_every_fixture` (Task 7).
- **Type mismatch after adoption.** A retained Secret with type `Opaque` adopted by a spec with `kubernetes.io/basic-auth` must not loop on an immutable-field error. Expected: `Conflict` with a message telling the user to delete the Secret. Pinned by `type_mismatch_is_conflict` (Task 7).
- **Empty values.** A key present with an empty value is treated as missing (regenerated), including the SSH public key (re-derived, not regenerated) and the htpasswd line (recomputed). Pinned by row-8 tests and `ssh_public_key_missing_is_rederived` (Task 6).
- **Huge clock skew / anchor in the future.** A `generated-at` annotation in the future or unparseable must not cause immediate or endless rotation. Expected: unparseable → anchor reset to `now` (no rotation, Warning `AnchorReset`); future → no rotation, requeue at `anchor + interval`. Pinned by `rotation_anchor_edge_cases` (Task 7).

---

## File Structure

```
crates/secretspec-api/src/constants.rs            # + ANNOTATION_KEYS, event reasons, condition reasons
crates/secretspec-core/Cargo.toml                 # + ssh-key, bcrypt, data-encoding, rand_core (+ OS RNG), k8s-openapi, jiff/time via k8s-openapi
crates/secretspec-core/src/lib.rs
crates/secretspec-core/src/generate/mod.rs        # GenerateError, re-exports
crates/secretspec-core/src/generate/chars.rs      # generate_chars
crates/secretspec-core/src/generate/bytes.rs      # generate_bytes
crates/secretspec-core/src/generate/ssh.rs        # generate_ssh, public_key_line
crates/secretspec-core/src/generate/htpasswd.rs   # htpasswd_line
crates/secretspec-core/src/plan/mod.rs            # plan(), Plan, SecretAction, PlannedEvent
crates/secretspec-core/src/plan/keys.rs           # KeyMap (ANNOTATION_KEYS) parse/emit
crates/secretspec-core/src/plan/inventory.rs      # what an existing Secret holds per slot
crates/secretspec-core/src/plan/decide.rs         # per-slot keep/regenerate/re-derive
crates/secretspec-core/src/plan/rotation.rs       # anchor, due, next
crates/secretspec-core/src/plan/ownership.rs      # managed? conflict? adopt? wait-for-gc? ownerRefs
crates/secretspec-core/src/plan/status.rs         # status + Ready condition
crates/secretspec-core/tests/generators.rs
crates/secretspec-core/tests/planner_rows.rs      # one test per behaviour-table row
crates/secretspec-core/tests/planner_props.rs     # idempotence, review-focus cases
crates/secretspec-core/tests/support/mod.rs       # spec builders, seeded RNG, fixed clock
docs/design/2026-10-18-secretspec-operator-design.md   # §5 + §7.2/§7.4 additions (Task 1, Task 7)
```

---

### Task 1: Key-map annotation and new constants

**Files:**
- Modify: `crates/secretspec-api/src/constants.rs`, `docs/design/2026-10-18-secretspec-operator-design.md` (§5 annotation list, §7.4 events)
- Create: `crates/secretspec-core/src/plan/mod.rs` (module skeleton only), `crates/secretspec-core/src/plan/keys.rs`, tests in `keys.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces in `secretspec_api::constants`:
  - `ANNOTATION_KEYS: &str = "secretspec.jabbas.eu/keys"` — records where role-based values currently live so a `keys.*` rename moves the value instead of regenerating it.
  - Event reasons: `EVENT_GENERATED = "Generated"`, `EVENT_REGENERATED = "Regenerated"`, `EVENT_ROTATED = "Rotated"`, `EVENT_ADOPTED = "Adopted"`, `EVENT_CONFLICT = "Conflict"`, `EVENT_INVALID_SPEC = "InvalidSpec"`, `EVENT_LOW_ENTROPY = "LowEntropy"`, `EVENT_FINGERPRINTS_RESET = "FingerprintsReset"`, `EVENT_ANCHOR_RESET = "AnchorReset"`, `EVENT_APPLY_FAILED = "ApplyFailed"` (used by plan 3).
  - Condition: `CONDITION_READY = "Ready"`; reasons `REASON_GENERATED = "Generated"`, `REASON_CONFLICT = "Conflict"`, `REASON_INVALID_SPEC = "InvalidSpec"`, `REASON_APPLY_FAILED = "ApplyFailed"`, `REASON_WAITING_FOR_GC = "WaitingForGarbageCollection"`.
- Produces in `secretspec_core::plan::keys`:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
  pub enum Role { SshPrivateKey, SshPublicKey, BasicAuthUsername, BasicAuthPassword, BasicAuthHtpasswd }
  // wire strings: "ssh/private-key", "ssh/public-key", "basic-auth/username", "basic-auth/password", "basic-auth/htpasswd"
  #[derive(Clone, Debug, Default, PartialEq, Eq)]
  pub struct KeyMap(pub BTreeMap<Role, String>);
  impl KeyMap {
      pub fn for_spec(spec: &ResolvedSpec) -> Self;          // empty for Fields
      pub fn to_annotation(&self) -> String;                 // compact JSON, keys sorted by wire string
      pub fn from_annotation(s: &str) -> Self;               // tolerant: invalid JSON → empty; unknown roles / invalid key names ignored
  }
  ```
- Design update (§5): add `secretspec.jabbas.eu/keys` — "JSON map from role (`ssh/private-key`, `ssh/public-key`, `basic-auth/username`, `basic-auth/password`, `basic-auth/htpasswd`) to the Secret key currently holding it; absent for `fields`. Used only to move values when `keys.*` is renamed; a missing or unreadable map means 'the keys named in the spec'." §7.4: add Warning reasons `FingerprintsReset`, `AnchorReset`.

- [ ] **Step 1:** Create branch `git switch -c generators-planner` from up-to-date `main`.
- [ ] **Step 2: Failing tests** in `keys.rs`: `for_spec_ssh_defaults` (`ssh-privatekey`, `ssh-publickey`), `for_spec_basic_auth_traefik` (htpasswd → `users`), `for_spec_fields_is_empty`, `annotation_round_trip_sorted_compact` (`{"ssh/private-key":"a","ssh/public-key":"b"}`), `from_annotation_is_tolerant` (`"x"`, `"[]"`, unknown role, key name `..`, non-string value → those entries dropped, others kept).
- [ ] **Step 3:** Run `cargo test -p secretspec-core keys` → FAIL.
- [ ] **Step 4:** Implement constants, `keys.rs`, design edits.
- [ ] **Step 5:** Full gate `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check && cargo xtask crdgen --check` → PASS (CRD unchanged).
- [ ] **Step 6: Commit** `feat(core): key-map annotation and event/condition constants`.

---

### Task 2: Character and byte generators

**Files:**
- Create: `crates/secretspec-core/src/generate/{mod.rs,chars.rs,bytes.rs}`, `crates/secretspec-core/tests/generators.rs`, `crates/secretspec-core/tests/support/mod.rs`
- Modify: `crates/secretspec-core/Cargo.toml`, `src/lib.rs`, root `Cargo.toml` (workspace deps)

**Interfaces:**
- Consumes: `secretspec_api::resolve::{CharsParams, BytesParams, Require}`, `Encoding`, `SecretValue`.
- Produces:
  ```rust
  pub fn generate_chars<R: CryptoRng + ?Sized>(p: &CharsParams, rng: &mut R) -> Result<SecretValue, GenerateError>;
  pub fn generate_bytes<R: CryptoRng + ?Sized>(p: &BytesParams, rng: &mut R) -> Result<SecretValue, GenerateError>;
  #[derive(Debug, thiserror::Error)]
  pub enum GenerateError { #[error("alphabet has fewer than 2 characters")] AlphabetTooSmall,
      #[error("no {0} characters in the alphabet")] EmptyClass(&'static str),
      #[error("random number generator failed")] Rng, #[error("ssh key generation failed: {0}")] Ssh(String),
      #[error("bcrypt failed")] Bcrypt }
  ```
  (`CryptoRng` = the cryptographic RNG trait of the `rand_core` version used by `ssh-key`; tests/support exposes `fn seeded_rng(seed: u64) -> impl CryptoRng`.)
- Algorithm (normative, design §4.2): uniform index in `0..n` by rejection sampling on bytes (draw `u8`/`u16` as needed, reject values `>= n * floor(MAX/n)`, then `% n`); for each class with `require > 0` draw that many from `alphabet ∩ class`; fill the remaining `length − Σrequire` from the full alphabet; Fisher–Yates shuffle with the same RNG (index for position `i` drawn uniformly from `0..=i`); then `prefix + body + suffix`. Classes: lower `a-z`, upper `A-Z`, digits `0-9`, symbols = ASCII punctuation (`u8::is_ascii_punctuation`).
- Bytes: draw `count` bytes, encode with `data-encoding` (`HEXLOWER`, `BASE64`, `BASE64URL`, `BASE32`), then prefix/suffix.

- [ ] **Step 1: Failing tests** (`tests/generators.rs`):
  - `chars_length_and_alphabet`: for default params (32, alphanumeric) output is 32 bytes, all in alphabet; with prefix `GK` and suffix `-x`, output starts/ends accordingly and total length is 32+3.
  - `chars_require_counts_hold`: v12-like params (ascii minus `/@"`, require 1/1/1/1) over 1000 seeds: every output has ≥1 of each class and none of `/@"`; also require 8/8/8/8 with length 32 → exactly ≥8 each.
  - `chars_uniformity_smoke`: alphabet `ab`, length 1, 10 000 draws with a seeded RNG → count of `a` within 4 900..=5 100.
  - `chars_errors`: alphabet `a` → `AlphabetTooSmall`; hex alphabet with `require.upper = 1` → `EmptyClass("upper")`.
  - `chars_deterministic_with_seed`: same seed → same output; different seeds → different output.
  - `bytes_encodings`: `count 32` → hex len 64 lowercase `[0-9a-f]`; base64 len 44 ending `=`; base64url len 44 no `+`/`/`; base32 len 56 `[A-Z2-7=]`; prefix/suffix applied; decoding (strip prefix/suffix) yields 32 bytes.
  - `generated_values_redact`: `format!("{:?}", v)` == `"<redacted>"` for both generators.
- [ ] **Step 2:** Run `cargo test -p secretspec-core --test generators` → FAIL.
- [ ] **Step 3:** Implement; add deps (`data-encoding`, `rand_core`, OS RNG provider; `rand_chacha` dev-dep).
- [ ] **Step 4:** Run → PASS; full gate.
- [ ] **Step 5: Commit** `feat(core): character and byte generators`.

---

### Task 3: SSH key pair and htpasswd generators

**Files:**
- Create: `crates/secretspec-core/src/generate/{ssh.rs,htpasswd.rs}`
- Modify: `crates/secretspec-core/tests/generators.rs`, `Cargo.toml`s, `deny.toml`/`.cargo/audit.toml` (only if the RSA advisory fires), `.github/workflows/ci.yml` (release-mode slow test step)

**Interfaces:**
- Consumes: `SshAlgorithmParams`, `EcdsaCurve`, `BCRYPT_COST`, `SecretValue`, `GenerateError`.
- Produces:
  ```rust
  pub struct SshKeyPair { pub private_key: SecretValue /* OpenSSH PEM */, pub public_key: String /* authorized_keys line + \n */ }
  pub fn generate_ssh<R: CryptoRng + ?Sized>(alg: SshAlgorithmParams, comment: &str, rng: &mut R) -> Result<SshKeyPair, GenerateError>;
  /// Re-derives the public line from an existing private key with the given comment; Err if the private key does not parse.
  pub fn public_key_line(private_key: &[u8], comment: &str) -> Result<String, GenerateError>;
  /// Returns true if `private_key` parses and its algorithm/size equals `alg`.
  pub fn private_key_matches(private_key: &[u8], alg: SshAlgorithmParams) -> bool;
  /// `<username>:<$2y$12$…>` with no trailing newline; salt (16 bytes) drawn from `rng`.
  pub fn htpasswd_line<R: CryptoRng + ?Sized>(username: &str, password: &SecretValue, rng: &mut R) -> Result<SecretValue, GenerateError>;
  /// True if `line` is `<username>:<hash>` and bcrypt-verifies `password`.
  pub fn htpasswd_verifies(line: &[u8], username: &str, password: &SecretValue) -> bool;
  ```
  The OpenSSH private key comment is set to the same comment (empty allowed).

- [ ] **Step 1: Failing tests:**
  - `ssh_ed25519_round_trip`: generate with comment `deploy@app`; private key starts with `-----BEGIN OPENSSH PRIVATE KEY-----`; `public_key` starts with `ssh-ed25519 ` and ends with ` deploy@app\n`; `public_key_line(private, "deploy@app") == public_key`; `private_key_matches(.., Ed25519)` true, `(.., Rsa{bits:2048})` false.
  - `ssh_empty_comment`: comment `""` → public line has exactly two space-separated fields + `\n`.
  - `ssh_rsa_2048_and_ecdsa_curves`: rsa 2048 → `ssh-rsa `; ecdsa P-256/P-384/P-521 → `ecdsa-sha2-nistp256|384|521 `; each `private_key_matches` its own params only.
  - `ssh_rsa_4096` (`#[ignore = "slow: RSA 4096"]`): `private_key_matches(.., Rsa{bits:4096})`.
  - `ssh_comment_change_keeps_key`: `public_key_line(private, "new")` has the same base64 blob as the original line.
  - `public_key_line_rejects_garbage`: `b"not a key"` → `Err`.
  - `htpasswd_format_and_verify`: line for `admin` matches regex `^admin:\$2y\$12\$[./A-Za-z0-9]{53}$`, has no `\n`, `htpasswd_verifies(line, "admin", pw)` true, false for another password or username `root`; same seed → same line (salt from RNG).
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Implement with `ssh-key` and `bcrypt` (`hash_with_salt` + `$2y$` format). Handle the RSA advisory per Global Constraints if it fires.
- [ ] **Step 4:** Run → PASS; `cargo test -p secretspec-core --release --test generators -- --ignored ssh_rsa_4096` → PASS; full gate. Add CI step running the ignored test in release mode.
- [ ] **Step 5: Commit** `feat(core): SSH key pair and htpasswd generators`.

---

### Task 4: Inventory, ownership and rotation helpers

**Files:**
- Create: `crates/secretspec-core/src/plan/{inventory.rs,ownership.rs,rotation.rs}` with `#[cfg(test)]` unit tests
- Modify: `crates/secretspec-core/src/plan/mod.rs`, `Cargo.toml` (k8s-openapi dependency)

**Interfaces:**
- Consumes: `Secret`, `ObjectMeta`, `OwnerReference`, `SecretSpec`, constants, `FingerprintMap`/`ParsedAnnotation`, `KeyMap`.
- Produces:
  ```rust
  // inventory.rs
  pub struct Inventory {                          // view of an existing managed Secret
      pub data: BTreeMap<String, Vec<u8>>,        // empty values removed (treated as missing)
      pub fingerprints: Option<ParsedAnnotation>, // None = annotation absent or not a JSON object
      pub keys: KeyMap,                           // from ANNOTATION_KEYS (tolerant)
      pub generated_at: Anchor,
      pub secret_type: String,                    // "" → "Opaque"
  }
  pub enum Anchor { Absent, Invalid, At(Timestamp) }
  pub fn inventory(secret: &Secret) -> Inventory;

  // ownership.rs
  pub enum Ownership {
      Absent,                    // no Secret
      Owned,                     // labels ours, spec-name == name, and (controller ref uid == spec uid, or spec-uid annotation == spec uid)
      Adoptable,                 // labels ours, spec-name == name, no controller ref, spec-uid differs or absent
      WaitForGc,                 // labels ours, controller ref to another uid
      Conflict(String),          // message: "Secret <name> exists and is not managed by this SecretSpec"
  }
  pub fn ownership(secret: Option<&Secret>, spec: &SecretSpec) -> Ownership;
  pub fn owner_references(spec: &SecretSpec, policy: DeletionPolicy) -> Option<Vec<OwnerReference>>; // Delete → one controller ref (apiVersion secretspec.jabbas.eu/v1alpha1, kind SecretSpec, name, uid, controller true, blockOwnerDeletion true); Retain → None

  // rotation.rs
  pub struct RotationPlan { pub due: bool, pub anchor: Timestamp, pub next: Option<Timestamp>, pub anchor_reset: bool }
  pub fn rotation(interval: Option<Duration>, anchor: &Anchor, now: Timestamp) -> RotationPlan;
  ```
  `Timestamp` is the type wrapped by k8s-openapi 0.28 `Time` (use `Time`'s inner type; do not add a second time crate).
- Rotation semantics: no interval → `due=false, next=None`, anchor kept (or now if absent). Interval and `Absent` → anchor=now, not due. `Invalid` → anchor=now, not due, `anchor_reset=true`. `At(t)`: if `t > now` → not due, `next=t+interval`; else due iff `now >= t+interval`; if due → anchor=now, `next=now+interval` (missed intervals coalesce); else `next=t+interval`.

- [ ] **Step 1: Failing unit tests** in each file covering: inventory drops empty values and parses all three annotations, missing/garbage annotations; every `Ownership` variant (no labels → Conflict; spec-name differs → Conflict; owned via ownerRef; owned via spec-uid annotation under `Retain`; adoptable; controller ref to other uid → WaitForGc); `owner_references` both policies; rotation table above incl. coalescing (anchor 300 days ago, interval 90d → due once, next = now+90d) and future anchor.
- [ ] **Step 2:** Run `cargo test -p secretspec-core plan::` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run → PASS; full gate.
- [ ] **Step 5: Commit** `feat(core): planner inventory, ownership and rotation helpers`.

---

### Task 5: Per-slot decisions

**Files:**
- Create: `crates/secretspec-core/src/plan/decide.rs` with unit tests

**Interfaces:**
- Consumes: Tasks 2–4, `ResolvedSpec`, `expected_fingerprints`, `canonical_htpasswd`, `fingerprint_of`.
- Produces:
  ```rust
  pub enum SlotOutcome { Kept, Generated, Regenerated, Rederived /* ssh public line rewritten, htpasswd recomputed with same password */ }
  pub struct Decision {
      pub data: BTreeMap<String, SecretValue>,     // all generated keys (not literals) under their spec key names
      pub fingerprints: FingerprintMap,            // expected + htpasswd fingerprint; unknown entries carried over
      pub outcomes: BTreeMap<Slot, SlotOutcome>,
      pub fingerprints_reset: bool,                // some present value was kept although its fingerprint was missing/invalid
  }
  pub fn decide<R: CryptoRng + ?Sized>(spec: &ResolvedSpec, inv: Option<&Inventory>, force_all: bool, rng: &mut R) -> Result<Decision, GenerateError>;
  ```
- Rules (normative):
  1. Locate the current value of each role/slot: fields → key `name`; roles → key from `inv.keys` if present there and in data, else the spec key name.
  2. `force_all` (rotation due, or `inv == None`) → generate every slot (`Generated` when `inv == None`, else `Regenerated`).
  3. Slot value missing → generate (`Generated` if there was no Secret, else `Regenerated`).
  4. Stored fingerprint present and equal to expected → keep.
  5. Stored fingerprint present and different → regenerate.
  6. Stored fingerprint absent or invalid (annotation absent, unparseable, or slot in `problems`) while the value is present → keep, stamp expected fingerprint, `fingerprints_reset = true`.
  7. SSH: if private key kept but does not parse or `!private_key_matches` → regenerate the pair. Public line = `public_key_line(private, comment)` always (re-derived when missing or comment changed → `Rederived` if it differs from the stored line, else `Kept`).
  8. basicAuth: password slot per rules 2–6. htpasswd: keep iff password kept AND stored line present AND stored htpasswd fingerprint == `fingerprint_of(canonical_htpasswd(username, password, stored_line, BCRYPT_COST))`; if the fingerprint is absent/invalid but `htpasswd_verifies(line, username, password)` → keep and stamp (`fingerprints_reset`); otherwise recompute (`Rederived` when the password was kept, `Generated`/`Regenerated` when it was new). The htpasswd fingerprint always uses the line actually written.
  9. Unknown fingerprint entries are carried over unchanged; fingerprints of slots no longer in the spec are dropped.

- [ ] **Step 1: Failing unit tests**, one per rule 1–9 (names `rule_1_…` to `rule_9_…`), using seeded RNG and builders from `tests/support` (move builders into a `#[cfg(test)]` module or a small test-support module shared by unit and integration tests).
- [ ] **Step 2:** Run → FAIL. **Step 3:** Implement. **Step 4:** Run → PASS; full gate.
- [ ] **Step 5: Commit** `feat(core): per-slot keep/regenerate decisions`.

---

### Task 6: `plan()` — desired Secret, status, events

**Files:**
- Create: `crates/secretspec-core/src/plan/status.rs`
- Modify: `crates/secretspec-core/src/plan/mod.rs`, `src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn plan<R: CryptoRng + ?Sized>(spec: &SecretSpec, existing: Option<&Secret>, now: Timestamp, rng: &mut R) -> Plan;
  pub struct Plan { pub action: SecretAction, pub status: SecretSpecStatus, pub events: Vec<PlannedEvent>, pub requeue_after: Option<Duration> }
  pub enum SecretAction { Apply(Box<Secret>), UpToDate, Nothing /* conflict, invalid, wait */ }
  pub struct PlannedEvent { pub warning: bool, pub reason: &'static str, pub message: String }
  ```
- Flow: `resolve` (Err → `Nothing`, Ready False `InvalidSpec` with the joined error text, Warning `InvalidSpec`) → `ownership` (`Conflict` → `Nothing`, Ready False `Conflict`, Warning `Conflict`; `WaitForGc` → `Nothing`, Ready False `WaitingForGarbageCollection`, no event, `requeue_after = 5s`) → type check (existing managed Secret whose type ≠ `spec.secret_type` → `Nothing`, Ready False `Conflict`, message `Secret <name> has type <old>; delete it to recreate it with type <new>`, Warning `Conflict`) → rotation → `decide` (`GenerateError` → `Nothing`, Ready False `InvalidSpec`, Warning) → build desired Secret → compare → status/events.
- Desired Secret: `metadata.name/namespace` = spec's; labels = `spec.secret.labels` + `managed-by` + `spec-name`; annotations = `spec.secret.annotations` + `fingerprints` (`to_annotation`) + `keys` (omitted for Fields) + `generated-at` (anchor, RFC 3339 UTC, seconds precision) + `spec-uid`; `ownerReferences` per policy; `type` = resolved type; `data` = literals + decision data. Only these fields are set (it is an SSA intent).
- `UpToDate` iff the existing Secret already has: equal `data` (as a whole map), the same values for every label/annotation key in the desired set, and the same controller ownerReference presence/uid. Extra labels/annotations/keys owned by others do not count. (Removed generated/literal keys make `data` differ → `Apply`.)
- Events (key names only, sorted): no existing Secret → `Generated` "generated keys: a, b"; rotation due → `Rotated` "rotated keys: …"; otherwise any `Regenerated`/`Generated` outcome → `Regenerated` "regenerated keys: …"; `Adoptable` → `Adopted` (in addition); `fingerprints_reset` → Warning `FingerprintsReset` "kept existing values for keys: … (fingerprint metadata was missing or invalid)"; `anchor_reset` → Warning `AnchorReset`; any (re)generated key with entropy < 128 bits → Warning `LowEntropy` "keys with less than 128 bits of entropy: …". `Rederived`/`Kept` alone → no event.
- Status: `observedGeneration` = `metadata.generation`; `kind`; `secretName`; `lastGeneratedTime` = now if any slot was (re)generated else previous; `lastRotationTime` = now when rotated else previous; `nextRotationTime` = rotation `next`; Ready condition with `observedGeneration`, `lastTransitionTime` = previous condition's when status+reason unchanged, else now; message for `Generated`: `Secret <name> is up to date`.
- `requeue_after`: `next − now` when rotation is configured (min 1 s), else `None`; `WaitForGc` → 5 s.

- [ ] **Step 1: Failing smoke tests** in `tests/planner_rows.rs` for rows 1 (new spec) and 15 (conflict) only, asserting action variant, data keys, labels/annotations, ownerRef, status Ready reason and events.
- [ ] **Step 2:** Run → FAIL. **Step 3:** Implement. **Step 4:** Run → PASS; full gate.
- [ ] **Step 5: Commit** `feat(core): pure reconciliation planner`.

---

### Task 7: Behaviour table and properties

**Files:**
- Modify: `crates/secretspec-core/tests/planner_rows.rs`, create `crates/secretspec-core/tests/planner_props.rs`, modify `docs/design/2026-10-18-secretspec-operator-design.md` (§7.2 rows 16–19)

**Interfaces:** Consumes `plan()` and helpers. Produces tests only (+ design rows).

Design §7.2 gains rows **[architect]**: 16 fingerprint metadata lost → values kept, fingerprints re-stamped, Warning `FingerprintsReset`; 17 `keys.*` renamed → value moved, no regeneration; 18 retained Secret type differs from spec → `Conflict`, nothing written; 19 rotation anchor unparseable → anchor reset, no rotation, Warning `AnchorReset`.

- [ ] **Step 1: Write one test per row** named `row_01_new_secretspec` … `row_19_anchor_reset` (row 13 asserts ownerRef presence for `Delete` and absence for `Retain`; row 7 feeds the previous desired Secret back with a fresh RNG seed and asserts `UpToDate`, no events and an unchanged status (including `lastTransitionTime`); row 6 changes SecretSpec labels/annotations only; row 10 uses anchor `now − 91d`, interval 90d; row 11 enables rotation on a Secret with `generated-at = now − 10d`, interval 90d → no rotation, next = anchor+90d). Each row test asserts data changes per key (kept keys byte-equal, changed keys differ), events, Ready reason.
- [ ] **Step 2: Property tests** in `planner_props.rs`: `planner_is_idempotent_for_every_fixture` (every `crates/secretspec-api/tests/fixtures/valid/*.yaml` with name/namespace/uid/generation set: plan → feed back → `UpToDate`, no events), `values_never_logged` (Debug of `Plan` contains no generated value — check by searching for each generated value's bytes), `type_mismatch_is_conflict`, `rotation_anchor_edge_cases` (future anchor, unparseable anchor), `fingerprints_lost_keeps_values` (annotation removed / set to `"x"` / one slot invalid).
- [ ] **Step 3:** Run `cargo test -p secretspec-core --test planner_rows --test planner_props`; fix the implementation (not the tests) until PASS; full gate.
- [ ] **Step 4:** Update design §7.2 with rows 16–19.
- [ ] **Step 5: Commit** `test(core): behaviour table and planner properties`.

---

### Task 8: PR — STOP

- [ ] **Step 1:** Full gate plus `cargo test -p secretspec-core --release -- --ignored`.
- [ ] **Step 2:** With owner permission, push `generators-planner` and open PR `generators-planner → main` titled `Generators and pure planner`; body lists new annotation `secretspec.jabbas.eu/keys`, new event reasons, behaviour rows 16–19, and any advisory ignore added.
- [ ] **Step 3: STOP** — the owner merges. Plan 3 is written after merge.
