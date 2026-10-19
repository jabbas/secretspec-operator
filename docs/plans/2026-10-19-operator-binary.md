# Operator Binary Implementation Plan (plan 3 of 4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the `secretspec-operator` binary: a kube-rs controller that reads `SecretSpec`s and their Secrets, calls the pure `plan()`, server-side-applies the result, writes status and Events, exposes metrics and health, and runs with Lease-based leader election — proven by in-process integration tests against a real kind cluster.

**Architecture:** New crate `crates/secretspec-operator` (binary). Pure glue (configuration, the foreign-key view of an existing Secret, ApplyFailed status, requeue policy) is unit-tested; Kubernetes I/O lives in a thin reconcile function wired into one `Controller` per watched scope (cluster-wide, or one per listed namespace). Decisions stay in `secretspec_core::plan` — the binary never decides whether to generate.

**Tech Stack:** Rust 2024 (MSRV 1.89), kube 4.x (`runtime`, `client`, `rustls-tls`), k8s-openapi 0.28 (`v1_34`), tokio, futures, tracing + tracing-subscriber (JSON), clap (derive, env), axum (one HTTP server: `/metrics`, `/healthz`, `/readyz`), prometheus-client, thiserror (lib modules) / anyhow (`main` only), jiff (already in use), a maintained Lease-based leader-election crate compatible with kube 4 (or a small in-house module, see Task 5).

**Spec:** `docs/design/2026-10-18-secretspec-operator-design.md` (approved; read §5–§9, §14). Pure planner contract: `crates/secretspec-core/src/plan/mod.rs` (`plan`, `Plan`, `SecretAction`, `PlannedEvent`) and §7.2 rows 1–19.

**Scope:** Plan 4 (container image, Helm chart, release pipeline, user docs, chart-based e2e, home-lab test) follows. No Dockerfile, chart or release workflow here.

## Global Constraints

- Every `.rs` file starts with `// SPDX-License-Identifier: Apache-2.0`. Never name any other secret-generator project, fork, company or person.
- `unsafe_code = forbid`; clippy `-D warnings`; no `unwrap()`/`expect()`/`panic!` in production paths.
- Frozen: API `v1alpha1` (CRD, `MSG_*`), `docs/fingerprints.md`. Additions to constants allowed.
- **Never log, trace, event or expose** a `Secret`, its `data`, the `SecretAction::Apply` body, an `Inventory`, or any `kube::Error` body for a Secret write. Log the `SecretSpec` namespace/name, the action variant, reasons and key names only.
- Field manager for every write: `secretspec-operator`. Secrets are written ONLY with server-side apply (`Patch::Apply`, `force: true`). The operator never deletes Secrets (no `delete` verb in RBAC).
- RBAC used by the binary must be exactly design §9: `secretspecs` get/list/watch; `secretspecs/status` get/patch/update; `secretspecs/finalizers` update; `secrets` get/list/watch/create/patch/update; `events.k8s.io/events` create/patch; `coordination.k8s.io/leases` get/create/update in the operator namespace. Integration tests run the controller with a ServiceAccount bound to exactly these rules.
- In namespace mode the operator MUST NOT issue any cluster-wide list or watch (`Api::all`) for any resource.
- `plan()` (and therefore RSA/bcrypt work) runs inside `tokio::task::spawn_blocking`; a fresh `os_seeded_rng()` per reconcile.
- Conventional commits; branch `operator-binary` from `main`; at the end, per the owner's standing instruction for this new project, merge `--no-ff` into `main` and push after the final review passes.

## Review Focus

- **Apply fails after generation.** Expected: status keeps the previous `observedGeneration`, `lastGeneratedTime`, `lastRotationTime`, `nextRotationTime`; Ready=False `ApplyFailed`; only the Warning `ApplyFailed` event (planned Generated/Regenerated/Rotated/Adopted suppressed); retried with backoff. Pinned by `apply_failed_status_keeps_previous` (Task 3) and `apply_failure_is_reported_and_retried` (Task 6).
- **A key added to the Secret by someone else.** Expected: never removed by the operator, never causes a re-apply loop, never hides a key the spec needs. Pinned by `foreign_keys_are_ignored` (Task 3) and `foreign_key_survives_and_no_reapply_loop` (Task 6).
- **Operator restart / leader change mid-life.** Expected: no value changes. Pinned by `restart_changes_nothing` (Task 6).
- **Namespace mode isolation.** Expected: SecretSpecs outside the list are never reconciled and no cluster-wide request is made (the restricted RBAC would 403). Pinned by `namespace_mode_ignores_other_namespaces` (Task 6).
- **Secret being deleted while reconciling** (`deletionTimestamp` set, e.g. foreground deletion). Expected: no apply, requeue. Pinned by `secret_being_deleted_is_not_applied` (Task 3) and covered in Task 6 row "delete Secret".

---

## File Structure

```
Cargo.toml                                        # + member, + workspace deps
crates/secretspec-core/src/plan/mod.rs            # Task 2: generation failures retryable
crates/secretspec-api/src/constants.rs            # Task 2: REASON_GENERATION_FAILED, EVENT_GENERATION_FAILED, FIELD_MANAGER
crates/secretspec-operator/Cargo.toml
crates/secretspec-operator/src/main.rs            # anyhow; parse config, init logging, run
crates/secretspec-operator/src/config.rs          # clap Config
crates/secretspec-operator/src/logging.rs         # tracing init (json|text, level)
crates/secretspec-operator/src/view.rs            # owned_view(): hide foreign keys from the planner
crates/secretspec-operator/src/outcome.rs         # apply-failed status, event filtering, requeue policy
crates/secretspec-operator/src/reconcile.rs       # Ctx, reconcile(), error_policy(), Error
crates/secretspec-operator/src/controller.rs      # Controller wiring per scope, Secret→SecretSpec mapper
crates/secretspec-operator/src/leader.rs          # Lease-based leader election
crates/secretspec-operator/src/server.rs          # axum: /metrics /healthz /readyz
crates/secretspec-operator/src/metrics.rs         # prometheus-client registry, store-backed collectors
crates/secretspec-operator/tests/support/mod.rs   # kind client, namespaces, RBAC-restricted client, controller harness
crates/secretspec-operator/tests/integration.rs   # #[ignore] scenarios against kind
.github/workflows/ci.yml                          # + operator-integration job
docs/development.md                               # run locally, run integration tests
```

---

### Task 1: Crate skeleton, configuration and logging

**Files:** Create `crates/secretspec-operator/{Cargo.toml,src/main.rs,src/config.rs,src/logging.rs}`; modify root `Cargo.toml`.

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Debug, clap::Parser)]
  pub struct Config {
      /// Comma-separated namespaces; empty = all namespaces.
      #[arg(long, env = "WATCH_NAMESPACES", value_delimiter = ',', default_value = "")] pub watch_namespaces: Vec<String>,
      #[arg(long, env = "LEADER_ELECTION", default_value_t = true, action = clap::ArgAction::Set)] pub leader_election: bool,
      #[arg(long, env = "POD_NAMESPACE")] pub lease_namespace: Option<String>,   // required when leader_election
      #[arg(long, env = "POD_NAME")] pub pod_name: Option<String>,               // lease holder identity; falls back to hostname
      #[arg(long, env = "LEASE_NAME", default_value = "secretspec-operator")] pub lease_name: String,
      #[arg(long, env = "RESYNC_INTERVAL", default_value = "1h", value_parser = parse_duration)] pub resync_interval: Duration,
      #[arg(long, env = "HTTP_BIND", default_value = "0.0.0.0:8080")] pub http_bind: SocketAddr,
      #[arg(long, env = "LOG_LEVEL", default_value = "info")] pub log_level: String,
      #[arg(long, env = "LOG_FORMAT", default_value = "json", value_enum)] pub log_format: LogFormat, // json | text
      #[arg(long, env = "CONCURRENCY", default_value_t = 4)] pub concurrency: u16,
  }
  impl Config { pub fn validate(&self) -> Result<(), ConfigError>; pub fn watch_namespaces(&self) -> Vec<String> /* trimmed, non-empty, deduplicated, sorted */; }
  pub fn parse_duration(s: &str) -> Result<Duration, String>;  // `<n>s|m|h`, n ≥ 1, resync 1m..=24h
  pub fn init_logging(level: &str, format: LogFormat) -> Result<(), anyhow::Error>;
  ```
  `main` parses and validates config, initialises logging, logs the version (`env!("CARGO_PKG_VERSION")`) and config (no secrets exist in config), and exits 0 (wiring comes in Task 5).

- [ ] **Step 1:** `git switch -c operator-binary` from up-to-date `main`.
- [ ] **Step 2: Failing tests** (`config.rs` `#[cfg(test)]`): defaults; `--watch-namespaces " b, a,,a"` → `["a","b"]`; `WATCH_NAMESPACES=""` → all namespaces; `parse_duration` accepts `30s`, `1m`, `1h`, `24h`, rejects `0s`, `59s` for resync (below 1m), `25h`, `1d`, `abc`; `validate` fails when `leader_election` and no `lease_namespace` with message `POD_NAMESPACE (or --lease-namespace) is required when leader election is enabled`; `--leader-election false` parses.
- [ ] **Step 3:** Run `cargo test -p secretspec-operator` → FAIL. **Step 4:** Implement. **Step 5:** Run → PASS; full gate (`cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check && cargo audit && cargo xtask crdgen --check`).
- [ ] **Step 6: Commit** `feat(operator): crate skeleton, configuration and logging`.

---

### Task 2: Generation failures are retryable (core)

**Files:** Modify `crates/secretspec-api/src/constants.rs`, `crates/secretspec-core/src/plan/mod.rs`, `crates/secretspec-core/tests/planner_props.rs`, design §4.6/§7.4.

**Interfaces:**
- Produces constants `REASON_GENERATION_FAILED = "GenerationFailed"`, `EVENT_GENERATION_FAILED = "GenerationFailed"`, `FIELD_MANAGER = "secretspec-operator"`.
- `plan()` maps a `GenerateError` (internal failure: SSH library, bcrypt) to `SecretAction::Nothing`, Ready False reason `GenerationFailed` with message `value generation failed: <GenerateError Display>` (Display carries no values), Warning `GenerationFailed`, `requeue_after = 30s`, previous times preserved — instead of `InvalidSpec`. Design §4.6 Ready table and §7.4 Warning row gain `GenerationFailed` **[architect]**.

- [ ] **Step 1: Failing test** `generation_failure_is_retryable`: a resolved spec cannot be made to fail generation deterministically, so extract the mapping as `pub fn halt_for_generate_error(err: &GenerateError, spec: &SecretSpec, kind: SecretKind, now: Timestamp) -> Plan` and test it directly for `GenerateError::Ssh(..)` and `GenerateError::Bcrypt`: action `Nothing`, Ready False `GenerationFailed`, one Warning `GenerationFailed` event, `requeue_after == 30s`, previous `lastGeneratedTime`/`lastRotationTime`/`nextRotationTime` preserved.
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** PASS; full gate. **Step 5: Commit** `feat(core): generation failures are retryable`.

---

### Task 3: Pure operator glue — foreign-key view, apply-failed outcome, requeue policy

**Files:** Create `crates/secretspec-operator/src/{view.rs,outcome.rs}` with `#[cfg(test)]` tests; modify `main.rs` (module declarations).

**Interfaces:**
- Consumes: `secretspec_api::{resolve, ResolvedSpec, constants::*}`, `secretspec_core::{Plan, PlannedEvent, SecretAction, plan::keys::KeyMap}`, `k8s_openapi::api::core::v1::Secret`.
- Produces:
  ```rust
  // view.rs
  /// The existing Secret as the planner should see it: data keys owned only by other
  /// field managers are removed, so they never make the Secret look out of date.
  /// A key is hidden only if BOTH: (a) the spec does not name it (literal key, field name,
  /// sshKeyPair.keys.*, basicAuth.keys.*, or a key recorded in ANNOTATION_KEYS) and
  /// (b) the `secretspec-operator` Apply entry in managedFields exists and does not list it
  /// under `f:data`. If our managedFields entry is absent or unparseable, nothing is hidden.
  pub fn owned_view(existing: &Secret, spec: &SecretSpec) -> Secret;

  // outcome.rs
  pub fn apply_failed_status(previous: Option<&SecretSpecStatus>, generation: Option<i64>, message: &str, now: Timestamp) -> SecretSpecStatus;
  /// Events to publish after a successful apply/no-op: the planned ones unchanged.
  /// After a failed apply: planned Warning events except FingerprintsReset/AnchorReset are kept,
  /// Normal events (Generated, Regenerated, Rotated, Adopted) are dropped, and one Warning
  /// `ApplyFailed` is added.
  pub fn events_after_apply_failure(planned: &[PlannedEvent], message: &str) -> Vec<PlannedEvent>;
  pub fn requeue_after(planned: Option<Duration>, resync: Duration) -> Duration;   // min(planned, resync), at least 1 s; None → resync
  pub fn apply_failed_message(err: &kube::Error) -> String;  // "apply of Secret failed: <code> <reason>" — never the error body
  ```
  `apply_failed_status`: copies the previous status (or default), keeps previous `observedGeneration` and all times, sets Ready False `ApplyFailed` with `message`, condition `observedGeneration` = `generation`, `lastTransitionTime` = previous if Ready was already False/ApplyFailed else `now`.

- [ ] **Step 1: Failing tests:** `foreign_keys_are_ignored` (managedFields with our Apply entry owning `f:data.f:password`; data has `password` and `extra` → view has only `password`); `spec_named_keys_are_never_hidden` (our entry lacks `password` but the spec names it → kept); `missing_or_garbage_managed_fields_hides_nothing`; `recorded_role_key_is_kept` (KeyMap annotation names `auth` after rename to `users` → `auth` kept); `apply_failed_status_keeps_previous`; `events_after_apply_failure_filters_normal_events`; `requeue_after_caps_and_floors`; `apply_failed_message_has_no_body` (construct a `kube::Error::Api` whose message contains `c2VjcmV0` → output contains code/reason, not the message).
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** PASS; full gate. **Step 5: Commit** `feat(operator): foreign-key view, apply-failure outcome and requeue policy`.

---

### Task 4: Reconcile function (Kubernetes I/O)

**Files:** Create `crates/secretspec-operator/src/reconcile.rs`.

**Interfaces:**
- Consumes: Tasks 2–3, `secretspec_core::{plan, generate::os_seeded_rng}`, `kube::runtime::events::{Recorder, Reporter, Event, EventType}`.
- Produces:
  ```rust
  pub struct Ctx { pub client: kube::Client, pub recorder: Recorder, pub resync: Duration, pub metrics: Arc<Metrics> }
  #[derive(Debug, thiserror::Error)]
  pub enum Error { #[error("kubernetes API error: {0}")] Kube(String) /* code + reason only */, #[error("random number generator unavailable")] Rng,
                   #[error("planner task failed")] Join, #[error("{0}")] ApplyFailed(String) }
  pub async fn reconcile(spec: Arc<SecretSpec>, ctx: Arc<Ctx>) -> Result<Action, Error>;
  pub fn error_policy(spec: Arc<SecretSpec>, err: &Error, ctx: Arc<Ctx>) -> Action;  // requeue 30 s, warn log with ns/name + error Display
  ```
- Reconcile algorithm (normative):
  1. `SecretSpec` has `deletionTimestamp` → `Action::await_change()`.
  2. `GET` the Secret by name in the spec's namespace (direct API read, not a cache — avoids acting on stale data right after our own apply). 404 → `None`.
  3. Secret has `deletionTimestamp` → `Action::requeue(5s)`, nothing else.
  4. `view = existing.map(|s| owned_view(&s, &spec))`.
  5. `rng = os_seeded_rng()` (err → `Error::Rng`); `plan` in `spawn_blocking` with clones and `now = Timestamp::now()` (join err → `Error::Join`).
  6. `Apply(secret)` → `Api::<Secret>::namespaced(..).patch(name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(&*secret))`. On error: status = `apply_failed_status(..)`, events = `events_after_apply_failure(..)`, patch status, publish events, return `Err(Error::ApplyFailed(msg))`.
  7. Otherwise (success, `UpToDate`, `Nothing`): patch status with `plan.status` only if it differs from the current status (`Patch::Merge` of `{"status": …}` on the status subresource); publish `plan.events`.
  8. Return `Action::requeue(requeue_after(plan.requeue_after, ctx.resync))`.
  9. Record metrics: reconcile result and duration; generations per reason from published events.
  Event mapping: `PlannedEvent { warning, reason, message }` → `Event { type_: Warning|Normal, reason, note: Some(message), action: "Reconcile", secondary: None }`, reference = the SecretSpec's `ObjectReference`.
  Logging: one `info` line per reconcile with namespace, name, action variant (`Apply|UpToDate|Nothing`), Ready reason, duration; `debug` for skipped status patches. Never log `Secret`/`kube::Error` bodies.

- [ ] **Step 1:** Implement (I/O is verified by Task 6; add unit tests only for helpers you extract, e.g. `planned_to_kube_event`, `status_patch_needed`).
- [ ] **Step 2:** Full gate. **Step 3: Commit** `feat(operator): reconcile with server-side apply, status and events`.

---

### Task 5: Controller wiring, leader election, HTTP server, metrics

**Files:** Create `crates/secretspec-operator/src/{controller.rs,leader.rs,server.rs,metrics.rs}`; modify `main.rs`.

**Interfaces:**
- Produces:
  ```rust
  // controller.rs
  pub async fn run(cfg: Config, client: Client, metrics: Arc<Metrics>, ready: Arc<AtomicBool>, shutdown: impl Future<Output = ()>) -> anyhow::Result<()>;
  pub fn secret_to_spec(secret: Secret) -> Option<ObjectRef<SecretSpec>>;  // label spec-name + namespace; None if label missing
  // leader.rs
  pub async fn run_with_leadership<F>(client: Client, lease_ns: &str, lease_name: &str, holder: &str, work: F) -> anyhow::Result<()> where F: Future<Output = anyhow::Result<()>>;
  // server.rs
  pub async fn serve(bind: SocketAddr, metrics: Arc<Metrics>, ready: Arc<AtomicBool>, shutdown: impl Future<Output = ()>) -> anyhow::Result<()>;
  // metrics.rs
  pub struct Metrics { /* registry, reconcile counter by result, duration histogram, generations counter by reason */ }
  impl Metrics { pub fn new() -> Self; pub fn register_store(&self, store: Store<SecretSpec>); pub fn encode(&self) -> String; }
  ```
- Controller per scope: all namespaces → one `Controller::new(Api::<SecretSpec>::all(..), watcher::Config::default())`; namespace list → one controller per namespace with `Api::namespaced`, run concurrently (`futures::future::try_join_all` / `select_all`). Each controller: SecretSpec stream filtered with `predicates::generation` (status writes do not retrigger); `.watches(Api::<Secret>::{all|namespaced}, watcher::Config::default().labels("app.kubernetes.io/managed-by=secretspec-operator"), secret_to_spec)`; `.with_config(controller::Config::default().concurrency(cfg.concurrency))`; graceful shutdown on SIGTERM/SIGINT.
- Leader election: Lease `coordination.k8s.io/v1` in `lease_ns`, lease duration 15 s, renew every 5 s, holder = pod name. Use a maintained crate compatible with kube 4.x if one exists; otherwise implement a minimal module (acquire if absent/expired, renew with `resourceVersion` precondition, never steal a live lease) with unit tests on the pure expiry logic. Losing the lease → stop controllers and exit the process with a non-zero code (Kubernetes restarts it). With `--leader-election false` controllers start immediately.
- `/healthz` → 200 always while the process runs; `/readyz` → 200 once the Kubernetes client has completed one successful API discovery call and (when leading or leader election disabled) the controllers started, or while standing by as a non-leader; else 503. `/metrics` → OpenMetrics text.
- Metrics (names final): `secretspec_reconcile_total{result="ok|error"}`, `secretspec_reconcile_duration_seconds` (histogram), `secretspec_generations_total{reason="Generated|Regenerated|Rotated"}`, and scrape-time gauges computed from the SecretSpec reflector store(s): `secretspec_secretspecs{ready="true|false|unknown"}` and `secretspec_next_rotation_timestamp_seconds{namespace,name}` (only objects with `status.nextRotationTime`). Store-backed gauges disappear automatically when a SecretSpec is deleted. Design §14 is updated with the final names **[architect]**.

- [ ] **Step 1: Failing unit tests:** `secret_to_spec_uses_label` (namespace + label → ObjectRef; missing label → None); metrics encode contains the metric names; leader expiry logic tests if in-house.
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** PASS; full gate; `cargo run -p secretspec-operator -- --help` lists every flag.
- [ ] **Step 5: Commit** `feat(operator): controller wiring, leader election, metrics and health`.

---

### Task 6: Integration tests on kind

**Files:** Create `crates/secretspec-operator/tests/{support/mod.rs,integration.rs}`; modify `.github/workflows/ci.yml`, `docs/development.md`.

**Interfaces:**
- Consumes the library surface of the operator crate (expose `run`/`Config` via `src/lib.rs` if needed — keep `main.rs` thin).
- Harness: env `SECRETSPEC_TEST_KUBECONFIG` (panic with a clear message when unset; tests are `#[ignore]`, run with `-- --ignored --test-threads=1`). Setup applies the generated CRD, creates namespaces `secretspec-it-<rand>` (+ a second one for isolation), a ServiceAccount + ClusterRole/Role bindings with exactly the RBAC of Global Constraints, and builds a client impersonating that ServiceAccount. Each test starts the controller in-process (`--leader-election false` except the leader test) on a tokio task and stops it at the end; teardown deletes the namespaces.
- Scenarios (one test each, waiting with a timeout ≤ 60 s, polling ≤ 500 ms):
  1. `examples_generate_secrets` — apply valid fixtures v01, v03 (ed25519), v06, v09, v12 → Secrets exist with expected keys, labels, annotations, ownerRef per policy; status Ready True; Event `Generated`.
  2. `restart_changes_nothing` — capture values, stop and restart the controller → identical data and `resourceVersion` unchanged after 5 s.
  3. `deleting_one_key_regenerates_only_it` — remove `password` with a JSON patch → only `password` changes; Event `Regenerated`.
  4. `deleting_the_secret_regenerates_everything` — delete the Secret → recreated with new generated values, same literals.
  5. `retain_survives_spec_recreate` — `Retain` spec: delete SecretSpec → Secret stays; recreate → same values, Event `Adopted`.
  6. `delete_policy_garbage_collects` — `Delete`: delete SecretSpec → Secret gone (wait for GC).
  7. `namespace_mode_ignores_other_namespaces` — controller with `watch_namespaces=[A]` and a client whose RBAC covers only namespace A (Roles, no ClusterRole for secrets/secretspecs) → spec in A reconciled, spec in B never gets status or Secret; controller logs no 403.
  8. `rotation_with_short_interval` — `rotation.interval: 1h` cannot be waited for; instead set `generated-at` to now−2h on a scheduled Secret (status has nextRotationTime) → rotated on the next reconcile (trigger by touching a label on the Secret); Event `Rotated`.
  9. `foreign_key_survives_and_no_reapply_loop` — another field manager adds `extra` via SSA → `extra` survives; Secret `resourceVersion` stable over 10 s.
  10. `conflict_with_unmanaged_secret` — pre-existing unlabelled Secret → Ready False `Conflict`, Secret untouched.
  11. `apply_failure_is_reported_and_retried` — remove the `patch`+`create` verbs on secrets from the test Role, create a spec → Ready False `ApplyFailed`, Event `ApplyFailed`, `observedGeneration` not advanced; restore RBAC → becomes Ready True.
  12. `leader_election_single_active` — start two controllers with leader election on the same Lease → exactly one reconciles (only one holder; status written once), stopping the leader lets the second take over within 30 s.
- CI: add job `operator-integration` (matrix 1.34 + newest, same pinned kind action and node images as `server-validation`) running `cargo test -p secretspec-operator --test integration -- --ignored --test-threads=1`.
- `docs/development.md`: how to run the operator locally against kind (`cargo run -p secretspec-operator -- --leader-election false --log-format text`) and the integration suite (Docker or Podman via `KIND_EXPERIMENTAL_PROVIDER=podman`).

- [ ] **Step 1:** Write the harness and scenarios.
- [ ] **Step 2:** Run locally on kind 1.34 and the newest stable minor (owner allows local Podman + kind; never touch `~/.kube/config`; delete clusters afterwards) → fix implementation bugs (not tests) until PASS; any change to the planner contract needs a controller ruling — report it instead of changing `secretspec-core` semantics.
- [ ] **Step 3:** Full gate; CI job added. **Step 4: Commit** `test(operator): integration tests on kind`.

---

### Task 7: Merge — after final review

- [ ] **Step 1:** Full gate + `cargo test -p secretspec-core --release -- --ignored` + integration suite on both Kubernetes versions.
- [ ] **Step 2:** Merge `--no-ff` into `main` with message `Merge operator binary (plan 3)`, push `main` and `operator-binary`; watch CI to green.
- [ ] **Step 3: STOP** — plan 4 is written next.
