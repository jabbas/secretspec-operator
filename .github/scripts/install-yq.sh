#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Installs the pinned mikefarah/yq on a GitHub Actions linux/amd64 runner, for
# the `helm-index` job of release.yml and the `release-scripts` job of ci.yml
# (the scripts under test parse and rewrite the shared Helm index with it, so
# the runner image's yq is not trusted to be this one). The archive is verified
# against a SHA-256 taken from the `checksums` file of that upstream release
# (column "SHA-256" in `checksums_hashes_order`). Bump both together. Renovate
# proposes the version (see the comment below, read by renovate.json) but
# cannot compute the checksum: a version-only bump fails the check below.
set -euo pipefail

# renovate: datasource=github-releases depName=mikefarah/yq
YQ_VERSION=v4.54.3
YQ_SHA256=7dd21fe96aac5ecccef949f4e4abecf9f9344dc3a853f5a2b4633c7a7afa7dda

archive="yq_linux_amd64.tar.gz"
dir="${RUNNER_TEMP:?RUNNER_TEMP is not set (this script is for Actions runners)}/tools/bin"
mkdir -p "$dir"
curl -fsSL --retry 3 -o "$RUNNER_TEMP/$archive" \
  "https://github.com/mikefarah/yq/releases/download/${YQ_VERSION}/${archive}"
if ! echo "$YQ_SHA256  $RUNNER_TEMP/$archive" | sha256sum --check --strict -; then
  echo "::error::$archive does not match the pinned checksum; update YQ_SHA256 (and YQ_VERSION) together in .github/scripts/install-yq.sh, from the checksums file of the upstream release (docs/development.md, \"Pinned tools and Renovate\")"
  exit 1
fi
tar -xzf "$RUNNER_TEMP/$archive" -C "$dir" ./yq_linux_amd64
mv "$dir/yq_linux_amd64" "$dir/yq"
echo "$dir" >> "${GITHUB_PATH:?GITHUB_PATH is not set}"
"$dir/yq" --version
