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
#   - both have the same manifest: the same paths, and for each path the same
#     type (file, directory, symlink), the same mode (not for symlinks) and
#     the same link target (symlinks only; compared as text, never followed);
#   - there is no special file (device, FIFO, socket) in either;
#   - every regular file but the top-level Chart.yaml is byte-for-byte equal;
#   - Chart.yaml (a regular file) differs only in its `version:` and
#     `appVersion:` lines, each of which occurs exactly once per file.
#
# The manifest comes first because cmp follows symlinks and ignores modes: a
# file swapped for a symlink to equal bytes, or a changed permission, must not
# pass. cmp is used for regular files only.
#
# Fail-closed: a missing directory or an unreadable file is a failure, not a
# pass. Needs: bash, diff, cmp, find, grep, sort, readlink and stat (GNU or
# BSD, so it runs on macOS and on the Linux runner).

# path_mode <path>: the permission bits in octal (lstat; not for symlinks).
path_mode() {
  # GNU stat first; BSD stat (macOS) does not know -c.
  stat -c '%a' -- "$1" 2> /dev/null || stat -f '%Lp' "$1"
}

# manifest <dir>
#
# Prints one line per entry, sorted: "<path>\t<type>\t<mode>\t<link target>"
# (mode and target are "-" where they do not apply). Returns 1 after
# reporting a special file or an unreadable entry.
manifest() {
  local dir="$1" paths path full type mode target status=0
  paths="$(cd "$dir" && find . -mindepth 1 | LC_ALL=C sort)" || return 1
  while IFS= read -r path; do
    [[ -n "$path" ]] || continue
    full="$dir/$path"
    mode="-" target="-"
    if [[ -L "$full" ]]; then
      type=symlink
      target="$(readlink "$full")" || return 1
    elif [[ -d "$full" ]]; then
      type=dir
      mode="$(path_mode "$full")" || return 1
    elif [[ -f "$full" ]]; then
      type="file"
      mode="$(path_mode "$full")" || return 1
    else
      echo "::error::$full is a special file (device, FIFO or socket); a chart has none" >&2
      status=1
      continue
    fi
    printf '%s\t%s\t%s\t%s\n' "$path" "$type" "$mode" "$target"
  done <<<"$paths"
  return "$status"
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
  local rc="$1" final="$2" rc_manifest final_manifest path type ok=0 dir key n

  for dir in "$rc" "$final"; do
    if [[ ! -f "$dir/Chart.yaml" || -L "$dir/Chart.yaml" ]]; then
      echo "::error::$dir is not an extracted chart (no regular file Chart.yaml)" >&2
      return 1
    fi
  done

  rc_manifest="$(manifest "$rc")" || return 1
  final_manifest="$(manifest "$final")" || return 1
  if [[ "$rc_manifest" != "$final_manifest" ]]; then
    echo "::error::the final chart and the release candidate chart do not have the same entries (path, type, mode, link target); release candidate on the left:" >&2
    diff <(printf '%s\n' "$rc_manifest") <(printf '%s\n' "$final_manifest") >&2 || true # the output is the report
    ok=1
  fi

  # Regular files present on both sides (the manifest check above reports the
  # rest). Everything but Chart.yaml must be identical.
  while IFS=$'\t' read -r path type _; do
    [[ "$type" == file && "$path" != ./Chart.yaml ]] || continue
    if [[ -f "$final/$path" && ! -L "$final/$path" ]] && ! cmp -s "$rc/$path" "$final/$path"; then
      echo "::error::$path differs between the release candidate chart and the final chart:" >&2
      diff "$rc/$path" "$final/$path" >&2 || true # the output is the report
      ok=1
    fi
  done <<<"$rc_manifest"

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
