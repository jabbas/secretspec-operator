#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# CI gate for the `verify` job of release.yml. The release does not re-run the
# test suite; it requires that the tagged commit already passed `ci.yml` as a
# push to `main`:
#
#   bash .github/scripts/ci-status.sh <commit-sha>
#
# Looks for runs of workflow ci.yml with event=push, head_branch=main and
# head_sha=<commit-sha> (runs of pull requests are ignored: they test a merge
# ref, not this commit).
#
#   - at least one run concluded `success`        -> pass
#   - no success, a run is still queued/running   -> poll, then decide on it
#   - only failed/cancelled runs, or no run at all -> fail
#
# Fail-closed: an API error, an answer that does not look like a list of
# workflow runs, or a list that is cut off is never read as "no run" or as
# "success"; it fails.
#
# Environment:
#   GITHUB_REPOSITORY    owner/name (set by Actions)
#   GH_TOKEN             token for gh (the job's GITHUB_TOKEN, `actions: read`)
#   CI_STATUS_INTERVAL   seconds between polls              (default 30)
#   CI_STATUS_TIMEOUT    seconds to wait for a running CI   (default 1800)
#   CI_STATUS_WORKFLOW   workflow file to look at           (default ci.yml)
#
# Needs: gh, jq.

CI_WORKFLOW="${CI_STATUS_WORKFLOW:-ci.yml}"

# ci_runs <sha>
#
# Prints the matching runs as "<status> <conclusion>" lines (conclusion is
# "-" while a run is not finished). Returns non-zero on any API or parse
# problem.
ci_runs() {
  local sha="$1" body count
  # event/branch/head_sha narrow the answer; the jq filter below checks them
  # again, so a server that ignores a parameter cannot widen the result.
  if ! body="$(gh api "repos/$GITHUB_REPOSITORY/actions/workflows/$CI_WORKFLOW/runs?event=push&branch=main&head_sha=$sha&per_page=100")"; then
    echo "::error::could not list the $CI_WORKFLOW runs of $GITHUB_REPOSITORY for $sha (gh api failed); refusing to release without a CI verdict" >&2
    return 1
  fi
  if ! jq -e '(.workflow_runs | type) == "array" and (.total_count | type) == "number"' <<<"$body" > /dev/null 2>&1; then
    echo "::error::the $CI_WORKFLOW run list for $sha is not a workflow-run list; refusing to release without a CI verdict" >&2
    return 1
  fi
  # A list that is cut off could hide a success (or the one failure).
  if ! jq -e '.total_count <= (.workflow_runs | length)' <<<"$body" > /dev/null; then
    count="$(jq -r '.total_count' <<<"$body")"
    echo "::error::$count $CI_WORKFLOW runs match $sha but not all were returned; refusing to release without a CI verdict" >&2
    return 1
  fi
  jq -r --arg sha "$sha" '
    .workflow_runs[]
    | select(.event == "push" and .head_branch == "main" and .head_sha == $sha)
    | "\(.status // "unknown") \(.conclusion // "-")"' <<<"$body"
}

# ci_wait <sha>
#
# Returns 0 when CI succeeded for <sha>, 1 otherwise (after printing why).
ci_wait() {
  local sha="$1" interval="${CI_STATUS_INTERVAL:-30}" timeout="${CI_STATUS_TIMEOUT:-1800}"
  local waited=0 runs status conclusion success pending others

  if [[ ! "$sha" =~ ^[0-9a-f]{40}$ ]]; then
    echo "::error::'$sha' is not a full commit SHA" >&2
    return 1
  fi
  if [[ ! "$interval" =~ ^[0-9]+$ || ! "$timeout" =~ ^[0-9]+$ ]]; then
    echo "::error::CI_STATUS_INTERVAL and CI_STATUS_TIMEOUT must be whole seconds" >&2
    return 1
  fi

  while :; do
    runs="$(ci_runs "$sha")" || return 1
    success=0 pending=0 others=""
    while read -r status conclusion; do
      [[ -n "${status:-}" ]] || continue
      if [[ "$status" == completed ]]; then
        if [[ "$conclusion" == success ]]; then
          success=$((success + 1))
        else
          others="$others $conclusion"
        fi
      else
        pending=$((pending + 1))
      fi
    done <<<"$runs"

    if ((success > 0)); then
      echo "commit $sha has a successful $CI_WORKFLOW run from a push to main"
      return 0
    fi
    if ((pending == 0)); then
      if [[ -z "$others" ]]; then
        echo "::error::commit $sha has no successful CI run from a push to main; merge to main, wait for CI, then tag" >&2
      else
        echo "::error::commit $sha has no successful CI run from a push to main (concluded:$others); fix CI on main, then tag a commit that passed" >&2
      fi
      return 1
    fi
    if ((waited >= timeout)); then
      echo "::error::CI for commit $sha is still running after ${timeout}s; wait for it, then re-run this job" >&2
      return 1
    fi
    echo "CI for $sha is still running ($pending run(s)); waited ${waited}s of ${timeout}s"
    sleep "$interval"
    waited=$((waited + interval))
    # Guard against a zero interval looping forever.
    if ((interval == 0)); then
      waited=$((waited + 1))
    fi
  done
}

main() {
  if [[ $# -ne 1 ]]; then
    echo "usage: ci-status.sh <commit-sha>" >&2
    return 2
  fi
  if [[ -z "${GITHUB_REPOSITORY:-}" ]]; then
    echo "::error::GITHUB_REPOSITORY is not set" >&2
    return 1
  fi
  ci_wait "$1"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  set -euo pipefail
  main "$@"
fi
