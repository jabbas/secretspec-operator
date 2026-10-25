# Development

## Prerequisites

- Rust toolchain (`rust-toolchain.toml` pins the exact release, currently 1.99.1,
  so CI, the release build and your machine agree; `rustup` installs it on first
  use. Raise it deliberately, in its own commit.)
- `cargo-deny` for `cargo deny check`
- For the server-side tests only: a container runtime — **Docker or Podman** —
  and [kind](https://kind.sigs.k8s.io/) (the CI uses v0.33.0)

## Checks before committing

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
cargo xtask crdgen --check   # `cargo xtask crdgen` regenerates the CRD
```

The CRD (`charts/secretspec-operator/files/crd/secretspecs.secretspec.jabbas.eu.yaml`)
is generated from the Rust types and rules; never edit it by hand.

## Test layers

Invalid fixtures live in `crates/secretspec-api/tests/fixtures/` and declare
which layer rejects them (`# layer: cel|schema`, see `tests/common/mod.rs`).

| Layer | Command | Needs |
|-------|---------|-------|
| Schema structure, CRD golden checks | `cargo test -p secretspec-api --test schema` | nothing |
| CEL rules (client-side evaluation of the generated rules) | `cargo test -p secretspec-api --test cel` | nothing |
| `resolve()` runtime validation and defaults | `cargo test -p secretspec-api --test resolve` | nothing |
| Real API server (CRD acceptance, CEL cost, schema, defaulting, immutability) | see below | a cluster |
| Operator unit tests (config, foreign-key view, outcome, leader logic, metrics) | `cargo test -p secretspec-operator` | nothing |
| Operator against a real API server (reconcile, RBAC, leader election) | see [Operator integration tests](#operator-integration-tests-on-kind) | a cluster |
| Helm chart + image, end to end (examples, upgrade, uninstall, namespace mode) | see [End-to-end tests](#end-to-end-tests-chart--image-on-kind) | a cluster and the image |

`cargo test --workspace` runs the cluster-free layers; both cluster suites are `#[ignore]`d.
Where the client-side tests and a real API server disagree, the server wins and
the Rust types/rules are fixed.

## Server-side validation on kind

The suite applies the generated CRD (server-side apply, field manager
`secretspec-test`), then dry-run-creates every fixture in a throw-away
namespace `secretspec-crd-test-<random>`, which it deletes afterwards. The CRD
is left installed. **Use a disposable cluster** — never point
`SECRETSPEC_TEST_KUBECONFIG` at a cluster you care about. The tests panic if
the variable is unset.

CI runs the suite on Kubernetes 1.34 and 1.37 (kind node images pinned by
digest, see `.github/workflows/ci.yml`). Locally, with the node images
published for kind v0.33.0:

```sh
# Kubernetes 1.34
kind create cluster --name secretspec-api-134 \
  --image kindest/node:v1.34.11@sha256:44e222ee2132dab25ff87301682f89eb82c7880ea3a1bf543bfe9708fd08d67d \
  --kubeconfig "$TMPDIR/secretspec-api-134.kubeconfig"
SECRETSPEC_TEST_KUBECONFIG="$TMPDIR/secretspec-api-134.kubeconfig" \
  cargo test -p secretspec-api --test server_validation -- --ignored --test-threads=1
kind delete cluster --name secretspec-api-134

# Kubernetes 1.37 (newest stable minor)
kind create cluster --name secretspec-api-137 \
  --image kindest/node:v1.37.0@sha256:a1ed56cfb0e7b93589bdf97c8cd566405a265939e3620fc4f5de89adff580ae5 \
  --kubeconfig "$TMPDIR/secretspec-api-137.kubeconfig"
SECRETSPEC_TEST_KUBECONFIG="$TMPDIR/secretspec-api-137.kubeconfig" \
  cargo test -p secretspec-api --test server_validation -- --ignored --test-threads=1
kind delete cluster --name secretspec-api-137
```

`--kubeconfig <file>` makes kind write the kubeconfig to that file instead of
`~/.kube/config`, so your other contexts stay untouched. (Set `TMPDIR` or use
any other path if it is unset.) When a new kind release is out, take the node
images and digests from its release notes and update both this file and the CI
matrix together.

### Docker

Nothing extra: kind uses Docker by default.

### Podman

```sh
podman machine init     # once; add `--cpus 4 --memory 6144` if the defaults are small
podman machine set --rootful   # kind needs a rootful machine on macOS (machine must be stopped)
podman machine start
export KIND_EXPERIMENTAL_PROVIDER=podman
```

Then run the same `kind create cluster …` / `cargo test …` commands as above.
Stop the machine afterwards with `podman machine stop` if you do not need it.

### Running a single server test

```sh
SECRETSPEC_TEST_KUBECONFIG=… cargo test -p secretspec-api --test server_validation \
  invalid_fixtures_rejected -- --ignored --test-threads=1
```

The failure output lists every offending fixture with the server's message.

## Running the operator locally on kind

The binary reads the usual kubeconfig (`KUBECONFIG`, else `~/.kube/config`).
Point `KUBECONFIG` at the file kind wrote so that no other context is used:

```sh
kind create cluster --name secretspec-dev \
  --image kindest/node:v1.37.0@sha256:a1ed56cfb0e7b93589bdf97c8cd566405a265939e3620fc4f5de89adff580ae5 \
  --kubeconfig "$TMPDIR/secretspec-dev.kubeconfig"
export KUBECONFIG="$TMPDIR/secretspec-dev.kubeconfig"
kubectl apply --server-side \
  -f charts/secretspec-operator/files/crd/secretspecs.secretspec.jabbas.eu.yaml
cargo run -p secretspec-operator -- --leader-election false --log-format text \
  --http-bind 127.0.0.1:8080
```

In a second shell (same `KUBECONFIG`):

```sh
kubectl create namespace test
kubectl apply -f crates/secretspec-api/tests/fixtures/valid/v01-minimal-fields.yaml
kubectl -n test get secretspecs,secrets
curl -s localhost:8080/readyz; curl -s localhost:8080/metrics
```

Run as yourself, the operator has your (cluster-admin) rights; the integration
suite below is what checks it with the real, minimal RBAC. With leader
election on you also need `--lease-namespace` and `--pod-name` (macOS has no
hostname file to fall back to). `kind delete cluster --name secretspec-dev`
when done.

## Operator integration tests on kind

`crates/secretspec-operator/tests/integration.rs` runs the operator
in-process (`secretspec_operator::run`) against a real API server. Each test:

- applies the generated CRD (left installed);
- creates two namespaces `secretspec-it-<random>-a` and `-b`, a ServiceAccount
  in `-a` and exactly the RBAC of design §9 — a ClusterRole in cluster mode, or
  only Roles in `-a` in namespace mode; the Lease Role is always in `-a`;
- runs the operator with a client that **impersonates** that ServiceAccount,
  with its own identity and Lease name;
- deletes the namespaces and its ClusterRole/ClusterRoleBinding afterwards,
  also when it fails.

Same cluster requirements as the server suite (disposable cluster, Docker or
Podman with `KIND_EXPERIMENTAL_PROVIDER=podman`); the same kind cluster can
run both suites:

```sh
kind create cluster --name secretspec-op-134 \
  --image kindest/node:v1.34.11@sha256:44e222ee2132dab25ff87301682f89eb82c7880ea3a1bf543bfe9708fd08d67d \
  --kubeconfig "$TMPDIR/secretspec-op-134.kubeconfig"
SECRETSPEC_TEST_KUBECONFIG="$TMPDIR/secretspec-op-134.kubeconfig" \
  cargo test -p secretspec-operator --test integration -- --ignored --test-threads=1
kind delete cluster --name secretspec-op-134
```

and likewise with the 1.37 image. The suite takes about two minutes;
`apply_failure_is_reported_and_retried` alone takes about a minute because it
waits for two 30 s error retries. Tests must run with `--test-threads=1`:
they share the process-wide log capture.

The operator's log is captured (the namespace-mode test asserts that it has
no `403`); set `SECRETSPEC_TEST_LOG=1` and add `--nocapture` to see it:

```sh
SECRETSPEC_TEST_LOG=1 SECRETSPEC_TEST_KUBECONFIG=… \
  cargo test -p secretspec-operator --test integration leader_election_single_active \
  -- --ignored --test-threads=1 --nocapture
```

## Building the image

The image is the prebuilt static musl binary on
`gcr.io/distroless/static-debian12:nonroot` (pinned by digest in
`build/Containerfile`); it runs as UID/GID 65532 and works with a read-only
root filesystem. Binaries are cross-compiled with
[cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild), so no Docker
is needed to build them and the same command works on macOS and Linux.

One-time setup (user-level; versions used by CI and last verified locally):

```sh
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo install --locked cargo-zigbuild@0.23.4 cargo-auditable@0.7.7
python3 -m venv ~/.local/share/zig-venv
~/.local/share/zig-venv/bin/pip install ziglang==0.17.0   # CI: --require-hashes -r .github/requirements/ziglang.txt
export PATH="$HOME/.local/share/zig-venv/bin:$PATH"   # cargo-zigbuild finds `python3 -m ziglang`
```

Build the binaries (stripped, `--locked`) into `target/dist/<arch>/`:

```sh
cargo xtask dist --target x86_64-unknown-linux-musl --release    # target/dist/amd64/
cargo xtask dist --target aarch64-unknown-linux-musl --release   # target/dist/arm64/
file target/dist/*/secretspec-operator   # "statically linked" ELF per arch
```

The binary embeds its dependency list (cargo-auditable); the list survives
stripping. To check it, `syft target/dist/amd64/secretspec-operator` lists the
Rust crates (about 270).

Build and smoke-test the image with Podman (Docker: replace `podman` by
`docker`); `TARGETARCH` selects the binary, and the non-host platform runs
under emulation:

```sh
podman build --platform linux/amd64 \
  --build-arg VERSION=0.1.0-dev --build-arg REVISION="$(git rev-parse HEAD)" \
  -f build/Containerfile -t secretspec-operator:dev .
podman run --rm --read-only --user 65532:65532 secretspec-operator:dev --help
```

`.dockerignore` and `.containerignore` are identical (Docker and Podman read
different names); they send only `target/dist/` to the build.

## End-to-end tests (chart + image) on kind

`crates/secretspec-operator/tests/e2e.rs` installs the Helm chart from the
working tree with the image built above, into namespace `secretspec-system`
(PSA `restricted`), and exercises it as a user would: every `examples/*.yaml`
in a PSA-`restricted` namespace, `helm upgrade` and pod restart (values and
Secret `resourceVersion`s unchanged), `helm uninstall` (CRD, SecretSpecs and
Secrets stay), `watchNamespaces` mode (no 403s), and log redaction.

The harness needs `helm` (or `$HELM`) on the `PATH`, a **disposable, fresh**
cluster (the chart owns the CRD, so a cluster where the server or integration
suite already applied the CRD fails with "invalid ownership metadata" —
`kubectl delete crd secretspecs.secretspec.jabbas.eu` first), and the image
loaded into it:

```sh
cargo xtask dist --target x86_64-unknown-linux-musl --release
podman build --platform linux/amd64 -f build/Containerfile \
  -t localhost/secretspec-operator:e2e .
podman save -o "$TMPDIR/image.tar" localhost/secretspec-operator:e2e

kind create cluster --name secretspec-e2e-134 \
  --image kindest/node:v1.34.11@sha256:44e222ee2132dab25ff87301682f89eb82c7880ea3a1bf543bfe9708fd08d67d \
  --kubeconfig "$TMPDIR/secretspec-e2e-134.kubeconfig"
kind load image-archive "$TMPDIR/image.tar" --name secretspec-e2e-134
SECRETSPEC_E2E_KUBECONFIG="$TMPDIR/secretspec-e2e-134.kubeconfig" \
SECRETSPEC_E2E_IMAGE=localhost/secretspec-operator:e2e \
  cargo test -p secretspec-operator --test e2e -- --ignored --test-threads=1
kind delete cluster --name secretspec-e2e-134
```

(Docker: `docker build … --load` and `kind load docker-image`.) The image is
`<registry>/<repository>:<tag>`; the chart gets `image.pullPolicy=Never`.
The suite takes about 80 s once the cluster is up; run it on 1.37 too (node
image as in the server-side section). `--test-threads=1` is required: the
tests share one release.

## Releasing

A release is a pushed annotated tag on `main`, in plain semver (semver.org, no
`v` prefix): `X.Y.Z` or `X.Y.Z-rc.N`. `.github/workflows/release.yml` does
everything else; nothing is published by hand. The tag, the image tag
(`ghcr.io/jabbas/secretspec-operator:<tag>`), the chart version
(`oci://ghcr.io/jabbas/charts/secretspec-operator`) and the chart `appVersion`
are the same string. No `latest` tag exists.

`0.1.0` is the one exception: it was released (with `0.1.0-rc.1` and
`0.1.0-rc.2`) under the legacy tags `v0.1.0*`, image `:v0.1.0`. Those tags are
never touched; they do not trigger the workflow, which matches only plain
semver tags. Release notes of a final release start at the previous final tag
of either form.

### Bump the versions

Three places carry the release version and must be equal; `cargo xtask
chart-test` (test `chart_version_matches_workspace`) and the workflow's `verify`
job fail otherwise (items 1-3; item 4 is documentation, checked by nothing):

1. `version` in `[workspace.package]` of `Cargo.toml` (then `cargo build` so
   `Cargo.lock` follows);
2. `version` in `charts/secretspec-operator/Chart.yaml`;
3. `appVersion` in `charts/secretspec-operator/Chart.yaml` (a quoted string);
4. the documentation that names the latest published release: the `TAG` and
   `CHART_VERSION` of the verify blocks in `docs/installation.md` and
   `docs/security.md`, and the `--version` of the install examples in
   `README.md` and `docs/installation.md`. These are not checked by any gate;
   update them in the release pull request, so they point at a release that
   exists once the tag is pushed.

Also replace "Unreleased" in the `CHANGELOG.md` heading of the new version with
the release date (for the final release). Do this in a pull request and merge it
to `main`.

**Release candidates do not change any of these files.** The committed versions
are the *final* version (`0.1.1`); the tag `0.1.1-rc.1` is checked against it
(the part before `-rc.N` must equal the workspace version), and the workflow
injects the rc version only while packaging the chart
(`helm package --version 0.1.1-rc.1 --app-version 0.1.1-rc.1`), so the packaged
chart installs the image `…:0.1.1-rc.1`.

### Tag

Tag a commit that is on `main` and has passed CI. Tags are annotated, and a
pushed tag is **never moved or deleted**: a signed artifact for it may already
exist.

```sh
git switch main && git pull --ff-only
git tag -a 0.1.1-rc.1 -m 0.1.1-rc.1      # release candidate
git push origin 0.1.1-rc.1
# after the candidate has been tried out:
git tag -a 0.1.1 -m 0.1.1                # final release
git push origin 0.1.1
```

Verify a published release with the commands in
[Installation](installation.md#verify-the-release-optional-but-recommended) (the signing identity
is `…/release.yml@refs/tags/0.1.1`, i.e. the tag itself).

#### When a release run fails

The workflow never changes what a pushed tag points to, also not when a run is
repeated. The build is not reproducible, so a second build would have a different
digest; therefore the image is first pushed **by digest without a tag**, scanned,
signed and attested, and only then tagged. A re-run that finds the tag already
present resumes with the digest the tag points to instead of rebuilding, and the
job fails if the tag points anywhere else. The chart is handled the same way: a
chart version that already exists in the registry is pulled and signed, never
packaged and pushed again.

The registry lookups behind this (`.github/scripts/registry-lookup.sh`) fail
closed. Right before it creates the image tag, the workflow accepts only an
explicit "manifest unknown / not found" answer as "tag is free"; an access error
stops the job (re-run it). For the chart, an access error is retried, because a
package that was never pushed may be answered that way, and the lookup is repeated
immediately before `helm push`, which would silently overwrite a version.

Use **"Re-run failed jobs"** (not "Re-run all jobs") on the same workflow run,
but only when the cause is transient (registry, Sigstore, runner, scanner
database download) and the failed job is one of:

| Failed job | Re-run is safe because |
|---|---|
| `verify`, `build` | Nothing has been published yet. |
| `image`, before its "Tag the image" step (scan, SBOM, sign, attest) | The tag does not exist yet. A new image is built and pushed untagged; the digest of the failed attempt stays in the registry as an untagged (possibly signed) leftover that no tag refers to. |
| `image`, at or after "Tag the image" | The tag exists and is reused; the image is scanned, signed and verified again (extra signatures on the same digest are harmless). |
| `smoke`, `chart` | The image is fixed. `chart` reuses a chart version that was already pushed. |
| `release` | If the GitHub Release was created but is incomplete, delete that **release** (not the tag) in the UI first; `gh release create` refuses to overwrite one. |
| `helm-index` | Always, once the cause is fixed (see [`helm-index`](#helm-index-the-http-helm-repository)). Re-run only this job; it is idempotent. |

Cut the next candidate (`-rc.2`) or patch version instead, and do not re-tag, when:

- the failure is caused by the code, the chart or the workflow itself (fix it on
  `main` first);
- the scan fails on a vulnerability (on the first attempt the image stays
  untagged and unsigned; on a resume the already tagged and signed image is not
  changed, the release is just never completed, so a fixed image needs a new
  version);
- the workflow stops with "points to …, not to …" (somebody else moved or created
  the tag);
- a published image or chart turns out to be wrong.

### What the workflow does

| Job | Does |
|---|---|
| `verify` | Checks the tag format, that its version matches the workspace version (and the chart files), and that the commit is reachable from `main`; then runs the full gate: `fmt`, `clippy -D warnings`, tests including the ignored RSA tests in release mode, `crdgen --check`, `chart-test`, `cargo deny`, `cargo audit`. |
| `build` | Static musl binaries for amd64 and arm64 (`cargo xtask dist`: cargo-zigbuild through cargo-auditable, so the binary carries its crate list), uploaded as an artifact. |
| `image` | Builds the multi-arch image and pushes it by digest (no tag), scans each platform image with grype (fails on a fixable high or critical finding), creates an SPDX SBOM per platform with syft, signs the index and the platform images with cosign, attests the SBOMs on the platform images, then creates the release tag (only if it is free or already points to that digest) and verifies it all with the documented commands. |
| `smoke` | Runs the pushed image with `--help` on amd64 and on arm64 (QEMU). |
| `chart` | Packages the chart at the release version (or, if that version is already published, pulls it), checks that it renders the image `ghcr.io/jabbas/secretspec-operator:<tag>`, pushes it if new, signs it and verifies the signature. |
| `release` | Creates the GitHub Release (pre-release for `-rc.N`) with generated notes, the chart package, the SBOMs and `SHA256SUMS`. |
| `helm-index` | Adds the version to the HTTP Helm repository `jabbas/helm-charts` after checking the package against the signed OCI chart. See below. |

Write permissions are granted per job: only `image` and `chart` can push packages
and obtain an OIDC token for signing, only `release` can write repository
contents. `helm-index` has a read-only job token and writes only to
`jabbas/helm-charts`, with its own credential.

### `helm-index`: the HTTP Helm repository

`helm-index` (Environment `helm-charts`, runs after `release`) publishes the
chart to the classic Helm repository `https://jabbas.github.io/helm-charts`
(repository `jabbas/helm-charts`, one root `index.yaml`, no packages). Every
entry points at this release's asset
`https://github.com/jabbas/secretspec-operator/releases/download/<tag>/secretspec-operator-<version>.tgz`,
so the job must run after the release exists. Release candidates are listed
too; Helm and Flux hide pre-releases unless asked. In order, it:

1. fails at once if the secret `HELM_CHARTS_TOKEN` is missing;
2. downloads the release asset (the file the index will point at; not a build
   artifact, because artifacts expire after 7 days and a re-run must keep
   working) and asserts, with `::error::` on any mismatch, that it answers
   HTTP 200 after redirects and that its SHA-256 equals (a) the chart-layer
   digest of the pushed, signed OCI chart (read by the digest the `chart` job
   verified, with `crane` pinned in `release.yml`) and (b) the `SHA256SUMS`
   entry of the release;
3. only then checks out `jabbas/helm-charts` with the token (not stored in the
   checkout) and builds the new index
   (`.github/scripts/helm-index.sh`): `helm repo index --url <release download
   base>` runs on the package alone, and the one entry it produces is inserted
   into the existing document with `yq`. helm never rewrites the shared index,
   because `helm repo index --merge` drops entries it finds invalid, adds fields
   to foreign entries and removes unknown fields. The result is compared with
   the old document as data and must differ only by that entry and
   `generated`; anything else (a foreign chart, top-level fields, unknown
   fields) fails the job;
4. commits as `secretspec-operator <version>` (`github-actions[bot]`) and pushes;
   the token is handed to that one `git push` through its environment and is
   never printed. A lost push race (another chart updated the index) is retried
   from the new remote state, at most 5 times, then the job fails;
5. waits up to about 10 minutes until `…/helm-charts/index.yaml` serves the
   version with the right digest. If Pages is slow (the version is not listed
   yet), the job ends with a warning (the index is pushed); check the Pages
   build of `jabbas/helm-charts`. If Pages serves a **different** digest for
   the version, the job fails.

The tools are pinned: `crane` in the `env:` block of `release.yml`, `yq` in
`.github/scripts/install-yq.sh` (both checksum verified). `jq` is the one of the
runner image (it only reads the OCI manifest here).

The index logic is tested without network by
`bash .github/scripts/helm-index-test.sh` (needs `helm`, `yq`, `jq`, `git`,
`python3`; `HELM=/path/to/helm` selects another helm; the CI job
`release-scripts` runs it with helm v3.22.1): new version, same-digest re-run,
different digest, invalid entries, a shared index with a foreign chart, an
invalid foreign entry, unknown fields and top-level `annotations`/`serverInfo`
that must survive, tampered documents that must fail, a push race, bounded
retries, token handling and the Pages check.

It is **idempotent and fail-closed**. A version that is already in the index
with the same digest is a no-op; with a *different* digest the job fails and
nothing is changed (a published version is never edited: cut the next
release candidate). The job cannot change this repository or its packages.

If `helm-index` fails, fix the cause (token expired or missing, Pages down,
`jabbas/helm-charts` unreachable) and **re-run only that job** ("Re-run failed
jobs"); the release, the image and the OCI chart are not touched by it. A re-run
works as long as GitHub allows re-running the workflow run (30 days); it does
not depend on build artifacts. The versions `0.1.0`, `0.1.0-rc.1` and
`0.1.0-rc.2` predate the job and were added to the index by hand (their URLs use
the legacy `v0.1.0*` tags); the job only ever adds, it never rewrites an entry.

#### When a re-run is no longer possible

After 30 days a run cannot be re-run. There is deliberately no
`workflow_dispatch` path: the job needs the `chart` job's verified digest and
the tag ref, and a second entry point would need either a copy of the job or a
reusable workflow whose Environment-secret handling could not be tested here.
Add the version by hand instead, with the same checks, from a **fresh** clone
of `jabbas/helm-charts` (the function resets it to `origin/main`) and your own
git credentials for it (leave `HELM_CHARTS_TOKEN` unset). Needs `cosign`,
`crane`, `jq`, `yq` (mikefarah v4), `helm`, `git` and `sha256sum` (`shasum -a
256` where `sha256sum` is missing). It runs in a subshell with `set -euo
pipefail`, so any failing command or `MISMATCH` stops it before the index is
touched, and it does not close your terminal:

```sh
(
set -euo pipefail
TAG=0.1.1                  # the release tag; the version equals the tag
VERSION="${TAG}"
IDENTITY="https://github.com/jabbas/secretspec-operator/.github/workflows/release.yml@refs/tags/${TAG}"
ISSUER="https://token.actions.githubusercontent.com"
RELEASE_URL="https://github.com/jabbas/secretspec-operator/releases/download/${TAG}"

# 1. Fetch the release asset and check it exactly as the job does.
curl -fsSL -o "secretspec-operator-${VERSION}.tgz" "${RELEASE_URL}/secretspec-operator-${VERSION}.tgz"
SHA="$(sha256sum "secretspec-operator-${VERSION}.tgz" | cut -d' ' -f1)"
CHART_DIGEST="$(cosign verify "ghcr.io/jabbas/charts/secretspec-operator:${VERSION}" \
  --certificate-identity "${IDENTITY}" --certificate-oidc-issuer "${ISSUER}" \
  | jq -er '.[0].critical.image."docker-manifest-digest"')"
LAYER="$(crane manifest "ghcr.io/jabbas/charts/secretspec-operator@${CHART_DIGEST}" \
  | jq -er '.layers[] | select(.mediaType == "application/vnd.cncf.helm.chart.content.v1.tar+gzip") | .digest')"
[ "${LAYER}" = "sha256:${SHA}" ] || { echo "MISMATCH with the signed OCI chart"; exit 1; }
curl -fsSL "${RELEASE_URL}/SHA256SUMS" | grep -Fx "${SHA}  secretspec-operator-${VERSION}.tgz" || { echo "MISMATCH with SHA256SUMS"; exit 1; }

# 2. Add it, with the script of that tag (a checkout of this repository).
git clone git@github.com:jabbas/helm-charts.git helm-charts-fresh
git -C /path/to/secretspec-operator show "${TAG}:.github/scripts/helm-index.sh" > helm-index.sh
source ./helm-index.sh
publish_index helm-charts-fresh "$PWD/secretspec-operator-${VERSION}.tgz" "${VERSION}" "${SHA}" "${RELEASE_URL}"
wait_for_pages https://jabbas.github.io/helm-charts/index.yaml "${VERSION}" "${SHA}"
)
```

The last command exits non-zero if Pages does not serve the version within
about 10 minutes (the index is pushed by then; check the Pages build) or serves
a different digest.

### Verify a release

Follow [Security: Release signing and SBOM](security.md#release-signing-and-sbom)
(`cosign verify` for the image and the chart, `cosign verify-attestation` for the
SBOM of a platform image). Also check that the packages are public
(`docker pull ghcr.io/jabbas/secretspec-operator:<tag>` and `helm pull
oci://ghcr.io/jabbas/charts/secretspec-operator --version <version>` work
without logging in; `helm-index` reads the chart manifest anonymously too) and
that a pre-release is marked as such. For the HTTP channel, run the digest-chain
recipe in [Installation](installation.md#what-is-and-is-not-verified) for the new
version.

### One-time setup and maintenance

- **Package visibility.** After the first release, the `secretspec-operator`
  container package and the `charts/secretspec-operator` package on ghcr.io must
  be public and linked to this repository (package settings). Without that,
  users cannot pull or verify them anonymously.
- **`HELM_CHARTS_TOKEN` (Environment `helm-charts`).** The only credential that
  can write to `jabbas/helm-charts`. Create a **fine-grained personal access
  token**: resource owner `jabbas`, repository access **only**
  `jabbas/helm-charts`, permissions **Contents: Read and write** and
  **Metadata: Read** (nothing else), and a fixed expiry (for example one year).
  Store it as the secret `HELM_CHARTS_TOKEN` of the Environment `helm-charts`
  of this repository (not a repository secret). The Environment's deployment policy allows only the
  release tags (`[0-9]*.[0-9]*.[0-9]*`, which also matches `X.Y.Z-rc.N`), so
  nothing else can read the secret. **Renew the token before it expires**:
  generate a new one with the same settings, replace the secret, revoke the old
  token, and note the next expiry date. An expired or revoked token makes
  `helm-index` fail on the push (or at the check-out); re-run only that job after
  replacing it.
- **Rulesets and Environment** (repository settings, no bypass actors).
  - `immutable-release-tags` applies to all tags (`~ALL`) with the rules
    `deletion`, `update` and `non_fast_forward`: a pushed tag can never be
    moved or deleted, which enforces the "never move a tag" rule.
  - `protect-main` (rules `deletion` and `non_fast_forward`, default branch)
    exists on this repository and on `jabbas/helm-charts`.
  - The Environment `helm-charts` has a deployment tag policy
    (`[0-9]*.[0-9]*.[0-9]*`) and holds `HELM_CHARTS_TOKEN`; only release tags
    can use it.
- **Scanner and cosign versions** are pinned in the `env:` block of
  `release.yml` (so are `crane`, used by `helm-index`, and the URL of the HTTP
  repository). The syft and grype archives are verified against a SHA-256 that
  is pinned next to the version; when bumping one, take the checksum of the
  `linux_amd64.tar.gz` from the `*_checksums.txt` of that upstream release and
  change both. For crane the archive is `go-containerregistry_Linux_x86_64.tar.gz`
  and the checksum file is `checksums.txt` of the go-containerregistry release.
  `yq` is pinned in `.github/scripts/install-yq.sh` (archive
  `yq_linux_amd64.tar.gz`; its checksum is the SHA-256 column of the `checksums`
  file of the release, the column order is in `checksums_hashes_order`). cosign
  is installed by the SHA-pinned `sigstore/cosign-installer`, which verifies the
  download itself. Renovate does not touch these pins.
- **Build tools** in `build` (and in the CI `e2e` job): `cargo-zigbuild` and
  `cargo-auditable` are pinned in the `taiki-e/install-action` step; `ziglang` is
  installed with `--require-hashes` from `.github/requirements/ziglang.txt` (the
  hash is that of the linux x86_64 wheel; bump version and hash together). The
  BuildKit image of `docker/setup-buildx-action` is pinned by digest
  (`driver-opts`) in both workflows; bump all places together. Renovate does not
  touch these pins either.
- **Accepted residual risk.** If an existing chart version stays unreadable
  during *both* lookups (the first and the one right before `helm push`), it
  could be overwritten; this is accepted because the registry would have to
  answer access errors for a package our own credentials can write twice in a
  row, and closing it would also block the first release, whose "never pushed"
  answer looks exactly the same.
- **cosign.** Releases are signed with cosign v2.6.5 in the legacy format, which
  cosign v2 and v3 both verify.
