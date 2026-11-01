#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Release mode for the `verify` job of release.yml (ADR 0005):
#
#   bash .github/scripts/release-mode.sh <tag> <commit-sha> >> "$GITHUB_OUTPUT"
#
# Prints GITHUB_OUTPUT lines (key=value) on stdout, everything else on stderr:
#
#   mode=build            release candidate tag X.Y.Z-rc.N: the image is built
#   mode=promote          final tag X.Y.Z: the image of a release candidate
#   rc_tag=X.Y.Z-rc.N       that is tagged on the same commit is promoted
#   mode=build            final tag whose annotation has a `rebuild: <reason>`
#   rebuild_reason=...      line: an explicit emergency rebuild
#
# A final tag with neither a release candidate on its commit nor a `rebuild:`
# line fails. The `rebuild:` line wins over a release candidate: it is an
# explicit choice, never an automatic fallback.
#
# Everything is read through the GitHub API, not from the checkout: the
# checkout of a tag run can replace an annotated tag by a plain ref to its
# commit and lose the message. A tag is peeled to its commit by the API too.
#
# Fail-closed: an API error, an answer that does not have the expected shape,
# or a tag that is not a (tag of a) commit is never read as "no release
# candidate" or as "no rebuild marker"; it fails.
#
# Environment:
#   GITHUB_REPOSITORY    owner/name (set by Actions)
#   GH_TOKEN             token for gh (the job's GITHUB_TOKEN, `contents: read`)
#
# Needs: gh (with --paginate --slurp), jq. Written for bash 3.2 too (no arrays
# or associative arrays).

# resolve_object <type> <sha>
#
# Sets OBJ_COMMIT (the commit) and OBJ_MESSAGE (the tag annotation, empty for a
# lightweight tag) for a ref target as the refs API reports it: a commit
# (lightweight tag) or a tag object (annotated tag) that points at a commit.
resolve_object() {
  local type="$1" sha="$2" body
  OBJ_COMMIT="" OBJ_MESSAGE=""
  if [[ ! "$sha" =~ ^[0-9a-f]{40}$ ]]; then
    echo "::error::'$sha' is not a full object SHA" >&2
    return 1
  fi
  case "$type" in
    commit)
      OBJ_COMMIT="$sha"
      ;;
    tag)
      if ! body="$(gh api "repos/$GITHUB_REPOSITORY/git/tags/$sha")"; then
        echo "::error::could not read the tag object $sha (gh api failed); refusing to choose a release mode" >&2
        return 1
      fi
      if ! jq -e '(.message | type) == "string" and .object.type == "commit"
                  and (.object.sha | type) == "string"' <<<"$body" > /dev/null 2>&1; then
        echo "::error::the tag object $sha is not an annotated tag of a commit; refusing to choose a release mode" >&2
        return 1
      fi
      OBJ_COMMIT="$(jq -r '.object.sha' <<<"$body")"
      if [[ ! "$OBJ_COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
        echo "::error::the tag object $sha points at '$OBJ_COMMIT', not a commit SHA" >&2
        return 1
      fi
      OBJ_MESSAGE="$(jq -r '.message' <<<"$body")"
      ;;
    *)
      echo "::error::the tag points at a '$type' object, not at a commit; refusing to choose a release mode" >&2
      return 1
      ;;
  esac
}

# final_tag_message <tag> <commit-sha>
#
# Sets OBJ_MESSAGE to the annotation of the final tag. The tag must peel to
# <commit-sha> (the commit the workflow runs for).
final_tag_message() {
  local tag="$1" commit="$2" body type sha
  if ! body="$(gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/$tag")"; then
    echo "::error::could not read the tag $tag (gh api failed); refusing to choose a release mode" >&2
    return 1
  fi
  if ! jq -e --arg ref "refs/tags/$tag" '.ref == $ref and (.object.type | type) == "string"
                                          and (.object.sha | type) == "string"' <<<"$body" > /dev/null 2>&1; then
    echo "::error::the answer for the tag $tag is not a tag reference; refusing to choose a release mode" >&2
    return 1
  fi
  type="$(jq -r '.object.type' <<<"$body")"
  sha="$(jq -r '.object.sha' <<<"$body")"
  resolve_object "$type" "$sha" || return 1
  if [[ "$OBJ_COMMIT" != "$commit" ]]; then
    echo "::error::the tag $tag points at $OBJ_COMMIT, but this run is for $commit" >&2
    return 1
  fi
}

# rebuild_reason <message>
#
# Prints the reason of the first `rebuild: <reason>` line of a tag message and
# returns 0; prints nothing and returns 0 when there is no such line. An empty
# reason, a reason of more than 200 characters or one with a character outside
# [A-Za-z0-9 ._:/()#,+-] fails.
rebuild_reason() {
  local message="$1" line status=0 reason
  line="$(grep -m1 -E '^rebuild: .+' <<<"$message")" || status=$?
  if ((status == 1)); then
    return 0 # no marker
  elif ((status != 0)); then
    echo "::error::could not search the tag message for a rebuild marker" >&2
    return 1
  fi
  reason="${line#rebuild: }"
  reason="${reason%$'\r'}"
  # Trim surrounding blanks.
  reason="${reason#"${reason%%[![:space:]]*}"}"
  reason="${reason%"${reason##*[![:space:]]}"}"
  if [[ -z "$reason" ]]; then
    echo "::error::the 'rebuild:' line of the tag message has no reason" >&2
    return 1
  fi
  # A conservative allowlist, not an escaping scheme: the reason ends up in a
  # workflow command, a job output and the Markdown of the release notes, so
  # anything that is special in one of them is refused. C locale: the ranges
  # are ASCII.
  if ((${#reason} > 200)) || ! printf '%s' "$reason" | LC_ALL=C grep -Eq '^[A-Za-z0-9 ._:/()#,+-]+$'; then
    echo "::error::the 'rebuild:' reason may only contain the characters A-Z a-z 0-9 space . _ : / ( ) # , + - and at most 200 of them" >&2
    return 1
  fi
  printf '%s\n' "$reason"
}

# find_release_candidate <base> <commit-sha>
#
# Prints the rc tag (base-rc.N, highest N) whose commit is <commit-sha>, or
# nothing when there is none.
find_release_candidate() {
  local base="$1" commit="$2" prefix="$1-rc." body list ref type sha name n best="" numbers=""
  if ! body="$(gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/git/matching-refs/tags/${prefix}?per_page=100")"; then
    echo "::error::could not list the $prefix* tags (gh api failed); refusing to choose a release mode" >&2
    return 1
  fi
  # --slurp: one array per page.
  if ! jq -e 'type == "array" and all(.[]; type == "array"
              and all(.[]; (.ref | type) == "string" and (.object.type | type) == "string"
                           and (.object.sha | type) == "string"))' <<<"$body" > /dev/null 2>&1; then
    echo "::error::the list of $prefix* tags is not a list of references; refusing to choose a release mode" >&2
    return 1
  fi
  list="$(jq -r '.[][] | [.ref, .object.type, .object.sha] | @tsv' <<<"$body")" || return 1
  while IFS=$'\t' read -r ref type sha; do
    [[ -n "$ref" ]] || continue
    name="${ref#refs/tags/}"
    # The API matches a prefix; only X.Y.Z-rc.N (no leading zeros) counts.
    [[ "$name" == "$prefix"* ]] || continue
    n="${name#"$prefix"}"
    [[ "$n" =~ ^(0|[1-9][0-9]*)$ ]] || continue
    resolve_object "$type" "$sha" || return 1
    if [[ "$OBJ_COMMIT" == "$commit" ]]; then
      numbers="$numbers$n"$'\n'
    fi
  done <<<"$list"
  if [[ -n "$numbers" ]]; then
    best="$(sort -n <<<"$numbers" | tail -n 1)"
    printf '%s%s\n' "$prefix" "$best"
  fi
}

main() {
  local tag commit base reason escaped rc_tag
  if [[ $# -ne 2 ]]; then
    echo "usage: release-mode.sh <tag> <commit-sha>" >&2
    return 2
  fi
  tag="$1" commit="$2"
  if [[ -z "${GITHUB_REPOSITORY:-}" ]]; then
    echo "::error::GITHUB_REPOSITORY is not set" >&2
    return 1
  fi
  if [[ ! "$commit" =~ ^[0-9a-f]{40}$ ]]; then
    echo "::error::'$commit' is not a full commit SHA" >&2
    return 1
  fi
  # Groups: 1 = base, 5 = the rc suffix (same expression as the workflow).
  if [[ ! "$tag" =~ ^((0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*))(-rc\.(0|[1-9][0-9]*))?$ ]]; then
    echo "::error::tag '$tag' is neither X.Y.Z nor X.Y.Z-rc.N" >&2
    return 1
  fi
  base="${BASH_REMATCH[1]}"

  # A release candidate is always built.
  if [[ -n "${BASH_REMATCH[5]}" ]]; then
    echo "$tag is a release candidate: the image is built" >&2
    echo "mode=build"
    return 0
  fi

  # A final tag. The annotation is read first: the marker is an explicit
  # choice and wins over an existing release candidate.
  final_tag_message "$tag" "$commit" || return 1
  reason="$(rebuild_reason "$OBJ_MESSAGE")" || return 1
  if [[ -n "$reason" ]]; then
    # Workflow-command data escapes (the allowlist leaves nothing to escape;
    # this keeps the message safe if the allowlist is ever widened).
    escaped="${reason//\%/%25}"
    escaped="${escaped//$'\r'/%0D}"
    escaped="${escaped//$'\n'/%0A}"
    echo "::warning::$tag is rebuilt, not promoted from a release candidate: $escaped" >&2
    echo "mode=build"
    echo "rebuild_reason=$reason"
    return 0
  fi

  rc_tag="$(find_release_candidate "$base" "$commit")" || return 1
  if [[ -z "$rc_tag" ]]; then
    echo "::error::no release candidate $base-rc.N on $commit; cut a release candidate first, or add a 'rebuild: <reason>' line to the tag message for an emergency release" >&2
    return 1
  fi
  echo "$tag is promoted from $rc_tag (same commit $commit)" >&2
  echo "mode=promote"
  echo "rc_tag=$rc_tag"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  set -euo pipefail
  main "$@"
fi
