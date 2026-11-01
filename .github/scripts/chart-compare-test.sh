#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Tests for chart-compare.sh (the chart check of the promote mode in
# release.yml). Hermetic: small chart directories under a temporary directory,
# no helm, no network. Run it from anywhere:
#
#   bash .github/scripts/chart-compare-test.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$here/chart-compare.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failures=0
fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}
pass() { echo "PASS: $*"; }

# make_chart <dir> <version>: a small chart with the files of the real one that
# matter here (Chart.yaml, a template, the CRD, values).
make_chart() {
  local dir="$1" version="$2"
  mkdir -p "$dir/templates" "$dir/files/crd"
  printf 'apiVersion: v2\nname: secretspec-operator\ndescription: test\ntype: application\nversion: %s\nappVersion: %s\nkubeVersion: ">=1.34.0-0"\n' \
    "$version" "$version" > "$dir/Chart.yaml"
  printf 'image: {{ .Values.image }}:{{ .Chart.AppVersion }}\n' > "$dir/templates/deployment.yaml"
  printf 'kind: CustomResourceDefinition\nspec:\n  version: v1alpha1\n' > "$dir/files/crd/secretspecs.yaml"
  printf 'image: ghcr.io/jabbas/secretspec-operator\nreplicas: 1\n' > "$dir/values.yaml"
}

# scenario <name>: a release candidate chart and an identical final chart.
scenario() {
  rc_dir="$work/$1/rc"
  final_dir="$work/$1/final"
  rm -rf "${work:?}/$1"
  make_chart "$rc_dir" 0.1.3-rc.2
  make_chart "$final_dir" 0.1.3
}

execute() {
  rc=0
  out="$(bash "$script" "$rc_dir" "$final_dir" 2>&1)" || rc=$?
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

scenario identical-but-version
execute
expect_pass "identical except version and appVersion passes"

scenario template-differs
printf 'image: evil:{{ .Chart.AppVersion }}\n' > "$final_dir/templates/deployment.yaml"
execute
expect_fail "a template that differs fails" "templates/deployment.yaml differs"

scenario crd-differs
printf 'kind: CustomResourceDefinition\nspec:\n  version: v1alpha2\n' > "$final_dir/files/crd/secretspecs.yaml"
execute
expect_fail "a CRD that differs fails" "files/crd/secretspecs.yaml differs"

scenario values-differs
printf 'image: ghcr.io/jabbas/secretspec-operator\nreplicas: 2\n' > "$final_dir/values.yaml"
execute
expect_fail "values that differ fail" "values.yaml differs"

scenario chart-field-differs
printf 'apiVersion: v2\nname: secretspec-operator\ndescription: test\ntype: application\nversion: 0.1.3\nappVersion: 0.1.3\nkubeVersion: ">=1.30.0-0"\n' \
  > "$final_dir/Chart.yaml"
execute
expect_fail "an extra Chart.yaml field that differs fails" "Chart.yaml differs in more than version and appVersion"

scenario chart-field-added
printf 'deprecated: true\n' >> "$final_dir/Chart.yaml"
execute
expect_fail "a Chart.yaml field that is only in the final chart fails" "Chart.yaml differs in more than version and appVersion"

scenario file-added
printf 'extra\n' > "$final_dir/templates/extra.yaml"
execute
expect_fail "a file only in the final chart fails" "do not have the same files"

scenario file-missing
rm "$final_dir/values.yaml"
execute
expect_fail "a file only in the release candidate chart fails" "do not have the same files"

scenario duplicate-version
printf 'version: 9.9.9\n' >> "$final_dir/Chart.yaml"
execute
expect_fail "a second version line fails" "has 2 'version:' lines"

scenario missing-app-version
printf 'apiVersion: v2\nname: secretspec-operator\ndescription: test\ntype: application\nversion: 0.1.3\nkubeVersion: ">=1.34.0-0"\n' \
  > "$final_dir/Chart.yaml"
execute
expect_fail "a missing appVersion line fails" "has 0 'appVersion:' lines"

scenario not-a-chart
rm "$final_dir/Chart.yaml"
execute
expect_fail "a directory without Chart.yaml fails" "is not an extracted chart"

scenario missing-directory
rm -rf "$final_dir"
execute
expect_fail "a missing directory fails" "is not an extracted chart"

echo
if ((failures > 0)); then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all chart-compare tests passed"
