# Packaging, Release and v0.1.0 Implementation Plan (plan 4 of 4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the working operator into a publishable product: multi-arch image, Helm chart (OCI), signed release pipeline with release candidates, user documentation and examples, chart-based e2e — then release `v0.1.0-rc.1`, test it in the owner's home lab (`test-secretspec`), and tag `v0.1.0`.

**Architecture:** Binaries are cross-compiled per architecture (`x86_64`/`aarch64` musl, static) and copied into a distroless `static:nonroot` image; the chart renders the generated CRD as a template, RBAC for both scope modes, and the Deployment. A `cargo xtask chart-test` renders the chart for several value sets and asserts the rendered objects in Rust. A tag-triggered workflow builds, scans, signs (cosign keyless), attests SBOMs, pushes image + chart to ghcr, and creates the GitHub Release.

**Tech Stack:** cargo-zigbuild (or `cross`) for musl targets, Docker Buildx in CI / Podman locally, distroless `static-debian12:nonroot` pinned by digest, Helm 3 (pinned), cosign, syft, grype (or trivy), GitHub Actions (SHA-pinned), Renovate.

**Spec:** `docs/design/2026-10-18-secretspec-operator-design.md` (§2 definition of done, §9 RBAC, §10 packaging/release/licence, §12 home-lab test, §13 docs, §14 observability).

## Global Constraints

- Owner instruction for this plan (2026-10-20): execute autonomously, including releasing `v0.1.0-rc.1`, running the home-lab test, and tagging `v0.1.0` when the rc passes. Feature work happens on branch `packaging`, merged `--no-ff` into `main` and pushed after the final review; tags are created on `main`.
- Never name any other secret-generator project, fork, company or person anywhere (README acknowledgement is one sentence without names: "SecretSpec Operator was inspired by earlier Kubernetes secret-generator projects."). SPDX `Apache-2.0` headers on `.rs` files; Helm templates/YAML carry `# SPDX-License-Identifier: Apache-2.0` as first comment where comments are allowed.
- Image: `ghcr.io/jabbas/secretspec-operator:<tag>` for `linux/amd64` and `linux/arm64`; tags only `vX.Y.Z` and `vX.Y.Z-rc.N`; **no `latest`**. Base images pinned by digest. Runs as UID/GID 65532, read-only root filesystem works.
- Chart: `charts/secretspec-operator`, published to `oci://ghcr.io/jabbas/charts/secretspec-operator`; chart `version` = `appVersion` = tag without `v` (e.g. `0.1.0-rc.1`). CRD rendered from `files/crd/…yaml` as a template (`crds.install`, `crds.keep` → `helm.sh/resource-policy: keep`); never a `crds/` directory.
- RBAC rendered by the chart equals design §9 exactly; namespace mode renders a Role+RoleBinding per listed namespace and no ClusterRole for secrets/secretspecs; the lease Role lives in the release namespace in both modes.
- Signing: cosign keyless with GitHub OIDC; identity `https://github.com/jabbas/secretspec-operator/.github/workflows/release.yml@refs/tags/<tag>`, issuer `https://token.actions.githubusercontent.com`. Docs show these exact commands.
- CI/release runners pinned to `ubuntu-24.04` (not `ubuntu-latest`); all actions pinned by full SHA with `# vX` comment.
- Home-lab safety: before ANY command against the home lab, verify `kubectl config current-context` points at the API server `https://kube.home:6443` (cluster `k8s`); if not, STOP — never touch a work cluster. Use an explicit `--context`/`--kubeconfig` on every command. Do not use Flux; do not touch any namespace other than `test-secretspec`; the only cluster-scoped objects created are the SecretSpec CRD (and nothing else — chart in namespace mode renders no ClusterRole). Teardown removes everything created.
- No `latest` tags, no force pushes, no history rewrites. Never print Secret values into logs, PR text or reports (test-generated values may appear only inside test assertions).

## Review Focus

- **Chart RBAC drift.** Expected: rendered rules equal §9 for both modes; namespace mode has zero cluster-scoped RBAC. Pinned by `chart_rbac_cluster_mode`, `chart_rbac_namespace_mode` (Task 2).
- **Image runs under PSA `restricted` with read-only root FS on both arches.** Pinned by `e2e_install_restricted_namespace` (Task 3) and the release smoke step (Task 5).
- **Helm upgrade or pod restart changes values.** Expected: never. Pinned by `e2e_upgrade_and_restart_keep_values` (Task 3) and home-lab steps (Task 7).
- **CRD lifecycle.** Expected: `helm uninstall` keeps the CRD (and every SecretSpec) when `crds.keep: true`; upgrade updates the CRD. Pinned by `e2e_uninstall_keeps_crd` (Task 3).
- **Release reproducibility/verification.** Expected: the documented `cosign verify` commands succeed for image and chart of `v0.1.0-rc.1`. Pinned by Task 6 Step 4.

---

## File Structure

```
build/Containerfile                         # distroless static nonroot, COPY per-arch binary
xtask/src/main.rs                           # + `dist` (zigbuild binaries), `chart-test`
charts/secretspec-operator/Chart.yaml
charts/secretspec-operator/values.yaml
charts/secretspec-operator/values.schema.json
charts/secretspec-operator/README.md
charts/secretspec-operator/templates/{_helpers.tpl,crd.yaml,serviceaccount.yaml,rbac.yaml,deployment.yaml,service.yaml,servicemonitor.yaml,NOTES.txt}
charts/secretspec-operator/files/crd/secretspecs.secretspec.jabbas.eu.yaml   # existing, generated
crates/secretspec-operator/tests/e2e.rs     # #[ignore] chart-based e2e (helm + kind)
examples/*.yaml                             # 7 copy-paste SecretSpecs
README.md
docs/{installation,api-reference,operations,security}.md
docs/development.md                         # + image, chart, e2e, release sections
.github/workflows/ci.yml                    # runner pins, chart-test, e2e, examples check
.github/workflows/release.yml
renovate.json
CHANGELOG.md                                # generated section for 0.1.0
Cargo.toml (workspace.package.version = "0.1.0")
```

---

### Task 1: Static binaries and container image

**Files:** Create `build/Containerfile`; modify `xtask/src/main.rs`, `xtask/Cargo.toml`, root `Cargo.toml` (`[workspace.package] version = "0.1.0"`, members inherit), `.dockerignore`/`.containerignore` (both, identical), `docs/development.md`.

**Interfaces:**
- `cargo xtask dist --target <x86_64-unknown-linux-musl|aarch64-unknown-linux-musl> [--release]` builds the operator with cargo-zigbuild (fail with an install hint if missing) into `target/dist/<arch>/secretspec-operator` where `<arch>` is `amd64|arm64`, stripped, `--locked`.
- `build/Containerfile`: `ARG TARGETARCH`; `FROM gcr.io/distroless/static-debian12:nonroot@sha256:<digest>`; `COPY target/dist/${TARGETARCH}/secretspec-operator /secretspec-operator`; `USER 65532:65532`; `ENTRYPOINT ["/secretspec-operator"]`; OCI labels (`org.opencontainers.image.source=https://github.com/jabbas/secretspec-operator`, `.licenses=Apache-2.0`, `.version`, `.revision` via build args, `.title`, `.description`).
- The binary needs no writable filesystem and no CA bundle beyond what rustls/webpki roots embed (in-cluster API uses the mounted SA CA).

- [ ] **Step 1:** `git switch -c packaging` from up-to-date `main`.
- [ ] **Step 2:** Implement `xtask dist`; build both targets locally; `file target/dist/*/secretspec-operator` → statically linked ELF for x86-64 and aarch64.
- [ ] **Step 3:** Build the image locally with Podman for the host arch (`podman build --platform linux/amd64 -f build/Containerfile -t secretspec-operator:dev .`); run `podman run --rm --read-only --user 65532:65532 secretspec-operator:dev --help` → exit 0 and flag list.
- [ ] **Step 4:** Full gate (`cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check && cargo audit && cargo xtask crdgen --check`); `docs/development.md` gains "Building the image".
- [ ] **Step 5: Commit** `build: static musl binaries and distroless image`.

---

### Task 2: Helm chart and `cargo xtask chart-test`

**Files:** Create everything under `charts/secretspec-operator/` listed above; modify `xtask` (+ `chart-test`), `.github/workflows/ci.yml` (job `chart`: setup-helm pinned, `cargo xtask chart-test`).

**Interfaces:**
- `values.yaml` = design §10.3 plus: `leaderElection.enabled: true`, `replicas: 1`, `resyncInterval: 1h`, `concurrency: 4`, `logLevel: info`, `logFormat: json`, `http.port: 8080`, `resources: {requests: {cpu: 10m, memory: 32Mi}, limits: {memory: 128Mi}}`, `terminationGracePeriodSeconds: 30`, `podSecurityContext: {runAsNonRoot: true, runAsUser: 65532, runAsGroup: 65532, fsGroup: 65532, seccompProfile: {type: RuntimeDefault}}`, `securityContext: {allowPrivilegeEscalation: false, readOnlyRootFilesystem: true, capabilities: {drop: [ALL]}}`, `serviceAccount: {create: true, name: "", annotations: {}}`, `metrics.service.enabled: true`, `serviceMonitor: {enabled: false, interval: 30s, labels: {}}`, image block per design (`global.imageRegistry`, `image.registry`, `repository`, `tag` default appVersion, `digest`, `pullPolicy: IfNotPresent`), `imagePullSecrets: []`, `podAnnotations`, `podLabels`, `nodeSelector`, `tolerations`, `affinity`, `priorityClassName`, `watchNamespaces: []`, `crds: {install: true, keep: true}`.
- `values.schema.json` validates types and enums (`logFormat` json|text, `pullPolicy`, `watchNamespaces` array of DNS-1123 labels).
- Deployment: env `WATCH_NAMESPACES` (comma-joined), `LEADER_ELECTION`, `POD_NAME`/`POD_NAMESPACE` from the downward API, `RESYNC_INTERVAL`, `HTTP_BIND=0.0.0.0:<port>`, `LOG_LEVEL`, `LOG_FORMAT`, `CONCURRENCY`; startup/liveness on `/healthz`, readiness on `/readyz`; `automountServiceAccountToken: true`; image ref per design §10.3 composition (digest wins over tag, `global.imageRegistry` wins over `image.registry`). `leaderElection.enabled: false` with `replicas > 1` → `fail` with a clear message.
- RBAC exactly design §9: cluster mode ClusterRole+ClusterRoleBinding (secretspecs, status, finalizers, secrets, events); namespace mode one Role+RoleBinding per listed namespace with the same rules; lease Role+RoleBinding in `.Release.Namespace` (`coordination.k8s.io` leases get/create/update) in both modes.
- CRD template: `{{- if .Values.crds.install }}` + `.Files.Get` of the generated YAML, adding `helm.sh/resource-policy: keep` when `crds.keep`; chart labels added without altering the spec.
- `cargo xtask chart-test` (needs `helm`): `helm lint` (default values and namespace-mode values), then `helm template` for value sets {default, namespace mode `[a,b]`, digest + global registry, serviceMonitor on, crds off} and Rust assertions on the parsed objects:
  - `chart_rbac_cluster_mode`, `chart_rbac_namespace_mode` (rules equal §9 exactly; namespace mode: no ClusterRole/ClusterRoleBinding at all; Roles in `a` and `b`; lease Role in release namespace);
  - `chart_image_reference` (`ghcr.io/jabbas/secretspec-operator:<appVersion>`, digest form, mirror form);
  - `chart_security_context_restricted`, `chart_downward_api_env`, `chart_probes`, `chart_crd_keep_annotation`, `chart_crd_matches_generated_file` (rendered CRD spec equals `files/crd/…` spec), `chart_rejects_multi_replica_without_leader_election`, `chart_no_latest_tag`.
- `charts/secretspec-operator/README.md`: values table (generated or hand-written), install commands.

- [ ] **Step 1:** Write `chart-test` assertions first; run → FAIL (no chart).
- [ ] **Step 2:** Implement the chart; run `cargo xtask chart-test` → PASS; `helm lint` clean.
- [ ] **Step 3:** CI job `chart` (helm pinned, runner `ubuntu-24.04`); pin every existing CI job to `ubuntu-24.04`.
- [ ] **Step 4:** Full gate. **Step 5: Commit** `feat(chart): Helm chart with RBAC for both scope modes`.

---

### Task 3: Examples, chart-based e2e on kind

**Files:** Create `examples/{app-admin-password,traefik-basic-auth,garage-rpc-secret,fernet-key,ssh-deploy-key,rotated-shared-token,rds-compatible-password}.yaml`; create `crates/secretspec-operator/tests/e2e.rs` (+ reuse `tests/support`); modify `crates/secretspec-api/tests/` (examples are validated like valid fixtures — server dry-run in `server_validation`, `resolve` in a unit test), `.github/workflows/ci.yml` (job `e2e`).

**Interfaces:**
- Examples per design §13 (each with a header comment explaining when to use it; `Retain` for app-persisted credentials; Traefik example uses `keys.htpasswd: users`; Garage example: one SecretSpec with `rpc-secret` (`bytes: 32, encoding: hex`) and `access-key-id` (`prefix: GK, length: 24, charset: hex`) plus `secret-access-key` (`bytes: 32, encoding: hex`); RDS example uses `charset: ascii`, `exclude: "/@\" "`, `require` 1/1/1/1; rotated token uses `rotation.interval: 30d`).
- e2e harness: env `SECRETSPEC_E2E_KUBECONFIG` and `SECRETSPEC_E2E_IMAGE` (image already loaded into kind); installs the chart from the working tree with `helm` into namespace `secretspec-system`, `image.repository/tag` overridden, `image.pullPolicy=Never`. Tests (`#[ignore]`, `--test-threads=1`):
  - `e2e_examples_generate` — apply every `examples/*.yaml` into an app namespace labelled `pod-security.kubernetes.io/enforce=restricted`; all Ready, Secrets present; operator pod Ready under restricted PSA in `secretspec-system` (`e2e_install_restricted_namespace` asserts the release namespace is also `restricted`).
  - `e2e_upgrade_and_restart_keep_values` — record data; `helm upgrade` changing `logLevel` and `resyncInterval`; delete the operator pod; wait Ready → all data byte-identical and Secret `resourceVersion` unchanged.
  - `e2e_uninstall_keeps_crd` — `helm uninstall` → CRD and SecretSpecs remain, Secrets remain; reinstall → no value changes.
  - `e2e_namespace_mode` — reinstall with `watchNamespaces=[app]` → SecretSpec in another namespace gets no status; operator logs contain no `403`/`Forbidden`.
  - `e2e_logs_contain_no_values` — `kubectl logs` of the operator pod contain none of the generated values (raw/base64).
- CI job `e2e` (matrix 1.34 + newest, pinned kind action/images): `cargo xtask dist --target x86_64-unknown-linux-musl --release`, build image with Docker Buildx `--load`, `kind load docker-image`, run `cargo test -p secretspec-operator --test e2e -- --ignored --test-threads=1`.

- [ ] **Step 1:** Write examples + validation tests (resolve + server dry-run) → run the server suite locally on kind (Podman provider, owner-approved) → PASS.
- [ ] **Step 2:** Write e2e; build image locally (Podman), `kind load image-archive` (Podman: `podman save` → `kind load image-archive`); run on kind 1.34.11 and 1.37.0 → PASS. Fix operator/chart bugs in separate `fix(...)` commits (no planner semantic changes; if needed, stop and report).
- [ ] **Step 3:** CI job `e2e`; full gate. **Step 4: Commit** `test(e2e): chart-based end-to-end tests and examples`.

---

### Task 4: User documentation

**Files:** Create/replace `README.md`, `docs/installation.md`, `docs/api-reference.md`, `docs/operations.md`, `docs/security.md`, `charts/secretspec-operator/README.md` (if not complete), `CHANGELOG.md`; modify `docs/development.md` (release section).

Content requirements (design §13 + carry-forwards):
- `README.md`: what/why (3–5 sentences), 5-line quickstart (`helm install … oci://ghcr.io/jabbas/charts/secretspec-operator --version 0.1.0`, `kubectl apply -f examples/app-admin-password.yaml`, read the Secret), feature list, status (`v1alpha1`), links to docs, licence, the single acknowledgement sentence (no names).
- `installation.md`: chart install/upgrade/uninstall, values table, cluster vs namespace mode (namespace mode is a real RBAC boundary; specs elsewhere are ignored), registry mirror / `imagePullSecrets` / digest pinning, CRD lifecycle (`crds.keep`, deleting the CRD deletes every SecretSpec), exact `cosign verify` commands for image and chart with the real identity, supported Kubernetes 1.34+, PSA `restricted` compatible, 2 replicas with leader election (non-leader reports ready; PDB does not protect the leader), `terminationGracePeriodSeconds`.
- `api-reference.md`: every field, default and validation (generated from or checked against the CRD descriptions; include the frozen messages table from `constants.rs`), Secret contract (labels/annotations), behaviour table (design §7.2 rows 1–19 in user language).
- `operations.md`: manual rotation = delete the Secret (or one key); consumers must reload (Secret volume refresh vs env vars, mention generic reloader tools without names); scheduled rotation semantics (first rotation one interval after first observation); `deletionPolicy` guidance (`Retain` for app-persisted credentials; with `Delete` an uninstall/reinstall or GitOps prune yields new values); troubleshooting via `kubectl describe ss` conditions/events (every reason incl. `GenerationFailed`, `PasswordTruncated`, `FingerprintsReset`, `AnchorReset`, `WaitingForGarbageCollection`); apply failures retry every 30 s; metrics list (incl. one `next_rotation` series per SecretSpec with rotation); `LOG_LEVEL` can never enable `kube_client` output and why; local run needs `--leader-election false`; Helm users: the common labels helper emits `app.kubernetes.io/managed-by: Helm`, which `secret.labels` rejects — show a working snippet; bcrypt 72-byte note.
- `security.md`: threat model, RBAC (no Secret `delete`), what is cached (Secret metadata only; data only read on reconcile), value-redaction guarantees, adoption note, RSA advisory ignore rationale, release signing + SBOM + `cosign verify`/`cosign verify-attestation` commands, reporting vulnerabilities (GitHub private advisories).
- `CHANGELOG.md`: `## 0.1.0` with feature summary.

- [ ] **Step 1:** Write docs; every command copy-pasteable; every value/flag cross-checked against the chart and code.
- [ ] **Step 2:** `markdownlint`-clean if available (otherwise skip), links resolve (relative paths exist). **Step 3: Commit** `docs: user documentation, examples guide and changelog`.

---

### Task 5: Release workflow and Renovate

**Files:** Create `.github/workflows/release.yml`, `renovate.json`; modify `docs/development.md` (release procedure).

**Interfaces:**
- Trigger: `push` tags `v[0-9]+.[0-9]+.[0-9]+` and `v[0-9]+.[0-9]+.[0-9]+-rc.[0-9]+`. `permissions: contents: write, packages: write, id-token: write, attestations: write`. Runner `ubuntu-24.04`.
- Jobs: `verify` (tag version without `v` == workspace version for final tags; for rc tags the base version matches; full gate incl. `test-slow`) → `build` (zigbuild both targets, upload artifacts) → `image` (Buildx multi-arch `linux/amd64,linux/arm64`, tags `ghcr.io/jabbas/secretspec-operator:<tag>` only, labels, push; scan the pushed digest with grype/trivy failing on high/critical; syft SPDX SBOM per arch; `cosign sign --yes <ref>@<digest>`; `cosign attest --type spdxjson`; then `cosign verify` with the exact documented identity) → `chart` (`helm package` with `--version <semver> --app-version <semver>`, `helm push` to `oci://ghcr.io/jabbas/charts`, `cosign sign` the chart digest, verify) → `release` (GitHub Release `<tag>`, `prerelease: true` for rc, notes generated from conventional commits since the previous tag — use GitHub's generated notes or git-cliff, builder's choice; attach SBOMs and checksums).
- Smoke: after `image`, run the pushed image for each arch (`docker run --platform linux/arm64 … --help` via QEMU) → exit 0.
- `renovate.json`: cargo, github-actions (keep SHA pins with version comments), Containerfile digests, Helm values image digest not pinned (released by us); group minor/patch; schedule weekly.

- [ ] **Step 1:** Write the workflow; `actionlint` locally if available (install via `go install` only if Go exists; otherwise skip and note).
- [ ] **Step 2:** Dry-run what can run locally (`helm package` with an rc version, `cosign version` not required).
- [ ] **Step 3: Commit** `ci: signed release workflow and Renovate`.

---

### Task 6: Merge and release candidate

- [ ] **Step 1:** Final whole-branch review (controller), fixes, then merge `packaging` `--no-ff` into `main`, push, CI green.
- [ ] **Step 2:** Tag `v0.1.0-rc.1` on `main` (annotated, message `v0.1.0-rc.1`), push the tag, watch `release.yml` to success.
- [ ] **Step 3:** Ensure packages are public: `gh api /user/packages/container/secretspec-operator` and `…/charts%2Fsecretspec-operator`; if `visibility != public`, set public (`gh api -X PATCH … -f visibility=public` or report the UI step if the API refuses).
- [ ] **Step 4:** From the workstation: `cosign verify` image and chart with the documented commands → OK; `docker buildx imagetools inspect` (or `skopeo inspect --raw`) shows both platforms; `helm pull oci://ghcr.io/jabbas/charts/secretspec-operator --version 0.1.0-rc.1` works anonymously.
- [ ] **Step 5:** If any step fails: fix on a branch → merge → `v0.1.0-rc.2` (never move or delete tags).

---

### Task 7: Home-lab test (design §12)

Run from the owner's workstation; obey the Home-lab safety constraint.

- [ ] **Step 1:** Verify context: `kubectl config view --minify -o jsonpath='{.clusters[0].cluster.server}'` == `https://kube.home:6443`; record the context name and use `--context <name>` (and `helm --kube-context <name>`) on every command. Otherwise STOP and report.
- [ ] **Step 2:** `kubectl create namespace test-secretspec` (label `pod-security.kubernetes.io/enforce=restricted`); `helm install secretspec oci://ghcr.io/jabbas/charts/secretspec-operator --version 0.1.0-rc.N -n test-secretspec --set watchNamespaces={test-secretspec}` after `cosign verify` of image and chart.
- [ ] **Step 3:** Scenarios (design §11 list, manual): apply all examples into `test-secretspec` → Ready; restart operator pod → no value change; delete one key → only that key changes; delete a Secret → new values; `Retain` example: delete + recreate the SecretSpec → same values; namespace isolation: a SecretSpec in another namespace is NOT created (do not create one — instead verify via `kubectl auth can-i --as=system:serviceaccount:test-secretspec:<sa> list secrets -A` → no); rotation: patch `generated-at` on the rotated-token Secret to 31 days ago, touch a label → rotated; operator logs contain no generated values.
- [ ] **Step 4:** Teardown: `helm uninstall secretspec -n test-secretspec`; delete namespace `test-secretspec`; `kubectl delete crd secretspecs.secretspec.jabbas.eu` (only this CRD); verify nothing with label `app.kubernetes.io/managed-by=secretspec-operator` remains cluster-wide.
- [ ] **Step 5:** Write `docs/releases/v0.1.0-home-lab-test.md` (date, rc tag, Kubernetes/Talos versions, each scenario result — no values), commit on `main` via a small branch merge.

---

### Task 8: Final release

- [ ] **Step 1:** If Task 7 passed on `v0.1.0-rc.N`: tag `v0.1.0` on the same commit as the passing rc (annotated), push; watch `release.yml`; verify signatures and anonymous pull as in Task 6 Step 4.
- [ ] **Step 2:** Update `README.md` quickstart version if it differs; final summary for the owner (what was released, links, any parked findings).
