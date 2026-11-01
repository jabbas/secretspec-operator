#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Tests for release-mode.sh (the release mode of the `verify` job in
# release.yml). Hermetic: a fake `gh` on PATH answers from files, nothing
# touches the network. Needs bash and jq. Run it from anywhere:
#
#   bash .github/scripts/release-mode-test.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$here/release-mode.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

COMMIT=2aa567477420e4128ca714dfbfcedafbf17425c5
OTHER=0123456789abcdef0123456789abcdef01234567
failures=0

fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}
pass() { echo "PASS: $*"; }

# Fake gh. `gh api [flags] repos/...` prints the file named after the path
# (query dropped, "/" -> "_") from $FAKE_GH_DIR/resp; a file with the suffix
# .error makes the call fail like an HTTP error; an unknown path is a 404.
mkdir -p "$work/bin"
cat > "$work/bin/gh" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
dir="$FAKE_GH_DIR"
echo "$*" >> "$dir/calls"
[[ "$1" == api ]] || { echo "fake gh: unexpected command $1" >&2; exit 99; }
path=""
for arg in "$@"; do
  case "$arg" in repos/*) path="$arg" ;; esac
done
[[ -n "$path" ]] || { echo "fake gh: no repos/ path in: $*" >&2; exit 98; }
key="${path%%\?*}"
key="${key//\//_}"
if [[ -e "$dir/resp/$key.error" ]]; then
  echo "gh: HTTP 502 (fake)" >&2
  exit 1
fi
if [[ ! -e "$dir/resp/$key" ]]; then
  echo "gh: HTTP 404 (fake): $path" >&2
  exit 1
fi
cat "$dir/resp/$key"
FAKE
chmod +x "$work/bin/gh"

# scenario <name> starts a case; describe the repository with ref_final,
# tag_object and rc_list, then `execute`, then check with expect_*.
scenario() {
  case_dir="$work/$1"
  rm -rf "$case_dir"
  mkdir -p "$case_dir/resp"
  : > "$case_dir/calls"
}

# put <api path> <body>
put() {
  local key="${1//\//_}"
  printf '%s' "$2" > "$case_dir/resp/$key"
}
put_error() {
  local key="${1//\//_}"
  : > "$case_dir/resp/$key.error"
}

# ref_tag <tag> <type> <sha>: the ref of a tag (git/ref/tags/<tag>).
ref_tag() {
  put "repos/example/repo/git/ref/tags/$1" \
    "$(jq -nc --arg ref "refs/tags/$1" --arg type "$2" --arg sha "$3" '{ref: $ref, object: {type: $type, sha: $sha}}')"
}

# tag_object <object-sha> <commit> <message>: an annotated tag object.
tag_object() {
  put "repos/example/repo/git/tags/$1" \
    "$(jq -nc --arg sha "$1" --arg commit "$2" --arg message "$3" \
      '{sha: $sha, message: $message, object: {type: "commit", sha: $commit}}')"
}

# Distinct, valid tag-object SHAs for the tests.
objsha() { printf '%040d' "$1"; }

# annotated <tag> <object-number> <commit> <message>: an annotated tag.
annotated() {
  ref_tag "$1" tag "$(objsha "$2")"
  tag_object "$(objsha "$2")" "$3" "$4"
}

# rc_list <base> <tag:type:sha>...: the matching-refs answer. With --slurp,
# gh prints one array per page.
rc_list() {
  local base="$1" entry items="" tag type sha
  shift
  for entry in "$@"; do
    IFS=: read -r tag type sha <<<"$entry"
    items="${items:+$items,}$(jq -nc --arg ref "refs/tags/$tag" --arg type "$type" --arg sha "$sha" \
      '{ref: $ref, object: {type: $type, sha: $sha}}')"
  done
  put "repos/example/repo/git/matching-refs/tags/$base-rc." "[[${items}]]"
}

# rc_annotated <tag> <object-number> <commit>: one annotated rc tag, its tag
# object included. Collect the entries for rc_list in $entries.
entries=()
rc_annotated() {
  tag_object "$(objsha "$2")" "$3" "$1"
  entries+=("$1:tag:$(objsha "$2")")
}

# execute <tag> [sha]: runs release-mode.sh. stdout (the GITHUB_OUTPUT lines)
# is in $out, stderr in $err, the exit status in $rc.
execute() {
  rc=0
  PATH="$work/bin:$PATH" FAKE_GH_DIR="$case_dir" GITHUB_REPOSITORY=example/repo GH_TOKEN=unused \
    bash "$script" "$1" "${2:-$COMMIT}" > "$case_dir/out" 2> "$case_dir/err" || rc=$?
  out="$(cat "$case_dir/out")"
  err="$(cat "$case_dir/err")"
}

expect_output() { # <name> <expected stdout>
  if [[ "$rc" == 0 && "$out" == "$2" ]]; then
    pass "$1"
  else
    fail "$1 (exit $rc, wanted '$2'): out='$out' err='$err'"
  fi
}
expect_fail() { # <name> <message fragment>
  if [[ "$rc" != 0 && -z "$out" ]] && grep -qF -- "$2" <<<"$err"; then
    pass "$1"
  else
    fail "$1 (exit $rc, wanted failure with '$2', no output): out='$out' err='$err'"
  fi
}
expect_message() { # <name> <fragment of stderr>
  if grep -qF -- "$2" <<<"$err"; then pass "$1"; else fail "$1 (wanted '$2'): $err"; fi
}
expect_no_calls() { # <name>
  if [[ ! -s "$case_dir/calls" ]]; then pass "$1"; else fail "$1: gh was called: $(cat "$case_dir/calls")"; fi
}

# --- release candidate -----------------------------------------------------
scenario rc-tag
execute 0.1.3-rc.2
expect_output "a release candidate tag is built" "mode=build"
expect_no_calls "a release candidate tag needs no API call"

# --- promote ---------------------------------------------------------------
scenario promote-highest
annotated 0.1.3 1 "$COMMIT" $'0.1.3\n'
entries=()
rc_annotated 0.1.3-rc.1 2 "$COMMIT"
rc_annotated 0.1.3-rc.2 3 "$COMMIT"
rc_list 0.1.3 "${entries[@]}"
execute 0.1.3
expect_output "rc.1 and rc.2 on the same commit: rc.2 is promoted" $'mode=promote\nrc_tag=0.1.3-rc.2'

scenario promote-numeric-order
annotated 0.1.3 1 "$COMMIT" $'0.1.3\n'
entries=()
rc_annotated 0.1.3-rc.9 2 "$COMMIT"
rc_annotated 0.1.3-rc.10 3 "$COMMIT"
rc_annotated 0.1.3-rc.2 4 "$COMMIT"
rc_list 0.1.3 "${entries[@]}"
execute 0.1.3
expect_output "rc.10 beats rc.9 (numeric order)" $'mode=promote\nrc_tag=0.1.3-rc.10'

scenario promote-skips-other-commits
annotated 0.1.3 1 "$COMMIT" $'0.1.3\n'
entries=()
rc_annotated 0.1.3-rc.1 2 "$COMMIT"
rc_annotated 0.1.3-rc.2 3 "$OTHER"
rc_list 0.1.3 "${entries[@]}"
execute 0.1.3
expect_output "a later rc on another commit is not promoted" $'mode=promote\nrc_tag=0.1.3-rc.1'

scenario promote-lightweight-rc
annotated 0.1.3 1 "$COMMIT" $'0.1.3\n'
rc_list 0.1.3 "0.1.3-rc.1:commit:$COMMIT"
execute 0.1.3
expect_output "a lightweight rc tag on the commit counts" $'mode=promote\nrc_tag=0.1.3-rc.1'

scenario promote-ignores-lookalikes
annotated 0.1.3 1 "$COMMIT" $'0.1.3\n'
rc_list 0.1.3 "0.1.3-rc.1:commit:$COMMIT" "0.1.3-rc.2x:commit:$COMMIT" "0.1.3-rc.02:commit:$COMMIT"
execute 0.1.3
expect_output "tags that are not X.Y.Z-rc.N are ignored" $'mode=promote\nrc_tag=0.1.3-rc.1'

scenario promote-lightweight-final
ref_tag 0.1.3 commit "$COMMIT"
rc_list 0.1.3 "0.1.3-rc.1:commit:$COMMIT"
execute 0.1.3
expect_output "a lightweight final tag with an rc is promoted" $'mode=promote\nrc_tag=0.1.3-rc.1'

# --- no release candidate --------------------------------------------------
scenario rc-other-commit
annotated 0.1.3 1 "$COMMIT" $'0.1.3\n'
entries=()
rc_annotated 0.1.3-rc.1 2 "$OTHER"
rc_list 0.1.3 "${entries[@]}"
execute 0.1.3
expect_fail "an rc on a different commit only fails" "no release candidate 0.1.3-rc.N on $COMMIT; cut a release candidate first, or add a 'rebuild: <reason>' line to the tag message"

scenario no-rc-no-marker
annotated 0.1.3 1 "$COMMIT" $'0.1.3\n\nrebuilt later, maybe\nnot rebuild: here\n'
rc_list 0.1.3
execute 0.1.3
expect_fail "no rc and no marker line fails" "no release candidate 0.1.3-rc.N"

scenario lightweight-no-rc
ref_tag 0.1.3 commit "$COMMIT"
rc_list 0.1.3
execute 0.1.3
expect_fail "a lightweight final tag without an rc fails" "no release candidate 0.1.3-rc.N"

# --- emergency rebuild -----------------------------------------------------
scenario rebuild-no-rc
annotated 0.1.4 1 "$COMMIT" $'0.1.4\n\nrebuild: CVE-2026-0001 hotfix\n'
rc_list 0.1.4
execute 0.1.4
expect_output "no rc and a rebuild line: build with the reason" $'mode=build\nrebuild_reason=CVE-2026-0001 hotfix'
expect_message "the reason is reported as a warning" "::warning::0.1.4 is rebuilt, not promoted from a release candidate: CVE-2026-0001 hotfix"

scenario rebuild-with-rc
annotated 0.1.4 1 "$COMMIT" $'0.1.4\n\nrebuild: base image CVE\n'
entries=()
rc_annotated 0.1.4-rc.1 2 "$COMMIT"
rc_list 0.1.4 "${entries[@]}"
execute 0.1.4
expect_output "a rebuild line wins over an existing rc" $'mode=build\nrebuild_reason=base image CVE'

scenario rebuild-crlf
annotated 0.1.4 1 "$COMMIT" $'0.1.4\r\n\r\nrebuild:   hotfix with CRLF  \r\n'
execute 0.1.4
expect_output "blanks and CR around the reason are trimmed" $'mode=build\nrebuild_reason=hotfix with CRLF'

scenario rebuild-first-line
annotated 0.1.4 1 "$COMMIT" $'0.1.4\n\nrebuild: first\nrebuild: second\n'
execute 0.1.4
expect_output "the first rebuild line is used" $'mode=build\nrebuild_reason=first'

scenario rebuild-empty
annotated 0.1.4 1 "$COMMIT" $'0.1.4\n\nrebuild:    \n'
rc_list 0.1.4
execute 0.1.4
expect_fail "a rebuild line without a reason fails" "has no reason"

scenario rebuild-allowed
annotated 0.1.4 1 "$COMMIT" $'0.1.4\n\nrebuild: CVE-2026-1234 hotfix (rustls)\n'
execute 0.1.4
expect_output "a normal reason is accepted" $'mode=build\nrebuild_reason=CVE-2026-1234 hotfix (rustls)'

scenario rebuild-allowed-punctuation
annotated 0.1.4 1 "$COMMIT" $'0.1.4\n\nrebuild: fix #12, see a/b.c:d_e +x-y\n'
execute 0.1.4
expect_output "every allowlisted punctuation character is accepted" $'mode=build\nrebuild_reason=fix #12, see a/b.c:d_e +x-y'

# Each of these must be refused, with the message that names the allowlist.
allowlist_message="may only contain the characters A-Z a-z 0-9 space . _ : / ( ) # , + - and at most 200 of them"
reject_reason() { # <name> <reason>
  scenario "rebuild-reject-$1"
  annotated 0.1.4 1 "$COMMIT" "0.1.4"$'\n\nrebuild: '"$2"$'\n'
  rc_list 0.1.4 "0.1.4-rc.1:commit:$COMMIT"
  execute 0.1.4
  expect_fail "a reason with $1 is rejected" "$allowlist_message"
}
reject_reason "an escaped newline (%0A)" 'oops%0A::error::x'
reject_reason "a percent sign" '100% sure'
reject_reason "backticks" $'run `id`'
reject_reason "a Markdown link" 'see [x](y)'
reject_reason "angle brackets" 'a <b> c'
reject_reason "a dollar sign" $'cost $HOME'
reject_reason "a quote" 'it'"'"'s "quoted"'
reject_reason "a semicolon" 'a; b'
reject_reason "non-ASCII letters" 'hotfix żółć'
reject_reason "a control character" $'bad \x1b[31mreason'
reject_reason "a backslash" 'a\nb'

scenario rebuild-length-limit
long="$(printf 'a%.0s' $(seq 1 200))"
annotated 0.1.4 1 "$COMMIT" "0.1.4"$'\n\nrebuild: '"$long"$'\n'
execute 0.1.4
expect_output "a reason of 200 characters is accepted" $'mode=build\nrebuild_reason='"$long"

scenario rebuild-too-long
annotated 0.1.4 1 "$COMMIT" "0.1.4"$'\n\nrebuild: '"${long}a"$'\n'
execute 0.1.4
expect_fail "a reason of 201 characters is rejected" "$allowlist_message"

# --- fail closed -----------------------------------------------------------
scenario api-error-ref
put_error repos/example/repo/git/ref/tags/0.1.3
rc_list 0.1.3 "0.1.3-rc.1:commit:$COMMIT"
execute 0.1.3
expect_fail "an API error reading the tag fails (no fallback to build)" "could not read the tag 0.1.3"

scenario api-error-tag-object
ref_tag 0.1.3 tag "$(objsha 1)"
put_error "repos/example/repo/git/tags/$(objsha 1)"
rc_list 0.1.3 "0.1.3-rc.1:commit:$COMMIT"
execute 0.1.3
expect_fail "an API error reading the tag object fails" "could not read the tag object"

scenario api-error-rc-list
annotated 0.1.3 1 "$COMMIT" $'0.1.3\n'
put_error repos/example/repo/git/matching-refs/tags/0.1.3-rc.
execute 0.1.3
expect_fail "an API error listing the rc tags fails (no fallback to build)" "could not list the 0.1.3-rc.* tags"

scenario api-error-rc-object
annotated 0.1.3 1 "$COMMIT" $'0.1.3\n'
rc_list 0.1.3 "0.1.3-rc.1:tag:$(objsha 2)"
put_error "repos/example/repo/git/tags/$(objsha 2)"
execute 0.1.3
expect_fail "an API error reading an rc tag object fails" "could not read the tag object"

scenario malformed-rc-list
annotated 0.1.3 1 "$COMMIT" $'0.1.3\n'
put repos/example/repo/git/matching-refs/tags/0.1.3-rc. '{"message":"Not Found"}'
execute 0.1.3
expect_fail "an rc list that is not a list of references fails" "is not a list of references"

scenario malformed-ref
put repos/example/repo/git/ref/tags/0.1.3 '[{"ref":"refs/tags/0.1.3-rc.1"}]'
execute 0.1.3
expect_fail "an answer that is not the tag reference fails" "is not a tag reference"

scenario tag-moved
annotated 0.1.3 1 "$OTHER" $'0.1.3\n'
rc_list 0.1.3 "0.1.3-rc.1:commit:$OTHER"
execute 0.1.3
expect_fail "a tag that peels to another commit than the run's fails" "but this run is for $COMMIT"

scenario nested-tag
ref_tag 0.1.3 tree "$(objsha 1)"
execute 0.1.3
expect_fail "a tag that points at a non-commit fails" "not at a commit"

scenario invalid-tag
execute v0.1.3
expect_fail "a tag that is neither X.Y.Z nor X.Y.Z-rc.N fails" "neither X.Y.Z nor X.Y.Z-rc.N"
expect_no_calls "an invalid tag needs no API call"

scenario invalid-sha
execute 0.1.3 abc
expect_fail "a short commit SHA fails" "not a full commit SHA"

echo
if ((failures > 0)); then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all release-mode tests passed"
