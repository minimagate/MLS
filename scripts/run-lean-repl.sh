#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
LEAN="$ROOT/.toolchains/lean-4.19.0-darwin_aarch64"
REPL="$ROOT/.toolchains/repl"
SHADOW="$ROOT/.mls"
PROJECT=${1:-$SHADOW}

case "$PROJECT" in
  /*) ;;
  *) printf '%s\n' "MLS Lean project path must be absolute: $PROJECT" >&2; exit 2 ;;
esac
if [ ! -f "$PROJECT/lake-manifest.json" ]; then
  printf '%s\n' "MLS Lean project is not initialized (missing lake-manifest.json): $PROJECT" >&2
  exit 2
fi
if [ ! -f "$PROJECT/.lake/build/lib/lean/MlsExtract.olean" ]; then
  printf '%s\n' "MLS extractor is not compiled in project: $PROJECT" >&2
  exit 2
fi

export PATH="$LEAN/bin:$PATH"
export MATHLIB_CACHE_DIR="$ROOT/.toolchains/mathlib-cache"
cd "$PROJECT"

# The pinned native REPL binary is rejected by dyld on this host because its
# generated __DATA_CONST segment lacks SG_READ_ONLY. Running the same pinned
# REPL Main module through Lean preserves the protocol and persistent process.
exec lake env sh -c 'export LEAN_PATH="$1:$LEAN_PATH"; exec lean --run "$2"' \
  sh "$REPL/.lake/build/lib/lean" "$ROOT/lean/MlsReplMain.lean"
