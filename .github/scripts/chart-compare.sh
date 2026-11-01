#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Chart check of the promote mode of release.yml (ADR 0005). The chart of a
# final release is packaged again from the tagged commit, but it must be the
# chart of the release candidate that is promoted, except for its version:
#
#   bash .github/scripts/chart-compare.sh <rc-chart-dir> <final-chart-dir>
#
# Both arguments are extracted charts (the directory that holds Chart.yaml,
# e.g. rc/secretspec-operator). Passes only if
#
#   - both have exactly the same files (templates, CRDs, values, schema, ...);
#   - every file but the top-level Chart.yaml is byte-for-byte equal;
#   - Chart.yaml differs only in its `version:` and `appVersion:` lines, each
#     of which occurs exactly once per file.
#
# Fail-closed: a missing directory or an unreadable file is a failure, not a
# pass. Needs: bash, diff, cmp, find, grep, sort.

# list_files <dir>: every non-directory under <dir>, relative, sorted.
list_files() {
  (cd "$1" && find . -mindepth 1 ! -type d -print | LC_ALL=C sort)
}

# count_key <key> <file>: the number of top-level lines that set <key>.
count_key() {
  local key="$1" file="$2" n status=0
  n="$(grep -c -E "^${key}:" "$file")" || status=$?
  # grep -c prints 0 and exits 1 when nothing matches; 2 is a real error.
  if ((status > 1)); then
    return 1
  fi
  printf '%s\n' "$n"
}

compare_charts() {
  local rc="$1" final="$2" rc_files final_files file dir ok=0 key n

  for dir in "$rc" "$final"; do
    if [[ ! -f "$dir/Chart.yaml" ]]; then
      echo "::error::$dir is not an extracted chart (no Chart.yaml)" >&2
      return 1
    fi
  done

  rc_files="$(list_files "$rc")" || return 1
  final_files="$(list_files "$final")" || return 1
  if [[ "$rc_files" != "$final_files" ]]; then
    echo "::error::the final chart and the release candidate chart do not have the same files:" >&2
    diff <(printf '%s\n' "$rc_files") <(printf '%s\n' "$final_files") >&2 || true # the output is the report
    return 1
  fi

  while IFS= read -r file; do
    [[ -n "$file" && "$file" != ./Chart.yaml ]] || continue
    if ! cmp -s "$rc/$file" "$final/$file"; then
      echo "::error::$file differs between the release candidate chart and the final chart:" >&2
      diff "$rc/$file" "$final/$file" >&2 || true # the output is the report
      ok=1
    fi
  done <<<"$rc_files"

  for key in version appVersion; do
    for dir in "$rc" "$final"; do
      n="$(count_key "$key" "$dir/Chart.yaml")" || return 1
      if [[ "$n" != 1 ]]; then
        echo "::error::$dir/Chart.yaml has $n '$key:' lines, expected exactly 1" >&2
        ok=1
      fi
    done
  done

  if ! diff \
    <(grep -v -E '^(version|appVersion):' "$rc/Chart.yaml") \
    <(grep -v -E '^(version|appVersion):' "$final/Chart.yaml") > /dev/null; then
    echo "::error::Chart.yaml differs in more than version and appVersion:" >&2
    diff \
      <(grep -v -E '^(version|appVersion):' "$rc/Chart.yaml") \
      <(grep -v -E '^(version|appVersion):' "$final/Chart.yaml") >&2 || true # the output is the report
    ok=1
  fi

  if ((ok != 0)); then
    return 1
  fi
  echo "the final chart equals the release candidate chart except for version and appVersion"
}

main() {
  if [[ $# -ne 2 ]]; then
    echo "usage: chart-compare.sh <rc-chart-dir> <final-chart-dir>" >&2
    return 2
  fi
  compare_charts "$1" "$2"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  set -euo pipefail
  main "$@"
fi
