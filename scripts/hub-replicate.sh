#!/bin/sh
# Hourly consistent copy of the hub's indexes to the external drive.
#
#   sh hub-replicate.sh                  one copy, now
#   DIWA_DIR=... HUB_BACKUPS=... sh hub-replicate.sh
#
# Same shape as everyday-vet-admin/backup.sh, for the same reasons:
#  - `sqlite3 .backup`, never `cp`: the daemon writes while this runs and a
#    copy without its -wal quietly loses the latest insights.
#  - refuses when the drive is not mounted, so a "backup" cannot land on the
#    boot disk it is meant to protect (an unmounted /Volumes path can exist
#    as a plain directory, so we ask the mount table, not the filesystem).
#  - launchd cannot write to the external drive here, sshd can: the agent
#    runs this through `ssh localhost` (see com.dorky-robot.diwa-hub-replicate).
# The bare mirrors are not copied; they are re-cloned from GitHub.
set -eu
# launchd/ssh shells start with a bare PATH; `mount` lives in /sbin.
PATH=/sbin:/usr/sbin:/opt/homebrew/bin:/usr/local/bin:$PATH

DIWA_DIR=${DIWA_DIR:-$HOME/.diwa-hub}
VOLUME=${HUB_VOLUME:-/Volumes/Orange4TB}
DEST=${HUB_BACKUPS:-$VOLUME/diwa-hub-snapshots}
KEEP_DAYS=${HUB_BACKUPS_KEEP:-30}
SQLITE=${SQLITE3:-sqlite3}

mount | grep -q " on $VOLUME " || { echo "$VOLUME is not mounted, refusing to copy" >&2; exit 1; }
[ -d "$DIWA_DIR" ] || { echo "no hub at $DIWA_DIR" >&2; exit 1; }

ts=$(date -u +%Y-%m-%dT%H%M)
out="$DEST/$ts"
mkdir -p "$out"
n=0
for db in "$DIWA_DIR"/*/index.db; do
  [ -f "$db" ] || continue
  slug=$(basename "$(dirname "$db")")
  mkdir -p "$out/$slug"
  "$SQLITE" "$db" ".backup '$out/$slug/index.db'"
  check=$("$SQLITE" "$out/$slug/index.db" "pragma integrity_check;")
  [ "$check" = "ok" ] || { echo "copy of $slug is not sound: $check" >&2; rm -rf "$out"; exit 1; }
  rm -f "$out/$slug/index.db-wal" "$out/$slug/index.db-shm"
  n=$((n + 1))
done
[ -f "$DIWA_DIR/repos.json" ] && cp "$DIWA_DIR/repos.json" "$out/repos.json"

find "$DEST" -mindepth 1 -maxdepth 1 -type d -mtime +"$KEEP_DAYS" -exec rm -rf {} +
kept=$(find "$DEST" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')
echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $out $n indexes $(du -sh "$out" | cut -f1) · $kept copies kept"
