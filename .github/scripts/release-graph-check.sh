#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Static check of the job graph of release.yml:
#
#   bash .github/scripts/release-graph-check.sh [workflow-file]
#
# The rule. A job with an `if:` can be skipped (the mode-dependent ones
# `build`, `licenses`, `promote`, `image`, `smoke`, and the joins after them).
# Under the implicit success() a job is skipped as well when ANY job in its
# `needs` was skipped, so a job that comes after a skippable job without a
# condition of its own silently never runs in one of the two modes (this
# happened to `helm-index`). Therefore every job that has a skippable job among
# its transitive `needs` must carry an explicit `if:` that
#   - contains `!cancelled()` or `always()` (replaces the implicit success()), and
#   - compares `needs.<job>.result` for every job in its own `needs`, and
#     every such comparison is exactly `== 'success'` or `== 'skipped'` (a
#     skipped dependency is a decision, not an accident; a failed or cancelled
#     one can never pass, which `!= 'failure'` would allow for a cancelled
#     one). `== 'skipped'` is only meaningful for a job that has an `if:`
#     itself, i.e. one that can be skipped.
#
# Limitation (principle #17): the check looks at the text of the `if:`, not at
# its logic. It does not parse the expression, so it cannot prove that the
# comparisons are combined correctly (`&&` vs `||`); it only guarantees that
# each dependency is mentioned and compared in the two allowed ways, which
# covers the expression shapes used in release.yml. The review of the
# combination stays with a human.
#
# "Skippable" is derived (any job that has an `if:`), not listed by hand, so a
# job added later is covered without editing this script. That is a superset of
# the hand-written list of mode-dependent jobs, at no extra cost.
#
# Needs yq (mikefarah, v4). Exit status: 0 = rule holds, 1 = violation,
# 2 = the file could not be read.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workflow="${1:-$here/../workflows/release.yml}"

if [[ ! -r "$workflow" ]]; then
  echo "release-graph-check: cannot read '$workflow'" >&2
  exit 2
fi

# One "<job> <need>" line per dependency; `needs` is a string or a list.
# shellcheck disable=SC2016 # $job is a yq variable, not a shell one
edges="$(yq '(.jobs // {}) | to_entries | .[] | select(.value | has("needs")) | .key as $job
  | ((.value.needs | select(kind == "seq") | .[]) // (.value.needs | select(kind == "scalar")))
  | $job + " " + .' "$workflow")"
jobs="$(yq '(.jobs // {}) | keys | .[]' "$workflow")"
skippable="$(yq '(.jobs // {}) | to_entries | .[] | select(.value | has("if")) | .key' "$workflow")"

if [[ -z "$jobs" ]]; then
  echo "release-graph-check: '$workflow' has no jobs" >&2
  exit 2
fi

direct_needs() { # <job>
  awk -v job="$1" '$1 == job {print $2}' <<<"$edges"
}

# All jobs reachable through `needs`, one per line.
ancestors() { # <job>
  local pending="$1" seen="" current need
  while [[ -n "$pending" ]]; do
    current="${pending%%$'\n'*}"
    if [[ "$pending" == *$'\n'* ]]; then pending="${pending#*$'\n'}"; else pending=""; fi
    while IFS= read -r need; do
      [[ -n "$need" ]] || continue
      if grep -qxF -- "$need" <<<"$seen"; then continue; fi
      seen+="$need"$'\n'
      pending+="${pending:+$'\n'}$need"
    done < <(direct_needs "$current")
  done
  printf '%s' "$seen"
}

# <pattern> <text>: how many times the extended regular expression matches.
count_matches() {
  { grep -oE -- "$1" <<<"$2" || true; } | wc -l | tr -d ' '
}

violations=0
violation() { # <job> <message>
  echo "release-graph-check: FAIL job '$1': $2" >&2
  violations=$((violations + 1))
}

while IFS= read -r job; do
  # The first skippable ancestor, if any: that is what makes the rule apply.
  trigger=""
  while IFS= read -r ancestor; do
    [[ -n "$ancestor" ]] || continue
    if grep -qxF -- "$ancestor" <<<"$skippable"; then
      trigger="$ancestor"
      break
    fi
  done < <(ancestors "$job")
  [[ -n "$trigger" ]] || continue

  condition="$(JOB="$job" yq '.jobs[strenv(JOB)].if // ""' "$workflow")"
  if [[ -z "$condition" ]]; then
    violation "$job" "comes after the skippable job '$trigger' but has no 'if:'; the implicit success() skips it whenever '$trigger' is skipped"
    continue
  fi
  # yq returns the value without YAML comments; this is a second line of
  # defence, so that no comment text can ever satisfy a check below.
  condition="$(sed -E 's/(^|[[:space:]])#.*$//' <<<"$condition")"
  if [[ "$condition" != *'!cancelled()'* && "$condition" != *'always()'* ]]; then
    violation "$job" "comes after the skippable job '$trigger'; its 'if:' needs !cancelled() or always() to replace the implicit success()"
  fi
  while IFS= read -r need; do
    mentions="$(count_matches "needs\.$need\.result" "$condition")"
    exact="$(count_matches "needs\.$need\.result == '(success|skipped)'" "$condition")"
    skipped="$(count_matches "needs\.$need\.result == 'skipped'" "$condition")"
    if [[ "$mentions" -eq 0 ]]; then
      violation "$job" "its 'if:' does not check needs.$need.result"
    elif [[ "$mentions" -ne "$exact" ]]; then
      violation "$job" "needs.$need.result must be compared only as == 'success' or == 'skipped' (found another operator or value)"
    fi
    if [[ "$skipped" -gt 0 ]] && ! grep -qxF -- "$need" <<<"$skippable"; then
      violation "$job" "its 'if:' accepts '$need' as skipped, but '$need' has no 'if:' and is never skipped"
    fi
  done < <(direct_needs "$job")
done <<<"$jobs"

if [[ "$violations" -ne 0 ]]; then
  echo "release-graph-check: $violations violation(s); rule: 'Job graph' in docs/development.md" >&2
  exit 1
fi
echo "release-graph-check: ok ($(wc -l <<<"$jobs" | tr -d ' ') jobs checked)"
