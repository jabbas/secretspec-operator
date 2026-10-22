#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Registry lookups for release.yml (sourced, not executed). They decide whether
# a release tag / chart version already exists, so that no re-run can ever
# change what it points to. Everything ambiguous fails: only an explicit
# "does not exist" answer from the registry counts as absent.

# tag_digest <ref> <strict|lenient>
#
# Prints the manifest digest <ref> resolves to, or nothing when it does not
# exist. Returns 1 for any other outcome (timeout, 5xx, access error, ...).
#
#   strict   Absent only on MANIFEST_UNKNOWN / "manifest unknown" / ": not
#            found". Use it where the repository certainly exists and the
#            result is followed by a write (setting the tag).
#   lenient  Additionally absent on NAME_UNKNOWN and on access errors
#            (denied / unauthorized / "403 Forbidden"): a package that was
#            never pushed may be answered that way. Only use it where a wrong
#            "absent" cannot lead to a write to the tag.
#
# When the answer is "absent", the registry's own words (stderr of the
# lookup, never a secret) are printed to stderr, so the first run shows how
# the registry really answers.
tag_digest() {
  local ref="$1" mode="$2" out err
  err="$(mktemp)"
  if out="$(docker buildx imagetools inspect "$ref" --format '{{json .Manifest}}' 2> "$err")"; then
    jq -er '.digest' <<< "$out"
  elif grep -qE 'MANIFEST_UNKNOWN|manifest unknown|: not found' "$err"; then
    echo "lookup of $ref: absent (registry answer follows)" >&2
    cat "$err" >&2
  elif [[ "$mode" == lenient ]] && grep -qiE 'NAME_UNKNOWN|name unknown|denied|unauthorized|forbidden' "$err"; then
    echo "lookup of $ref: taken as absent (registry answer follows)" >&2
    cat "$err" >&2
  else
    cat "$err" >&2
    return 1
  fi
}

# chart_state <oci-ref> <version> <dest-dir> <log-file>
#
# Pulls the chart and sets CHART_STATE to found, absent (the registry says the
# version or the package does not exist) or denied (access error: denied,
# unauthorized or 403 Forbidden, which is also how a package that was never
# pushed may be answered). Returns 1 for
# any other outcome. On `found` the package is in <dest-dir> and the log has
# its "Digest:" line.
chart_state() {
  local ref="$1" version="$2" dest="$3" log="$4"
  if helm pull "$ref" --version "$version" --destination "$dest" > "$log" 2>&1; then
    CHART_STATE=found
  elif grep -qE 'MANIFEST_UNKNOWN|manifest unknown|NAME_UNKNOWN|name unknown|: not found' "$log"; then
    CHART_STATE=absent
  elif grep -qiE 'denied|unauthorized|forbidden' "$log"; then
    CHART_STATE=denied
  else
    cat "$log" >&2
    return 1
  fi
}

# chart_resolve <oci-ref> <version> <dest-dir> <log-file>
#
# Like chart_state, but an access error is retried (a transient token problem
# goes away; a package that does not exist yet keeps answering the same way).
# Only an access error that persists over all attempts is taken as absent, and
# is reported as absent-denied.
chart_resolve() {
  local attempt
  for attempt in 1 2 3; do
    chart_state "$@" || return 1
    if [[ "$CHART_STATE" != denied ]]; then
      return 0
    fi
    echo "attempt $attempt: access error while looking up the chart; retrying" >&2
    sleep 10
  done
  CHART_STATE=absent-denied
}
