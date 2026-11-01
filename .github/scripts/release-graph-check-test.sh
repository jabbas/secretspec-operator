#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Tests for release-graph-check.sh. Hermetic: small workflow files under a
# temporary directory, no network. Needs yq. Run it from anywhere:
#
#   bash .github/scripts/release-graph-check-test.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$here/release-graph-check.sh"
real_workflow="$here/../workflows/release.yml"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failures=0
fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}
pass() { echo "PASS: $*"; }

# check <file>: sets rc and out.
check() {
  rc=0
  out="$(bash "$script" "$1" 2>&1)" || rc=$?
}

expect_pass() { # <name>
  if [[ "$rc" == 0 ]]; then pass "$1"; else fail "$1 (exit $rc): $out"; fi
}
expect_violation() { # <name> <message fragment>
  if [[ "$rc" == 1 ]] && grep -qF -- "$2" <<<"$out"; then
    pass "$1"
  else
    fail "$1 (exit $rc, wanted a violation with '$2'): $out"
  fi
}

# The release graph in miniature: `verify`, one job that only runs in one mode
# (`promote`), and a `chart` join after it. $1 is the `if:` of `publish`, a
# job after `chart`; "-" leaves it out.
write_workflow() { # <file> <publish if>
  local file="$1" condition="$2"
  {
    cat <<'EOF'
jobs:
  verify:
    runs-on: ubuntu-24.04
  promote:
    needs: verify
    if: needs.verify.outputs.mode == 'promote'
    runs-on: ubuntu-24.04
  chart:
    needs: [verify, promote]
    if: ${{ !cancelled() && needs.verify.result == 'success' && needs.promote.result != 'failure' }}
    runs-on: ubuntu-24.04
  publish:
    needs: [verify, chart]
EOF
    if [[ "$condition" != "-" ]]; then
      printf '    if: %s\n' "$condition"
    fi
    printf '    runs-on: ubuntu-24.04\n'
  } > "$file"
}

# --- the real workflow ---
check "$real_workflow"
expect_pass "the real release.yml satisfies the rule"

# --- fixtures ---
write_workflow "$work/good.yml" "\${{ !cancelled() && needs.verify.result == 'success' && needs.chart.result == 'success' }}"
check "$work/good.yml"
expect_pass "a join with !cancelled() and a result check per dependency passes"

write_workflow "$work/always.yml" "\${{ always() && needs.verify.result == 'success' && needs.chart.result == 'success' }}"
check "$work/always.yml"
expect_pass "always() is accepted in place of !cancelled()"

write_workflow "$work/no-if.yml" "-"
check "$work/no-if.yml"
expect_violation "no 'if:' after a skippable job fails (the helm-index bug)" "job 'publish': comes after the skippable job"

write_workflow "$work/no-status.yml" "\${{ needs.verify.result == 'success' && needs.chart.result == 'success' }}"
check "$work/no-status.yml"
expect_violation "an 'if:' without !cancelled() or always() fails" "needs !cancelled() or always()"

write_workflow "$work/missing-result.yml" "\${{ !cancelled() && needs.verify.result == 'success' }}"
check "$work/missing-result.yml"
expect_violation "an 'if:' that skips one dependency's result fails" "does not check needs.chart.result"

# `needs` of a single job as a string, and a dependency two levels down.
cat > "$work/transitive.yml" <<'EOF'
jobs:
  verify:
    runs-on: ubuntu-24.04
  maybe:
    needs: verify
    if: needs.verify.outputs.mode == 'build'
    runs-on: ubuntu-24.04
  middle:
    needs: maybe
    if: ${{ !cancelled() && needs.maybe.result == 'success' }}
    runs-on: ubuntu-24.04
  last:
    needs: middle
    runs-on: ubuntu-24.04
EOF
check "$work/transitive.yml"
expect_violation "a job that only reaches the skippable job through another job fails" "job 'last'"

cat > "$work/no-skippable.yml" <<'EOF'
jobs:
  verify:
    runs-on: ubuntu-24.04
  build:
    needs: verify
    runs-on: ubuntu-24.04
  publish:
    needs: [verify, build]
    runs-on: ubuntu-24.04
EOF
check "$work/no-skippable.yml"
expect_pass "a graph without any 'if:' needs no conditions"

cat > "$work/mode-guard-only.yml" <<'EOF'
jobs:
  verify:
    runs-on: ubuntu-24.04
  build:
    needs: verify
    if: needs.verify.outputs.mode == 'build'
    runs-on: ubuntu-24.04
  licenses:
    needs: verify
    if: needs.verify.outputs.mode == 'build'
    runs-on: ubuntu-24.04
EOF
check "$work/mode-guard-only.yml"
expect_pass "jobs that depend only on an unconditional job may carry a plain mode guard"

# --- input errors ---
rc=0
out="$(bash "$script" "$work/does-not-exist.yml" 2>&1)" || rc=$?
if [[ "$rc" == 2 ]] && grep -qF 'cannot read' <<<"$out"; then
  pass "a missing file exits with status 2"
else
  fail "a missing file exits with status 2 (exit $rc): $out"
fi

printf 'name: no jobs here\n' > "$work/no-jobs.yml"
check "$work/no-jobs.yml"
if [[ "$rc" == 2 ]] && grep -qF 'has no jobs' <<<"$out"; then
  pass "a file without jobs exits with status 2"
else
  fail "a file without jobs exits with status 2 (exit $rc): $out"
fi

if [[ "$failures" -ne 0 ]]; then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all release-graph-check tests passed"
