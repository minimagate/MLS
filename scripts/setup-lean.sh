#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TOOLCHAINS="$ROOT/.toolchains"
LEAN="$TOOLCHAINS/lean-4.19.0-darwin_aarch64"
ARCHIVE="$TOOLCHAINS/lean-4.19.0-darwin_aarch64.zip"
REPL="$TOOLCHAINS/repl"
SHADOW="$ROOT/.mls"
LEAN_URL="https://github.com/leanprover/lean4/releases/download/v4.19.0/lean-4.19.0-darwin_aarch64.zip"
LEAN_COMMIT="6caaee842e9495688c1567e78c0e68dbb96942aa"
LEAN_COMMIT_PREFIX="6caaee842e94"
# Recorded from this official release download. GitHub publishes no independent
# digest for the asset, so this is a local repeatability check only.
ARCHIVE_SHA256="09f5534815844869414274628b8dfa86be0a20df2d38c2a615e9afc0b06536c0"
REPL_URL="https://github.com/leanprover-community/repl.git"
REPL_COMMIT="3b27c8543a48444ffab4077f47526d91849cca3d"
MATHLIB_COMMIT="c44e0c8ee63ca166450922a373c7409c5d26b00b"

mkdir -p "$TOOLCHAINS"
if [ ! -x "$LEAN/bin/lean" ]; then
  if [ ! -f "$ARCHIVE" ]; then
    curl -fL "$LEAN_URL" -o "$ARCHIVE"
  fi
  ACTUAL_SHA256=$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')
  if [ "$ACTUAL_SHA256" != "$ARCHIVE_SHA256" ]; then
    printf '%s\n' "Lean archive hash mismatch: $ARCHIVE" >&2
    exit 1
  fi
  unzip -q "$ARCHIVE" -d "$TOOLCHAINS"
fi

export PATH="$LEAN/bin:$PATH"
export MATHLIB_CACHE_DIR="$TOOLCHAINS/mathlib-cache"
lean --version | grep -q "commit $LEAN_COMMIT_PREFIX"

if [ ! -d "$REPL/.git" ]; then
  git clone "$REPL_URL" "$REPL"
fi
git -C "$REPL" fetch --quiet origin "$REPL_COMMIT"
git -C "$REPL" checkout --quiet --detach "$REPL_COMMIT"
test "$(git -C "$REPL" rev-parse HEAD)" = "$REPL_COMMIT"
test "$(cat "$REPL/lean-toolchain")" = "leanprover/lean4:v4.19.0"
(cd "$REPL" && lake build)

mkdir -p "$SHADOW/Shadow"
cp "$ROOT/lean/MlsExtract.lean" "$SHADOW/MlsExtract.lean"
cp "$ROOT/lean/lakefile.lean" "$SHADOW/lakefile.lean"
cp "$ROOT/lean/lean-toolchain" "$SHADOW/lean-toolchain"

if [ ! -f "$SHADOW/lake-manifest.json" ]; then
  (cd "$SHADOW" && MATHLIB_NO_CACHE_ON_UPDATE=1 lake update)
fi
test "$(git -C "$SHADOW/.lake/packages/mathlib" rev-parse HEAD)" = "$MATHLIB_COMMIT"
# Running Cache/Main through Lean avoids a macOS loader failure observed in the
# natively linked cache wrapper from this pinned toolchain.
(cd "$SHADOW" && lake env lean --run .lake/packages/mathlib/Cache/Main.lean get)
(cd "$SHADOW" && lake build MlsExtract)

printf '%s\n' "Lean runtime ready: $LEAN"
printf '%s\n' "REPL command: $ROOT/scripts/run-lean-repl.sh"
