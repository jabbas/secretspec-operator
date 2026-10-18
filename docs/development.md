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

`cargo test --workspace` runs the first three; the server suite is `#[ignore]`d.
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
