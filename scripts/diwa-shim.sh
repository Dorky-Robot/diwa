#!/bin/sh
# Stand-in for `diwa` on machines that do not run the hub. Everything is
# answered by the single hub instance, so `diwa search`, `diwa ls`, `diwa stats`
# print exactly what the hub's binary prints (same output shape as the
# per-machine binary, so parsers such as overview's parseLs keep working).
#
#   DIWA_HUB_HOST   ssh host of the hub          (default: macmini)
#   DIWA_HUB_BIN    hub binary on that host      (default: ~/.diwa-hub/bin/diwa)
#   DIWA_HUB_DIR    hub data dir on that host    (default: ~/.diwa-hub)
#
# Read-only verbs only: anything that would write to the hub is refused here,
# so a stray `diwa index` on a laptop cannot spend the hub's quota.
set -eu
HOST=${DIWA_HUB_HOST:-macmini}
BIN=${DIWA_HUB_BIN:-.diwa-hub/bin/diwa}
DIR=${DIWA_HUB_DIR:-.diwa-hub}

case "${1:-}" in
  search|ls|stats|--help|-h|--version|-V|help|"") ;;
  *) echo "diwa shim: '$1' is not available here; the hub (on $HOST) does indexing." >&2; exit 2 ;;
esac

# Single-quote every argument for the remote shell.
q() { printf "'%s'" "$(printf %s "$1" | sed "s/'/'\\\\''/g")"; }
args=""
for a in "$@"; do args="$args $(q "$a")"; done

# Relative BIN/DIR resolve against the remote $HOME.
exec ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" \
  "DIWA_DIR=\"\$HOME/$DIR\" exec \"\$HOME/$BIN\"$args"
