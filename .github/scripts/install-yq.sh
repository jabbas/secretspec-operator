#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Installs the pinned mikefarah/yq on a GitHub Actions linux/amd64 runner, for
# the `helm-index` job of release.yml and the `release-scripts` job of ci.yml
# (the scripts under test parse and rewrite the shared Helm index with it, so
# the runner image's yq is not trusted to be this one). The archive is verified
# against a SHA-256 taken from the `checksums` file of that upstream release
# (column "SHA-256" in `checksums_hashes_order`). Bump both together.
set -euo pipefail

YQ_VERSION=v4.54.1
YQ_SHA256=e68a456f90c577af3fe4960184b3a3cf5c461e0348407c10f107da3a5fec8972

archive="yq_linux_amd64.tar.gz"
dir="${RUNNER_TEMP:?RUNNER_TEMP is not set (this script is for Actions runners)}/tools/bin"
mkdir -p "$dir"
curl -fsSL --retry 3 -o "$RUNNER_TEMP/$archive" \
  "https://github.com/mikefarah/yq/releases/download/${YQ_VERSION}/${archive}"
echo "$YQ_SHA256  $RUNNER_TEMP/$archive" | sha256sum --check --strict -
tar -xzf "$RUNNER_TEMP/$archive" -C "$dir" ./yq_linux_amd64
mv "$dir/yq_linux_amd64" "$dir/yq"
echo "$dir" >> "${GITHUB_PATH:?GITHUB_PATH is not set}"
"$dir/yq" --version
