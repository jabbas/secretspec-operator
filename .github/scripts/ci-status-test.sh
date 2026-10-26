#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Tests for ci-status.sh (the CI gate of the `verify` job in release.yml).
# Hermetic: a fake `gh` on PATH answers from files, nothing touches the
# network. Needs bash and jq. Run it from anywhere:
#
#   bash .github/scripts/ci-status-test.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$here/ci-status.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

SHA=2aa567477420e4128ca714dfbfcedafbf17425c5
OTHER=0123456789abcdef0123456789abcdef01234567
failures=0

fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}
pass() { echo "PASS: $*"; }

# Fake gh. Every call prints the next response file (answer.1, answer.2, ...;
# the last one repeats). A response file named *.error makes the call fail
# like `gh api` does on an HTTP error. The arguments are logged.
mkdir -p "$work/bin"
cat > "$work/bin/gh" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
dir="$FAKE_GH_DIR"
echo "$*" >> "$dir/calls"
n=$(($(cat "$dir/count" 2> /dev/null || echo 0) + 1))
echo "$n" > "$dir/count"
[[ "$1" == api ]] || { echo "fake gh: unexpected command $1" >&2; exit 99; }
while [[ ! -e "$dir/answer.$n" && ! -e "$dir/answer.$n.error" && $n -gt 1 ]]; do n=$((n - 1)); done
if [[ -e "$dir/answer.$n.error" ]]; then
  echo "gh: HTTP 502 (fake)" >&2
  exit 1
fi
cat "$dir/answer.$n"
FAKE
chmod +x "$work/bin/gh"

# scenario <name> starts a case; add API answers with answer/answer_error/
# answer_raw, `execute`, then check with expect_*. Output is in $out, the exit
# status in $rc.
scenario() {
  case_dir="$work/$1"
  rm -rf "$case_dir"
  mkdir -p "$case_dir"
  : > "$case_dir/calls"
  answers=0
}

# run <event> <branch> <sha> <status> <conclusion|null>: one run object.
run() {
  local conclusion="null"
  [[ "$5" == null ]] || conclusion="\"$5\""
  printf '{"path":".github/workflows/ci.yml","event":"%s","head_branch":"%s","head_sha":"%s","status":"%s","conclusion":%s}' \
    "$1" "$2" "$3" "$4" "$conclusion"
}

# answer <run>...: the next API answer lists these runs.
answer() {
  local list="" r
  for r in "$@"; do list="${list:+$list,}$r"; done
  answers=$((answers + 1))
  printf '{"total_count":%d,"workflow_runs":[%s]}' "$#" "$list" > "$case_dir/answer.$answers"
}
answer_raw() { # body
  answers=$((answers + 1))
  printf '%s' "$1" > "$case_dir/answer.$answers"
}
answer_error() {
  answers=$((answers + 1))
  : > "$case_dir/answer.$answers.error"
}

# execute [timeout-seconds [sha [interval-seconds]]]: runs ci-status.sh.
execute() {
  rc=0
  out="$(
    PATH="$work/bin:$PATH" FAKE_GH_DIR="$case_dir" GITHUB_REPOSITORY=example/repo \
      GH_TOKEN=unused CI_STATUS_INTERVAL="${3:-1}" CI_STATUS_TIMEOUT="${1:-5}" \
      CI_STATUS_ATTEMPTS=3 CI_STATUS_RETRY_WAIT=0 \
      bash "$script" "${2:-$SHA}" 2>&1
  )" || rc=$?
}

expect_pass() {
  if [[ "$rc" == 0 ]]; then pass "$1"; else fail "$1 (exit $rc): $out"; fi
}
expect_fail() { # <name> <message fragment>
  if [[ "$rc" != 0 ]] && grep -qF -- "$2" <<<"$out"; then
    pass "$1"
  else
    fail "$1 (exit $rc, wanted failure with '$2'): $out"
  fi
}
# expect_calls <n> <what>: the number of API calls the script made.
expect_calls() {
  local got
  got="$(cat "$case_dir/count" 2> /dev/null || echo 0)"
  if [[ "$got" == "$1" ]]; then pass "$2"; else fail "$2 (expected $1 API calls, got $got)"; fi
}

# --- success ---------------------------------------------------------------
scenario success
answer "$(run push main "$SHA" completed success)"
execute
expect_pass "a successful push run on main passes"
query_ok=true
for part in "actions/workflows/ci.yml/runs" "head_sha=$SHA" "event=push" "branch=main"; do
  grep -qF -- "$part" "$case_dir/calls" || query_ok=false
done
if [[ "$query_ok" == true ]]; then
  pass "asks for ci.yml push runs of the SHA on main"
else
  fail "unexpected API query: $(cat "$case_dir/calls")"
fi

scenario success-among-failures
answer "$(run push main "$SHA" completed failure)" "$(run push main "$SHA" completed success)"
execute
expect_pass "a success wins over an earlier failed attempt"

# --- in progress -> success ---------------------------------------------------
scenario wait-success
answer "$(run push main "$SHA" in_progress null)"
answer "$(run push main "$SHA" completed success)"
execute
expect_pass "in_progress, then success passes"
expect_calls 2 "polled twice"

scenario wait-queued
answer "$(run push main "$SHA" queued null)"
answer "$(run push main "$SHA" in_progress null)"
answer "$(run push main "$SHA" completed success)"
execute
expect_pass "queued, in_progress, then success passes"

# --- in progress -> failure ---------------------------------------------------
scenario wait-failure
answer "$(run push main "$SHA" in_progress null)"
answer "$(run push main "$SHA" completed failure)"
execute
expect_fail "in_progress, then failure fails" "no successful CI run from a push to main"

scenario cancelled
answer "$(run push main "$SHA" completed cancelled)"
execute
expect_fail "a cancelled run fails" "cancelled"

# --- ignored runs ---------------------------------------------------------------
scenario pr-only
answer "$(run pull_request feature "$SHA" completed success)"
execute
expect_fail "a PR run alone fails (its success does not count)" "merge to main, wait for CI, then tag"

scenario other-branch-and-sha
answer "$(run push feature "$SHA" completed success)" \
  "$(run push main "$OTHER" completed success)" \
  "$(run workflow_dispatch main "$SHA" completed success)"
execute
expect_fail "push on another branch, another SHA, other events do not count" "no successful CI run"

scenario pr-success-push-pending
answer "$(run pull_request feature "$SHA" completed success)" "$(run push main "$SHA" in_progress null)"
answer "$(run pull_request feature "$SHA" completed success)" "$(run push main "$SHA" completed failure)"
execute
expect_fail "a successful PR run does not end the wait for a push run in progress" "concluded: failure"
expect_calls 2 "waited for the push run"

scenario wrong-workflow-path
answer '{"path":".github/workflows/other.yml","event":"push","head_branch":"main","head_sha":"'"$SHA"'","status":"completed","conclusion":"success"}'
execute
expect_fail "a run of another workflow file does not count" "no successful CI run"

scenario completed-null-conclusion
answer "$(run push main "$SHA" completed null)"
execute
expect_fail "completed without a conclusion fails" "no successful CI run"

scenario pr-pending-ignored
answer "$(run pull_request feature "$SHA" in_progress null)"
execute
expect_fail "a PR run in progress is not waited for" "no successful CI run"
expect_calls 1 "no polling for ignored runs"

# --- no runs ------------------------------------------------------------------------
scenario no-runs
answer
execute
expect_fail "no runs at all fails" "commit $SHA has no successful CI run from a push to main; merge to main, wait for CI, then tag"

# --- API problems ---------------------------------------------------------------------
scenario api-error
answer_error
execute
expect_fail "a persistent API error fails" "gh api failed 3 time(s)"
expect_calls 3 "a persistent API error is retried, then given up"

scenario api-error-transient
answer_error
answer "$(run push main "$SHA" completed success)"
execute
expect_pass "one transient API error, then success, passes"
expect_calls 2 "the transient error was retried once"

scenario api-error-twice-transient
answer_error
answer_error
answer "$(run push main "$SHA" completed success)"
execute
expect_pass "two transient API errors, then success, pass"

scenario api-error-while-waiting
answer "$(run push main "$SHA" in_progress null)"
answer_error
execute
expect_fail "API errors while polling fail" "gh api failed"

scenario api-error-then-no-run
answer_error
answer
execute
expect_fail "a retried API error is never taken for success, an empty list still fails" "no successful CI run"

scenario api-malformed-element
answer_raw '{"total_count":1,"workflow_runs":[1]}'
execute
expect_fail "a run list element that is not an object fails" "not a workflow-run list"
expect_calls 1 "no retry for a malformed answer"

scenario api-null-element
answer_raw '{"total_count":1,"workflow_runs":[null]}'
execute
expect_fail "a null run list element fails" "not a workflow-run list"

scenario api-garbage
answer_raw '{"message":"Not Found"}'
execute
expect_fail "an answer that is not a run list fails" "not a workflow-run list"

scenario api-truncated
answer_raw '{"total_count":3,"workflow_runs":[]}'
execute
expect_fail "a cut-off list fails" "not all were returned"
expect_calls 1 "no retry for a cut-off list"

scenario api-html
answer_raw '<html>rate limited</html>'
execute
expect_fail "a non-JSON answer fails" "not a workflow-run list"

# --- timeout ---------------------------------------------------------------------------
scenario timeout
answer "$(run push main "$SHA" in_progress null)"
execute 2
expect_fail "still in_progress at the timeout fails" "still running after 2s"

scenario zero-interval
answer "$(run push main "$SHA" in_progress null)"
execute 3 "$SHA" 0
expect_fail "a zero interval still times out" "still running after 3s"
expect_calls 4 "a zero interval counts one second per poll"

# --- arguments ----------------------------------------------------------------------------
scenario bad-sha
execute 5 not-a-sha
expect_fail "a SHA that is not 40 hex digits fails" "not a full commit SHA"
expect_calls 0 "no API call for a bad SHA"

if [[ "$failures" -ne 0 ]]; then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all ci-status tests passed"
