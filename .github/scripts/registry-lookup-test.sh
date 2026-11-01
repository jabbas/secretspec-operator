#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Hermetic test of show_log in registry-lookup.sh: no line of the output may
# start with `::` (a workflow command), also after a carriage return.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR source=registry-lookup.sh
source "$here/registry-lookup.sh"

log="$(mktemp)"
trap 'rm -f "$log"' EXIT

printf 'Error: boom\n::error::injected\r::warning::also injected\nlast line' > "$log"
out="$(show_log "$log")"

if grep -q '^::' <<<"$out"; then
  echo "FAIL: show_log output has a line that starts with ::" >&2
  echo "$out" >&2
  exit 1
fi
for needle in 'Error: boom' '::error::injected' '::warning::also injected' 'last line'; do
  if ! grep -qF -- "$needle" <<<"$out"; then
    echo "FAIL: show_log lost the text '$needle'" >&2
    exit 1
  fi
done
echo "PASS: show_log prefixes every line, carriage returns included, and keeps the text"
echo "all registry-lookup tests passed"
