# Development

## Prerequisites

- Rust toolchain (`rust-toolchain.toml` pins it; `rustup` installs it on first use)
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
cargo install --locked cargo-zigbuild --version 0.23.4
python3 -m venv ~/.local/share/zig-venv
~/.local/share/zig-venv/bin/pip install ziglang==0.17.0
export PATH="$HOME/.local/share/zig-venv/bin:$PATH"   # cargo-zigbuild finds `python3 -m ziglang`
```

Build the binaries (stripped, `--locked`) into `target/dist/<arch>/`:

```sh
cargo xtask dist --target x86_64-unknown-linux-musl --release    # target/dist/amd64/
cargo xtask dist --target aarch64-unknown-linux-musl --release   # target/dist/arm64/
file target/dist/*/secretspec-operator   # "statically linked" ELF per arch
```

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

A release is a pushed annotated tag on `main`: `vX.Y.Z` or `vX.Y.Z-rc.N`.
`.github/workflows/release.yml` does everything else; nothing is published by
hand. The image is `ghcr.io/jabbas/secretspec-operator:<tag>` (with the `v`), the
chart is `oci://ghcr.io/jabbas/charts/secretspec-operator` at the tag without the
`v`. No `latest` tag exists.

### Bump the versions

Three places carry the release version and must be equal; `cargo xtask
chart-test` (test `chart_version_matches_workspace`) and the workflow's `verify`
job fail otherwise:

1. `version` in `[workspace.package]` of `Cargo.toml` (then `cargo build` so
   `Cargo.lock` follows);
2. `version` in `charts/secretspec-operator/Chart.yaml`;
3. `appVersion` in `charts/secretspec-operator/Chart.yaml` (a quoted string).

Also replace "Unreleased" in the `CHANGELOG.md` heading of the new version with
the release date (for the final release). Do this in a pull request and merge it
to `main`.

**Release candidates do not change any of these files.** The committed versions
are the *final* version (`0.1.0`); the tag `v0.1.0-rc.1` is checked against it
(the part before `-rc.N` must equal the workspace version), and the workflow
injects the rc version only while packaging the chart
(`helm package --version 0.1.0-rc.1 --app-version 0.1.0-rc.1`), so the packaged
chart installs the image `…:v0.1.0-rc.1`.

### Tag

Tag a commit that is on `main` and has passed CI. Tags are annotated, and a
pushed tag is **never moved or deleted**: a signed artifact for it may already
exist.

```sh
git switch main && git pull --ff-only
git tag -a v0.1.0-rc.1 -m "v0.1.0-rc.1"      # release candidate
git push origin v0.1.0-rc.1
# after the candidate has been tried out:
git tag -a v0.1.0 -m "v0.1.0"                # final release
git push origin v0.1.0
```

#### When a release run fails

The workflow never changes what a pushed tag points to, also not when a run is
repeated. The build is not reproducible, so a second build would have a different
digest; therefore the image is first pushed **by digest without a tag**, scanned,
signed and attested, and only then tagged. A re-run that finds the tag already
present resumes with the digest the tag points to instead of rebuilding, and the
job fails if the tag points anywhere else. The chart is handled the same way: a
chart version that already exists in the registry is pulled and signed, never
packaged and pushed again.

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

Cut the next candidate (`-rc.2`) or patch version instead, and do not re-tag, when:

- the failure is caused by the code, the chart or the workflow itself (fix it on
  `main` first);
- the scan fails on a vulnerability (the image is untagged and unsigned);
- the workflow stops with "points to …, not to …" (somebody else moved or created
  the tag);
- a published image or chart turns out to be wrong.

### What the workflow does

| Job | Does |
|---|---|
| `verify` | Checks the tag format, that its version matches the workspace version (and the chart files), and that the commit is reachable from `main`; then runs the full gate: `fmt`, `clippy -D warnings`, tests including the ignored RSA tests in release mode, `crdgen --check`, `chart-test`, `cargo deny`, `cargo audit`. |
| `build` | Static musl binaries for amd64 and arm64 (`cargo xtask dist`, cargo-zigbuild), uploaded as an artifact. |
| `image` | Builds the multi-arch image and pushes it by digest (no tag), scans each platform image with grype (fails on a fixable high or critical finding), creates an SPDX SBOM per platform with syft, signs the index and the platform images with cosign, attests the SBOMs on the platform images, then creates the release tag (only if it is free or already points to that digest) and verifies it all with the documented commands. |
| `smoke` | Runs the pushed image with `--help` on amd64 and on arm64 (QEMU). |
| `chart` | Packages the chart at the release version (or, if that version is already published, pulls it), checks that it renders the image `ghcr.io/jabbas/secretspec-operator:<tag>`, pushes it if new, signs it and verifies the signature. |
| `release` | Creates the GitHub Release (pre-release for `-rc.N`) with generated notes, the chart package, the SBOMs and `SHA256SUMS`. |

Write permissions are granted per job: only `image` and `chart` can push packages
and obtain an OIDC token for signing, only `release` can write repository
contents.

### Verify a release

Follow [Security: Release signing and SBOM](security.md#release-signing-and-sbom)
(`cosign verify` for the image and the chart, `cosign verify-attestation` for the
SBOM of a platform image). Also check that the packages are public
(`docker pull ghcr.io/jabbas/secretspec-operator:<tag>` and `helm pull
oci://ghcr.io/jabbas/charts/secretspec-operator --version <version>` work
without logging in) and that a pre-release is marked as such.

### One-time setup and maintenance

- **Package visibility.** After the first release, the `secretspec-operator`
  container package and the `charts/secretspec-operator` package on ghcr.io must
  be public and linked to this repository (package settings). Without that,
  users cannot pull or verify them anonymously.
- **Protect tags.** A repository ruleset for `v*` tags that blocks deletion and
  updates enforces the "never move a tag" rule.
- **Scanner and cosign versions** are pinned in the `env:` block of
  `release.yml`. The syft and grype archives are verified against a SHA-256 that
  is pinned next to the version; when bumping one, take the checksum of the
  `linux_amd64.tar.gz` from the `*_checksums.txt` of that upstream release and
  change both. cosign is installed by the SHA-pinned `sigstore/cosign-installer`,
  which verifies the download itself. Renovate does not touch these pins.
