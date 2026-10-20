#!/bin/sh
# Headless tests for the TeXmacs MLS plugin (texmacs-plugin/mls).
#
# Each tests/texmacs/test-*.scm runs in its own TeXmacs process. Every process gets a
# fresh TEXMACS_HOME_PATH under a temporary directory with the plugin symlinked in, so
# ~/.TeXmacs and ~/.my-texmacs are never read for plugin state or written. TeXmacs runs
# under a perl alarm (MLS_TEST_TIMEOUT seconds, default 120; 300 with MLS_LEAN_TESTS=1).
# The stub engine (stub/mls) stands in for `mls` unless a test asks for the real binary.
# MLS_LEAN_TESTS=1 also enables test-real-review.scm (real engine, pinned Lean); without it
# that test prints SKIP and passes.
#
# MLS_TEST_KEEP=1 keeps the temporary directory (homes, work copies, stub logs) for inspection.
# Exit status: 0 when every test passes or TeXmacs is absent (skipped); 1 otherwise.
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
TEXMACS_BIN=${TEXMACS_BIN:-/Applications/TeXmacs.app/Contents/MacOS/TeXmacs}
# The real-Lean test (MLS_LEAN_TESTS=1) runs pinned Lean inside the TeXmacs process, so it
# gets a longer default alarm.
if [ "${MLS_LEAN_TESTS:-}" = 1 ]; then TIMEOUT=${MLS_TEST_TIMEOUT:-300}; else TIMEOUT=${MLS_TEST_TIMEOUT:-120}; fi

if [ ! -x "$TEXMACS_BIN" ]; then
  echo "SKIP: TeXmacs not found at $TEXMACS_BIN (set TEXMACS_BIN to run the TeXmacs tests)"
  exit 0
fi

REAL_BIN="$REPO/target/debug/mls"
REAL_OK=""
if [ -x "$REAL_BIN" ] && "$REAL_BIN" --help 2>&1 | grep -q 'review-status'; then
  REAL_OK=1
fi

TMPBASE=${TMPDIR:-/tmp}
TMPBASE=${TMPBASE%/}
TMP=$(mktemp -d "$TMPBASE/mls-texmacs.XXXXXX") || exit 1
if [ -z "${MLS_TEST_KEEP:-}" ]; then trap 'rm -rf "$TMP"' EXIT INT TERM; else echo "keeping $TMP"; fi

failed=0
total_pass=0
tests=0
for test in "$HERE"/test-*.scm; do
  name=$(basename "$test" .scm)
  tests=$((tests + 1))
  run="$TMP/$name"
  mkdir -p "$run/home/plugins" "$run/home/system/tmp" "$run/work/my docs" \
           "$run/userhome" "$run/sentinels"
  ln -s "$REPO/texmacs-plugin/mls" "$run/home/plugins/mls"
  cp -R "$HERE/fixtures" "$run/fixtures"
  cp "$HERE/fixtures/canonical.tm" "$run/work/canonical.tm"
  cp "$HERE/fixtures/noncanonical.tm" "$run/work/noncanonical.tm"
  cp "$HERE/fixtures/canonical.tm" "$run/work/my docs/it's here.tm"
  real=""
  if [ -n "$REAL_OK" ]; then real="$REAL_BIN"; fi

  (
    # HOME and the working directory are per run: ~/.TeXmacs and ~/.my-texmacs are never
    # read or written, and relative paths used by injection probes land in sentinels/.
    cd "$run/sentinels" || exit 1
    export HOME="$run/userhome"
    export TEXMACS_HOME_PATH="$run/home"
    export MLS_TEST_DIR="$HERE" MLS_TEST_WORK="$run/work" MLS_TEST_FIXTURES="$run/fixtures"
    export MLS_STUB_LOG="$run/stub.log" MLS_STUB_VIEW="$run/fixtures/views/replay.json"
    export MLS_BIN="$HERE/stub/mls" MLS_REAL_BIN="$real"
    unset MLS_LIVE MLS_STUB_FAIL MLS_STUB_SEQ MLS_STUB_WRITE_IDS_FROM
    perl -e 'alarm shift; exec @ARGV' "$TIMEOUT" "$TEXMACS_BIN" -H -s \
      -x "(load \"$test\")" -q < /dev/null > "$run/out.txt" 2> "$run/err.txt"
  )
  status=$?

  grep -v 'approximating font' "$run/out.txt" | sed "s/^/[$name] /"
  problems=""
  if [ "$status" -ne 0 ]; then problems="$problems exit-status=$status(timeout is 142)"; fi
  if grep -q '^FAIL' "$run/out.txt"; then problems="$problems FAIL-lines"; fi
  if grep -q '^ERROR' "$run/out.txt"; then problems="$problems ERROR-lines"; fi
  if ! grep -Eq '^SUMMARY pass=[0-9]+ fail=0$' "$run/out.txt"; then problems="$problems no-clean-SUMMARY"; fi
  if [ -n "$problems" ]; then
    echo "FAILED $name:$problems"
    failed=$((failed + 1))
  else
    passed=$(grep -c '^PASS' "$run/out.txt" || true)
    total_pass=$((total_pass + passed))
    echo "ok $name: $passed PASS"
  fi
done

echo "texmacs tests: $total_pass PASS across $tests files, $failed file(s) failed"
[ -n "$REAL_OK" ] || echo "note: real-engine test skipped: review-status not in $REAL_BIN"
[ "$failed" -eq 0 ]
