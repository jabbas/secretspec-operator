#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Tests for helm-index.sh (the index update of the `helm-index` job in
# release.yml). Hermetic: temporary directories and a local bare git remote,
# no network, nothing is pushed anywhere else. Needs helm, yq (mikefarah v4)
# and git. Run it from anywhere:
#
#   bash .github/scripts/helm-index-test.sh
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

fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}
pass() { echo "PASS: $*"; }

# package <version>: packages the real chart at a fake version into pkg/.
package() {
  helm package "$chart_dir" --version "$1" --app-version "$1" --destination pkg > /dev/null
}
sha() { sha256sum "$1" | cut -d' ' -f1; }
commits() { git -C remote.git rev-list --count main; }

# The "published" repository starts with 0.1.0 and 0.1.0-rc.1, indexed by helm
# itself, so the fixture has the shape of a real index.
mkdir seed-pkg
for old in 0.1.0 0.1.0-rc.1; do
  helm package "$chart_dir" --version "$old" --app-version "$old" --destination seed-pkg > /dev/null
done
git init -q --bare -b main remote.git
git init -q -b main seed
helm repo index seed-pkg --url "$BASE/$V-old"
cp seed-pkg/index.yaml seed/index.yaml
git -C seed -c user.name=seed -c user.email=seed@example.invalid add index.yaml
git -C seed -c user.name=seed -c user.email=seed@example.invalid commit -q -m seed
git -C seed push -q "$work/remote.git" HEAD:main
git clone -q remote.git repo

package "$V"
tgz="pkg/secretspec-operator-$V.tgz"
digest="$(sha "$tgz")"
before="$(yq '.entries.secretspec-operator | length' repo/index.yaml)"
old_entries="$(yq -o=json -I=0 '[.entries.secretspec-operator[]] | sort_by(.version)' repo/index.yaml)"

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
[[ "$(yq -o=json -I=0 '[.entries.secretspec-operator[] | select(.version != "'"$V"'")] | sort_by(.version)' got.yaml)" \
  == "$old_entries" ]] || fail "1: old entries changed"
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

if [[ $failures != 0 ]]; then
  echo "$failures check(s) failed" >&2
  exit 1
fi
echo "all helm-index tests passed"
