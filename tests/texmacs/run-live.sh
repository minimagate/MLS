#!/bin/sh
# Offscreen Qt integration test for the persistent TeXmacs connection. Every run owns fresh
# HOME and TEXMACS_HOME_PATH directories, and perl bounds the only TeXmacs process started.
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
TEXMACS_BIN=${TEXMACS_BIN:-/Applications/TeXmacs.app/Contents/MacOS/TeXmacs}
TIMEOUT=${MLS_TEST_TIMEOUT:-45}
if [ ! -x "$TEXMACS_BIN" ]; then
  echo "SKIP: TeXmacs not found at $TEXMACS_BIN"
  exit 0
fi

TMPBASE=${TMPDIR:-/tmp}
TMP=$(mktemp -d "${TMPBASE%/}/mls-texmacs-live.XXXXXX") || exit 1
if [ -z "${MLS_TEST_KEEP:-}" ]; then trap 'rm -rf "$TMP"' EXIT INT TERM; else echo "keeping $TMP"; fi
mkdir -p "$TMP/home/plugins" "$TMP/home/system/tmp" "$TMP/userhome" "$TMP/work" "$TMP/sentinels"
ln -s "$REPO/texmacs-plugin/mls" "$TMP/home/plugins/mls"
cp "$HERE/fixtures/live-uncarried.tm" "$TMP/work/canonical.tm"
cp "$HERE/fixtures/live-uncarried.tm" "$TMP/work/other.tm"
: > "$TMP/live-order.log"

(
  cd "$TMP/sentinels" || exit 1
  export HOME="$TMP/userhome" TEXMACS_HOME_PATH="$TMP/home"
  export MLS_TEST_DIR="$HERE" MLS_TEST_WORK="$TMP/work" MLS_TEST_FIXTURES="$HERE/fixtures"
  export MLS_LIVE_STUB_LOG="$TMP/live-daemon.log" MLS_LIVE_STUB_VIEW="$HERE/fixtures/views/replay.json"
  export MLS_LIVE_STUB_ORDER_LOG="$TMP/live-order.log"
  export MLS_STUB_LOG="$TMP/live-engine.log" MLS_STUB_VIEW="$HERE/fixtures/views/replay.json"
  export MLS_STUB_WRITE_IDS_FROM="$HERE/fixtures/live-identified.tm"
  export MLS_BIN="$HERE/stub/mls" MLS_LIVE_BIN="$HERE/stub/live-daemon"
  unset MLS_LIVE MLS_TEST_KEEP
  QT_QPA_PLATFORM=offscreen perl -e 'alarm shift; exec @ARGV' "$TIMEOUT" \
    "$TEXMACS_BIN" -s -x "(load \"$HERE/live-integration.scm\")" < /dev/null \
    > "$TMP/out.txt" 2> "$TMP/err.txt"
)
status=$?
grep -v 'approximating font tuple' "$TMP/out.txt"
if [ "$status" -ne 0 ] || grep -q '^FAIL\|^ERROR' "$TMP/out.txt" || \
   ! grep -Eq '^SUMMARY pass=[0-9]+ fail=0$' "$TMP/out.txt"; then
  echo "FAILED live integration: exit-status=$status (timeout is 142)"
  cat "$TMP/err.txt"
  exit 1
fi
