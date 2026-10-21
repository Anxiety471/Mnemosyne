#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
have() { command -v "$1" >/dev/null 2>&1; }
archive="${1:-}"
if [[ -z "$archive" || ! -f "$archive" || ! -f "$archive.sha256" ]]; then
  echo "Usage: bash ops/restore.sh ./backups/drawer-TIMESTAMP.tar.gz (and .sha256)" >&2
  exit 2
fi
# realpath is missing on macOS by default; resolve without it when needed.
if have realpath; then
  archive="$(realpath "$archive")"
  backupdir="$(realpath backups)"
else
  archive="$(cd "$(dirname "$archive")" && pwd)/$(basename "$archive")"
  backupdir="$(pwd)/backups"
fi
if [[ "$(dirname "$archive")" != "$backupdir" ]]; then
  echo "Copy the backup and checksum into ./backups before restoring." >&2
  exit 2
fi
exec 9>backups/.maintenance.lock
flock -n 9 || { echo 'Another backup or restore is running.' >&2; exit 1; }
# Verify this exact archive, regardless of which filename its sidecar lists.
expected="$(awk 'NR == 1 {print $1}' "$archive.sha256")"
[[ "$expected" =~ ^[[:xdigit:]]{64}$ ]] || { echo 'Invalid checksum.' >&2; exit 1; }
actual="$(sha256sum "$archive")"
[[ "${actual%% *}" == "${expected,,}" ]] || { echo 'Checksum mismatch.' >&2; exit 1; }
echo "WARNING: this replaces the current Drawer store. Type RESTORE to proceed:"
read -r answer
# Trim whitespace and match case-insensitively, so "restore" and "RESTORE " both work.
answer="${answer//[[:space:]]/}"
if [[ "${answer^^}" != "RESTORE" ]]; then
  echo 'Aborted; nothing was changed.' >&2
  exit 1
fi

# Keep the writer stopped from the safety backup through the entire restore.
docker compose stop drawer
# Second resolution keeps the stamp portable; the random suffix avoids collisions.
stamp="$(date -u +%Y%m%dT%H%M%S)-$(openssl rand -hex 4)"
safety="drawer-before-restore-$stamp.tar.gz"
# Same exclusions as backup.sh, so a safety backup matches what backup.sh archives.
# Archive first, and only then write its checksum.
if ! docker compose run --rm --no-deps --user root --entrypoint sh drawer -c \
  'tar --exclude="./.maintenance.lock" --exclude="./users/*/.maintenance.lock" --exclude="./pre-restore-*" --exclude="./.restore-stage-*" --exclude="./users/*/transfers/*" -czf "/backups/$1" -C /data .' sh "$safety"; then
  echo "Safety backup failed; the current store is untouched and Drawer stays stopped." >&2
  exit 1
fi
# On failure leave the service stopped for inspection. Never start a partially moved store.
trap 'echo "Restore failed; Drawer remains stopped. Inspect the safety backup and pre-restore directory before starting it." >&2' EXIT
( cd backups && sha256sum "$safety" > "$safety.sha256" )
snapshot="pre-restore-$stamp"
stage=".restore-stage-$stamp"
# Pass filenames as arguments; never interpolate an archive name into shell code.
docker compose run --rm --no-deps --user root --entrypoint sh drawer -c '
  set -eu
  archive=$1; snapshot=$2; stage=$3
  mkdir "/data/$stage"
  tar -xzf "/backups/$archive" -C "/data/$stage"
  # Old snapshots may contain a process lease; keep the live lock inode.
  if [ -f "/data/$stage/.maintenance.lock" ]; then unlink "/data/$stage/.maintenance.lock"; fi
  test -f "/data/$stage/drawer.db"
  test -d "/data/$stage/blobs"
  chown -R drawer:drawer "/data/$stage"
  mkdir "/data/$snapshot"
  for entry in drawer.db drawer.db-wal drawer.db-shm blobs transfers users; do
    if [ -e "/data/$entry" ]; then mv "/data/$entry" "/data/$snapshot/"; fi
  done
  for entry in drawer.db drawer.db-wal drawer.db-shm blobs transfers users; do
    if [ -e "/data/$stage/$entry" ]; then mv "/data/$stage/$entry" /data/; fi
  done
  rmdir "/data/$stage"
' sh "$(basename "$archive")" "$snapshot" "$stage"
docker compose up -d drawer >/dev/null
trap - EXIT
echo "Restore extracted. Previous store retained as /data/$snapshot."
echo "Safety backup: backups/$safety"
echo "Verify with: docker compose exec drawer drawer doctor"
