#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Index update for the `helm-index` job of release.yml (sourced, not
# executed). The classic HTTP Helm repository (jabbas/helm-charts, served at
# https://jabbas.github.io/helm-charts) holds one root index.yaml and no
# packages; every entry points at the release asset of this repository.
# Everything ambiguous fails: a version that is already listed is never
# changed, and an index that does not look as expected is never pushed.
#
# Needs: helm, yq (mikefarah v4), git, curl.

CHART_NAME=secretspec-operator

# index_entry_count <index.yaml> <version>
#
# Prints how many entries of the chart carry <version>. An index without the
# chart (or an empty one) has none.
index_entry_count() {
  local index="$1" version="$2"
  VERSION="$version" CHART="$CHART_NAME" yq \
    '[(.entries[strenv(CHART)] // [])[] | select(.version == strenv(VERSION))] | length' "$index"
}

# index_entry_digest <index.yaml> <version>
#
# Prints the digest (bare hex, as helm writes it) of the single entry for
# <version>, or nothing when there is none. Returns 1 when the version is
# listed more than once or its entry has no usable digest.
index_entry_digest() {
  local index="$1" version="$2" count digest
  count="$(index_entry_count "$index" "$version")"
  if [[ "$count" == 0 ]]; then
    return 0
  fi
  if [[ "$count" != 1 ]]; then
    echo "::error::$index lists $CHART_NAME $version $count times" >&2
    return 1
  fi
  digest="$(VERSION="$version" CHART="$CHART_NAME" yq \
    '.entries[strenv(CHART)][] | select(.version == strenv(VERSION)) | .digest // ""' "$index")"
  if [[ ! "$digest" =~ ^[0-9a-f]{64}$ ]]; then
    echo "::error::the $CHART_NAME $version entry in $index has no valid digest ('$digest'); fix the index by hand" >&2
    return 1
  fi
  printf '%s\n' "$digest"
}

# check_new_index <old> <new> <version> <digest> <url>
#
# The merged index must be the old one plus exactly one entry: all old entries
# unchanged, the new one complete.
check_new_index() {
  local old="$1" new="$2" version="$3" digest="$4" url="$5" field
  local old_count new_count
  old_count="$(CHART="$CHART_NAME" yq '(.entries[strenv(CHART)] // []) | length' "$old")"
  new_count="$(CHART="$CHART_NAME" yq '(.entries[strenv(CHART)] // []) | length' "$new")"
  if [[ "$new_count" != "$((old_count + 1))" ]]; then
    echo "::error::the merged index has $new_count entries, expected $((old_count + 1))" >&2
    return 1
  fi
  if [[ "$(index_entry_count "$new" "$version")" != 1 ]]; then
    echo "::error::the merged index does not list $version exactly once" >&2
    return 1
  fi
  # Every old entry must survive byte for byte (as YAML data).
  if ! diff <(CHART="$CHART_NAME" yq -o=json -I=0 \
        '[(.entries[strenv(CHART)] // [])[]] | sort_by(.version)' "$old") \
      <(VERSION="$version" CHART="$CHART_NAME" yq -o=json -I=0 \
        '[.entries[strenv(CHART)][] | select(.version != strenv(VERSION))] | sort_by(.version)' "$new") \
      > /dev/null; then
    echo "::error::the merged index changed an existing entry" >&2
    return 1
  fi
  for field in name version apiVersion appVersion created digest; do
    if [[ -z "$(VERSION="$version" CHART="$CHART_NAME" F="$field" yq \
        '.entries[strenv(CHART)][] | select(.version == strenv(VERSION)) | .[strenv(F)] // ""' "$new")" ]]; then
      echo "::error::the new $version entry has no '$field'" >&2
      return 1
    fi
  done
  if [[ "$(index_entry_digest "$new" "$version")" != "$digest" ]]; then
    echo "::error::the new $version entry carries a different digest than $digest" >&2
    return 1
  fi
  if [[ "$(VERSION="$version" CHART="$CHART_NAME" yq \
      '.entries[strenv(CHART)][] | select(.version == strenv(VERSION)) | .urls | length' "$new")" != 1 ||
      "$(VERSION="$version" CHART="$CHART_NAME" yq \
      '.entries[strenv(CHART)][] | select(.version == strenv(VERSION)) | .urls[0]' "$new")" != "$url" ]]; then
    echo "::error::the new $version entry must have exactly the url $url" >&2
    return 1
  fi
}

# update_index <repo-dir> <chart.tgz> <version> <digest-hex> <base-url>
#
# Adds <version> to <repo-dir>/index.yaml. Returns
#   0  the index was changed (commit it),
#   10 the version is already listed with this digest (nothing to do),
#   1  anything else (a different digest, a malformed index, a bad merge).
# The chart is copied to a scratch directory, so no package ever lands in
# <repo-dir>.
update_index() {
  local repo="$1" chart="$2" version="$3" digest="$4" base="$5"
  local index="$repo/index.yaml" existing stage url
  [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || { echo "::error::'$digest' is not a sha256 hex digest" >&2; return 1; }
  [[ -f "$index" ]] || { echo "::error::$index does not exist" >&2; return 1; }
  [[ "$(yq '.apiVersion' "$index")" == v1 ]] || { echo "::error::$index is not a Helm repository index" >&2; return 1; }

  existing="$(index_entry_digest "$index" "$version")" || return 1
  if [[ -n "$existing" ]]; then
    if [[ "$existing" == "$digest" ]]; then
      echo "$CHART_NAME $version is already in the index with digest $digest; nothing to do"
      return 10
    fi
    echo "::error::$CHART_NAME $version is already in the index with digest $existing, not $digest; a published version is never changed. Cut the next release candidate." >&2
    return 1
  fi

  stage="$(mktemp -d)"
  cp -- "$chart" "$stage/"
  helm repo index "$stage" --url "$base" --merge "$index" || { rm -rf "$stage"; return 1; }
  url="$base/$(basename -- "$chart")"
  if ! check_new_index "$index" "$stage/index.yaml" "$version" "$digest" "$url"; then
    rm -rf "$stage"
    return 1
  fi
  cp -- "$stage/index.yaml" "$index"
  rm -rf "$stage"
}

# publish_index <repo-dir> <chart.tgz> <version> <digest-hex> <base-url> [attempts]
#
# update_index + commit + push, for a git working copy on its default branch.
# Each attempt starts from the remote branch and rebuilds the index, so a
# concurrent update by another chart (a lost push race) is picked up and a
# retry never merges by hand. Bounded: <attempts> (default 5), then failure.
publish_index() {
  local repo="$1" chart="$2" version="$3" digest="$4" base="$5" attempts="${6:-5}"
  local branch attempt rc
  branch="$(git -C "$repo" symbolic-ref --short HEAD)"
  for ((attempt = 1; attempt <= attempts; attempt++)); do
    git -C "$repo" fetch --quiet origin "$branch"
    git -C "$repo" reset --quiet --hard "origin/$branch"
    rc=0
    update_index "$repo" "$chart" "$version" "$digest" "$base" || rc=$?
    case "$rc" in
      10) return 0 ;;
      0) ;;
      *) return 1 ;;
    esac
    git -C "$repo" add -- index.yaml
    git -C "$repo" \
      -c user.name='github-actions[bot]' \
      -c user.email='41898282+github-actions[bot]@users.noreply.github.com' \
      commit --quiet -m "$CHART_NAME $version"
    if git -C "$repo" push --quiet origin "HEAD:refs/heads/$branch"; then
      echo "pushed $CHART_NAME $version (attempt $attempt)"
      return 0
    fi
    echo "::warning::push rejected (attempt $attempt of $attempts); fetching and retrying" >&2
    sleep "${PUBLISH_RETRY_SLEEP:-$((attempt * 5))}"
  done
  echo "::error::could not push the index after $attempts attempts" >&2
  return 1
}

# wait_for_pages <index-url> <version> <digest-hex> [tries] [sleep-seconds]
#
# Polls the published index until it lists <version> with <digest>. Returns 1
# when it does not within the bounds (the push is done; Pages is just slow
# or its build failed).
wait_for_pages() {
  local url="$1" version="$2" digest="$3" tries="${4:-30}" pause="${5:-20}" i body listed
  body="$(mktemp)"
  for ((i = 1; i <= tries; i++)); do
    # A query string makes the CDN fetch a fresh copy; Pages ignores it.
    if curl -fsSL --retry 2 -o "$body" "$url?cachebust=$(date +%s)-$i" 2> /dev/null; then
      listed="$(index_entry_digest "$body" "$version" 2> /dev/null || true)"
      if [[ "$listed" == "$digest" ]]; then
        echo "$url lists $CHART_NAME $version with digest $digest"
        rm -f "$body"
        return 0
      fi
    fi
    echo "waiting for Pages to serve $version ($i/$tries)"
    sleep "$pause"
  done
  rm -f "$body"
  return 1
}
