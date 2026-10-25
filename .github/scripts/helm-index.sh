#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Index update for the `helm-index` job of release.yml (sourced, not
# executed). The classic HTTP Helm repository (jabbas/helm-charts, served at
# https://jabbas.github.io/helm-charts) holds one root index.yaml and no
# packages; every entry points at the release asset of this repository.
#
# The index is shared by all charts of the owner. This script therefore never
# lets helm rewrite it: helm (`repo index --merge`) drops entries it considers
# invalid, adds fields to foreign entries and removes unknown top-level and
# entry fields. Instead, helm builds an index for the one new package in a
# scratch directory, and that single entry is inserted into the existing
# document with yq. Afterwards the whole new document is compared with the old
# one as data; anything that differs besides the new entry and `generated`
# fails the update.
#
# Everything ambiguous fails: a version that is already listed is never
# changed, and an index that does not look as expected is never pushed.
#
# Needs: helm (or $HELM), yq (mikefarah v4), jq (tests only), git, curl.

CHART_NAME=secretspec-operator

# The helm binary; the tests run against more than one.
helm_cmd() { "${HELM:-helm}" "$@"; }

# Normal form of an index document for comparison: no `generated`, charts
# without entries dropped, entries sorted by chart and version, keys sorted.
# shellcheck disable=SC2016  # yq expression, not shell
INDEX_NORMALIZE='del(.generated)
  | .entries = ((.entries // {})
      | with_entries(.value = ((.value // []) | sort_by(.version)))
      | with_entries(select(.value | length > 0)))
  | sort_keys(..)'

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

# check_entry <index.yaml> <version> <digest> <url>
#
# The entry for <version> must exist exactly once, be complete, carry the
# digest and have exactly the one url.
check_entry() {
  local index="$1" version="$2" digest="$3" url="$4" field
  if [[ "$(index_entry_count "$index" "$version")" != 1 ]]; then
    echo "::error::$index does not list $CHART_NAME $version exactly once" >&2
    return 1
  fi
  for field in name version apiVersion appVersion created digest; do
    if [[ -z "$(VERSION="$version" CHART="$CHART_NAME" F="$field" yq \
        '.entries[strenv(CHART)][] | select(.version == strenv(VERSION)) | .[strenv(F)] // ""' "$index")" ]]; then
      echo "::error::the new $version entry has no '$field'" >&2
      return 1
    fi
  done
  if [[ "$(VERSION="$version" CHART="$CHART_NAME" yq \
      '.entries[strenv(CHART)][] | select(.version == strenv(VERSION)) | .name' "$index")" != "$CHART_NAME" ]]; then
    echo "::error::the new $version entry is not named $CHART_NAME" >&2
    return 1
  fi
  if [[ "$(index_entry_digest "$index" "$version")" != "$digest" ]]; then
    echo "::error::the new $version entry carries a different digest than $digest" >&2
    return 1
  fi
  if [[ "$(VERSION="$version" CHART="$CHART_NAME" yq \
      '.entries[strenv(CHART)][] | select(.version == strenv(VERSION)) | .urls | length' "$index")" != 1 ||
      "$(VERSION="$version" CHART="$CHART_NAME" yq \
      '.entries[strenv(CHART)][] | select(.version == strenv(VERSION)) | .urls[0]' "$index")" != "$url" ]]; then
    echo "::error::the new $version entry must have exactly the url $url" >&2
    return 1
  fi
}

# check_new_index <old> <new> <version> <digest> <url>
#
# The new document must be the old one plus exactly one complete entry for
# <version>: compared as data, whole document, nothing may differ except that
# entry and the top-level `generated`.
check_new_index() {
  local old="$1" new="$2" version="$3" digest="$4" url="$5"
  local old_norm new_norm
  check_entry "$new" "$version" "$digest" "$url" || return 1
  if ! old_norm="$(yq -o=json -I=0 "$INDEX_NORMALIZE" "$old")"; then
    echo "::error::$old cannot be compared (an entry list is not a list?)" >&2
    return 1
  fi
  if ! new_norm="$(VERSION="$version" CHART="$CHART_NAME" yq -o=json -I=0 \
      "(.entries[strenv(CHART)] |= map(select(.version != strenv(VERSION)))) | $INDEX_NORMALIZE" "$new")"; then
    echo "::error::$new cannot be compared" >&2
    return 1
  fi
  if [[ "$old_norm" != "$new_norm" ]]; then
    echo "::error::the new index differs from the old one in more than the $CHART_NAME $version entry; the shared index is never rewritten" >&2
    return 1
  fi
}

# update_index <repo-dir> <chart.tgz> <version> <digest-hex> <base-url>
#
# Adds <version> to <repo-dir>/index.yaml. Returns
#   0  the index was changed (commit it),
#   10 the version is already listed with this digest (nothing to do),
#   1  anything else (a different digest, a malformed index, a bad entry).
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

  # helm only sees the new package: an index of exactly one entry.
  url="$base/$(basename -- "$chart")"
  stage="$(mktemp -d)"
  cp -- "$chart" "$stage/"
  if ! helm_cmd repo index "$stage" --url "$base"; then
    rm -rf "$stage"
    return 1
  fi
  mkdir "$stage/out"
  cp -- "$index" "$stage/out/index.yaml"
  # The one entry goes first (helm lists newest first; clients sort anyway);
  # everything else in the document stays as it is.
  if ! NEW="$stage/index.yaml" CHART="$CHART_NAME" VERSION="$version" yq -i '
      .entries[strenv(CHART)] = [load(strenv(NEW)).entries[strenv(CHART)][0]] + (.entries[strenv(CHART)] // [])
      | .generated = load(strenv(NEW)).generated' "$stage/out/index.yaml" ||
    ! check_new_index "$index" "$stage/out/index.yaml" "$version" "$digest" "$url"; then
    rm -rf "$stage"
    return 1
  fi
  cp -- "$stage/out/index.yaml" "$index"
  rm -rf "$stage"
}

# git_authed <git args...>
#
# Runs git with $HELM_CHARTS_TOKEN (when set) as an HTTP Authorization header
# for github.com, for this one command only. The token goes through the
# environment of a subshell: it is not in any argument list, not in
# .git/config and never printed.
git_authed() {
  if [[ -z "${HELM_CHARTS_TOKEN:-}" ]]; then
    git "$@"
    return
  fi
  (
    GIT_CONFIG_COUNT=1
    GIT_CONFIG_KEY_0=http.https://github.com/.extraheader
    GIT_CONFIG_VALUE_0="AUTHORIZATION: basic $(printf 'x-access-token:%s' "$HELM_CHARTS_TOKEN" | base64 | tr -d '\n')"
    export GIT_CONFIG_COUNT GIT_CONFIG_KEY_0 GIT_CONFIG_VALUE_0
    git "$@"
  )
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
    if git_authed -C "$repo" push --quiet origin "HEAD:refs/heads/$branch"; then
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
# Polls the published index until it lists <version> with <digest>. Returns
#   0  it does,
#   1  it does not list <version> within the bounds (the push is done; Pages
#      is slow or its build failed),
#   2  it lists <version> with a DIFFERENT digest (wrong content is served).
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
      if [[ -n "$listed" ]]; then
        echo "::error::$url lists $CHART_NAME $version with digest $listed, not $digest" >&2
        rm -f "$body"
        return 2
      fi
    fi
    echo "waiting for Pages to serve $version ($i/$tries)"
    sleep "$pause"
  done
  rm -f "$body"
  return 1
}
