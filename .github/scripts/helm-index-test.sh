#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Tests for helm-index.sh (the index update of the `helm-index` job in
# release.yml). Hermetic: temporary directories and a local bare git remote,
# no network (a local HTTP server stands in for Pages), nothing is pushed
# anywhere else. Needs helm, yq (mikefarah v4), jq, git and python3. Run it from
# anywhere; HELM selects another helm binary:
#
#   bash .github/scripts/helm-index-test.sh
#   HELM=/path/to/helm bash .github/scripts/helm-index-test.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
chart_dir="$root/charts/secretspec-operator"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

# No user or system git configuration (signing, hooks, default branch, ...).
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export GIT_TERMINAL_PROMPT=0

# shellcheck source-path=SCRIPTDIR source=helm-index.sh
source "$here/helm-index.sh"

BASE=https://example.invalid/releases/download
V=0.1.1-rc.1
URL_BASE="$BASE/$V"
failures=0

echo "helm: $("${HELM:-helm}" version --short)"

fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}
pass() { echo "PASS: $*"; }

# package <version>: packages the real chart at a fake version into pkg/.
package() {
  helm_cmd package "$chart_dir" --version "$1" --app-version "$1" --destination pkg > /dev/null
}
sha() { sha256sum "$1" | cut -d' ' -f1; }
commits() { git -C remote.git rev-list --count main; }

# The "published" repository starts with 0.1.0 and 0.1.0-rc.1, indexed by helm
# itself, so the fixture has the shape of a real index.
mkdir seed-pkg
for old in 0.1.0 0.1.0-rc.1; do
  helm_cmd package "$chart_dir" --version "$old" --app-version "$old" --destination seed-pkg > /dev/null
done
git init -q --bare -b main remote.git
git init -q -b main seed
helm_cmd repo index seed-pkg --url "$BASE/$V-old"
cp seed-pkg/index.yaml seed/index.yaml
# The index is shared: add what helm itself would drop or normalise. A foreign
# chart with a valid and an invalid entry (no apiVersion, no created), unknown
# entry fields, and unknown top-level fields.
yq -i '
  .annotations = {"example.com/note": "keep"}
  | .serverInfo = {"contextPath": "/helm"}
  | (.entries.secretspec-operator[] | select(.version == "0.1.0"))."x-unknown" = {"keep": ["me", 1]}
  | .entries.other-chart = [
      {"name": "other-chart", "version": "1.0.0", "apiVersion": "v2", "created": "2026-01-01T00:00:00Z",
       "digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
       "urls": ["https://example.invalid/other-1.0.0.tgz"], "x-custom": {"a": [1, 2]}},
      {"name": "other-chart", "version": "0.9.0", "digest": "bbbb", "urls": ["https://example.invalid/other-0.9.0.tgz"]}]' seed/index.yaml
cp seed/index.yaml seed-index.yaml
git -C seed -c user.name=seed -c user.email=seed@example.invalid add index.yaml
git -C seed -c user.name=seed -c user.email=seed@example.invalid commit -q -m seed
git -C seed push -q "$work/remote.git" HEAD:main
git clone -q remote.git repo

package "$V"
tgz="pkg/secretspec-operator-$V.tgz"
digest="$(sha "$tgz")"
before="$(yq '.entries.secretspec-operator | length' repo/index.yaml)"
# The whole document as data, without the chart's version $2 and `generated`,
# independent of the library's own comparison (jq, not yq).
doc() {
  yq -o=json "$1" | jq -S -c --arg v "$2" '
    del(.generated)
    | .entries |= with_entries(.value |= (map(select(.name != "secretspec-operator" or .version != $v)) | sort_by(.version)))'
}

# 1. A new version is added, committed by the bot, and pushed.
f=$failures
publish_index repo "$tgz" "$V" "$digest" "$URL_BASE" 3
git -C remote.git show main:index.yaml > got.yaml
[[ "$(git -C remote.git log -1 --format=%s main)" == "secretspec-operator $V" ]] || fail "1: commit subject"
[[ "$(git -C remote.git log -1 --format=%an main)" == "github-actions[bot]" ]] || fail "1: commit author"
[[ "$(yq '.entries.secretspec-operator | length' got.yaml)" == "$((before + 1))" ]] || fail "1: entry count"
[[ "$(index_entry_digest got.yaml "$V")" == "$digest" ]] || fail "1: digest"
[[ "$(V=$V yq '.entries.secretspec-operator[] | select(.version == strenv(V)) | .urls[0]' got.yaml)" \
  == "$URL_BASE/secretspec-operator-$V.tgz" ]] || fail "1: url"
[[ "$(doc got.yaml "$V")" == "$(doc seed-index.yaml "$V")" ]] || fail "1: the document changed besides the new entry"
[[ "$(yq '.annotations."example.com/note"' got.yaml)" == keep ]] || fail "1: top-level annotations lost"
[[ "$(yq '.serverInfo.contextPath' got.yaml)" == /helm ]] || fail "1: serverInfo lost"
[[ "$(yq '.entries.other-chart | length' got.yaml)" == 2 ]] || fail "1: the foreign chart changed"
[[ "$(yq '.entries.other-chart[1] | has("apiVersion")' got.yaml)" == false ]] || fail "1: a foreign entry was normalised"
[[ "$(yq '.entries.other-chart[0]."x-custom".a[1]' got.yaml)" == 2 ]] || fail "1: unknown foreign fields lost"
[[ "$(yq '(.entries.secretspec-operator[] | select(.version == "0.1.0"))."x-unknown".keep[0]' got.yaml)" == me ]] || fail "1: unknown entry fields lost"
[[ "$(git -C remote.git ls-tree -r --name-only main)" == "index.yaml" ]] || fail "1: the repository must hold only index.yaml"
[[ -z "$(find repo -name '*.tgz' -not -path 'repo/.git/*')" ]] || fail "1: a package landed in the repository"
[[ $failures == "$f" ]] && pass "1: new version is added, committed by the bot, only index.yaml changes"
after_first="$(commits)"

# 2. The same digest again is a no-op success.
f=$failures
publish_index repo "$tgz" "$V" "$digest" "$URL_BASE" 3
[[ "$(commits)" == "$after_first" ]] || fail "2: a commit was made"
[[ $failures == "$f" ]] && pass "2: same-digest re-run is a no-op"

# 3. Another digest for a listed version fails and pushes nothing.
f=$failures
other="$(printf 'x' | sha256sum | cut -d' ' -f1)"
if publish_index repo "$tgz" "$V" "$other" "$URL_BASE" 3 2> err.txt; then
  fail "3: a different digest was accepted"
fi
grep -q 'already in the index with digest' err.txt || fail "3: error message"
[[ "$(commits)" == "$after_first" ]] || fail "3: a commit was made"
[[ $failures == "$f" ]] && pass "3: different digest fails, nothing pushed"

# 4. A missing or invalid entry field fails closed.
f=$failures
mkdir broken
expect_fail() {
  local name="$1"
  shift
  if "$@" 2> err.txt; then
    fail "4$name: accepted"
  elif ! grep -q '::error::' err.txt; then
    fail "4$name: failed without an ::error:: message"
  fi
}
# a: the listed version has no digest
cp got.yaml broken/index.yaml
V=$V yq -i '(.entries.secretspec-operator[] | select(.version == strenv(V))) |= del(.digest)' broken/index.yaml
expect_fail a update_index broken "$tgz" "$V" "$digest" "$URL_BASE"
# b: the listed version has a malformed digest
V=$V yq -i '(.entries.secretspec-operator[] | select(.version == strenv(V))).digest = "abc"' broken/index.yaml
expect_fail b update_index broken "$tgz" "$V" "$digest" "$URL_BASE"
# c: the version is listed twice
cp got.yaml broken/index.yaml
yq -i '.entries.secretspec-operator += [.entries.secretspec-operator[0]]' broken/index.yaml
expect_fail c update_index broken "$tgz" "$V" "$digest" "$URL_BASE"
# d: the merged index has an entry without the required fields
cp repo/index.yaml old.yaml
V=$V yq -i 'del(.entries.secretspec-operator[] | select(.version == strenv(V)))' old.yaml
cp old.yaml new.yaml
entry_url="$URL_BASE/secretspec-operator-$V.tgz"
V=$V D=$digest U=$entry_url yq -i \
  '.entries.secretspec-operator += [{"name": "secretspec-operator", "version": strenv(V), "urls": [strenv(U)], "digest": strenv(D)}]' new.yaml
expect_fail d check_new_index old.yaml new.yaml "$V" "$digest" "$entry_url"
# e: not a Helm repository index
echo "foo: bar" > broken/index.yaml
expect_fail e update_index broken "$tgz" "$V" "$digest" "$URL_BASE"
# f: the digest argument is not a sha256
expect_fail f update_index repo "$tgz" "$V" "sha256:$digest" "$URL_BASE"
[[ $failures == "$f" ]] && pass "4: missing or invalid index fields fail closed (a-f)"

# 5. A push race (another chart updated the index first) is retried from the
#    new remote state and the concurrent change is kept.
f=$failures
V2=0.1.1-rc.2
package "$V2"
tgz2="pkg/secretspec-operator-$V2.tgz"
digest2="$(sha "$tgz2")"
git clone -q remote.git other
cat > repo/.git/hooks/pre-push << EOF
#!/usr/bin/env bash
# First push only: another writer gets in first.
rm -f "$work/repo/.git/hooks/pre-push"
cd "$work/other"
git pull -q
echo race > race.txt
git add race.txt
git -c user.name=other -c user.email=other@example.invalid commit -q -m race
git push -q origin HEAD:main
EOF
chmod +x repo/.git/hooks/pre-push
PUBLISH_RETRY_SLEEP=0 publish_index repo "$tgz2" "$V2" "$digest2" "$BASE/$V2" 3 2> err.txt
git -C remote.git show main:index.yaml > got2.yaml
[[ "$(index_entry_digest got2.yaml "$V2")" == "$digest2" ]] || fail "5: the new version is missing"
[[ "$(index_entry_digest got2.yaml "$V")" == "$digest" ]] || fail "5: an earlier version was lost"
git -C remote.git cat-file -e main:race.txt || fail "5: the concurrent commit was lost"
grep -q 'push rejected' err.txt || fail "5: no push was rejected, the race did not happen"
[[ $failures == "$f" ]] && pass "5: push race is retried and the concurrent change is kept"

# 6. The push never succeeds: the retries are bounded.
f=$failures
V3=0.1.1-rc.3
package "$V3"
tgz3="pkg/secretspec-operator-$V3.tgz"
digest3="$(sha "$tgz3")"
printf '#!/usr/bin/env bash\nexit 1\n' > repo/.git/hooks/pre-push
chmod +x repo/.git/hooks/pre-push
if PUBLISH_RETRY_SLEEP=0 publish_index repo "$tgz3" "$V3" "$digest3" "$BASE/$V3" 3 2> err.txt; then
  fail "6: a rejected push was reported as success"
fi
[[ "$(grep -c 'push rejected' err.txt)" == 3 ]] || fail "6: expected 3 attempts"
grep -q 'could not push the index after 3 attempts' err.txt || fail "6: error message"
[[ $failures == "$f" ]] && pass "6: a push that never succeeds fails after the bounded retries"

rm -f repo/.git/hooks/pre-push

# 7. Whatever the new document changes besides the new entry fails, however
#    small: foreign entries, unknown fields, top-level fields.
f=$failures
entry_url="$URL_BASE/secretspec-operator-$V.tgz"
check_new_index seed-index.yaml got.yaml "$V" "$digest" "$entry_url" 2> err.txt || fail "7: the good document was rejected"
tamper() {
  local name="$1" expr="$2"
  yq "$expr" got.yaml > tampered.yaml
  if check_new_index seed-index.yaml tampered.yaml "$V" "$digest" "$entry_url" 2> err.txt; then
    fail "7$name: accepted"
  elif ! grep -q 'differs from the old one' err.txt; then
    fail "7$name: wrong error: $(head -1 err.txt)"
  fi
}
tamper a 'del(.entries.other-chart[1])'
tamper b '.entries.other-chart[1].apiVersion = "v2"'
tamper c 'del(.annotations)'
tamper d 'del(.serverInfo)'
tamper e 'del((.entries.secretspec-operator[] | select(.version == "0.1.0"))."x-unknown")'
tamper f 'del(.entries.other-chart[0]."x-custom")'
tamper g '(.entries.secretspec-operator[] | select(.version == "0.1.0")).digest = "00"'
tamper h '.apiVersion = "v2"'
tamper i '.entries.new-chart = [{"name": "new-chart", "version": "1.0.0"}]'
tamper j '.entries.other-chart = []'
[[ $failures == "$f" ]] && pass "7: any change besides the new entry fails (a-j)"

# 8. The token reaches git for one command only: not in .git/config, not in
#    the output.
f=$failures
V4=0.1.1-rc.4
package "$V4"
tgz4="pkg/secretspec-operator-$V4.tgz"
digest4="$(sha "$tgz4")"
token=ghp_fake_test_token_0123456789
HELM_CHARTS_TOKEN=$token git_authed -C repo config --get http.https://github.com/.extraheader > header.txt || fail "8: no header reached git"
grep -q '^AUTHORIZATION: basic ' header.txt || fail "8: unexpected header"
git -C repo config --get http.https://github.com/.extraheader > /dev/null 2>&1 && fail "8: the header stayed in the git config"
HELM_CHARTS_TOKEN=$token publish_index repo "$tgz4" "$V4" "$digest4" "$BASE/$V4" 3 > out.txt 2>&1 || fail "8: publish failed"
grep -rqF "$token" out.txt repo/.git remote.git 2> /dev/null && fail "8: the token was written somewhere"
grep -qF "$(printf 'x-access-token:%s' "$token" | base64 | tr -d '\n')" out.txt && fail "8: the encoded token was printed"
git -C remote.git show main:index.yaml > got4.yaml
[[ "$(index_entry_digest got4.yaml "$V4")" == "$digest4" ]] || fail "8: not pushed"
[[ $failures == "$f" ]] && pass "8: the token is passed to git per command, never stored or printed"

# 9. wait_for_pages: listed, not listed (bounded), and a different digest.
f=$failures
mkdir site
git -C remote.git show main:index.yaml > site/index.yaml
port="$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
python3 -m http.server "$port" --bind 127.0.0.1 --directory site > /dev/null 2>&1 &
server=$!
trap 'kill $server 2> /dev/null || true; rm -rf "$work"' EXIT
for _ in $(seq 50); do
  if curl -fs "http://127.0.0.1:$port/index.yaml" > /dev/null; then break; fi
  sleep 0.1
done
pages="http://127.0.0.1:$port/index.yaml"
wait_for_pages "$pages" "$V" "$digest" 2 0 > /dev/null || fail "9: a listed version was not found"
rc=0
wait_for_pages "$pages" 9.9.9 "$digest" 2 0 > /dev/null 2>&1 || rc=$?
[[ $rc == 1 ]] || fail "9: an unlisted version must return 1, got $rc"
rc=0
wait_for_pages "$pages" "$V" "$other" 2 0 > /dev/null 2> err.txt || rc=$?
[[ $rc == 2 ]] || fail "9: a different digest must return 2, got $rc"
grep -q 'not ' err.txt || fail "9: error message"
[[ $failures == "$f" ]] && pass "9: Pages check: listed -> 0, not listed -> 1, different digest -> 2"

if [[ $failures != 0 ]]; then
  echo "$failures check(s) failed" >&2
  exit 1
fi
echo "all helm-index tests passed"
