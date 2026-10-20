#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
MLS="$ROOT/target/debug/mls"

if [ ! -x "$MLS" ]; then
  printf '%s\n' "MLS is not built at $MLS; run: cargo build -p mls-cli" >&2
  exit 127
fi

# Applications opened from Finder do not read the interactive shell profile.
# Import the already-configured key only into this launcher and its MLS child.
# Suppress all profile output so it can never corrupt the TeXmacs protocol.
if [ -z "${OPENROUTER_API_KEY:-}" ] && [ -r "$HOME/.zshrc" ]; then
  OPENROUTER_API_KEY=$(
    /bin/zsh -c '. "$HOME/.zshrc" >/dev/null 2>&1; printf "%s" "${OPENROUTER_API_KEY:-}"'
  )
  export OPENROUTER_API_KEY
fi

exec "$MLS" "$@"
