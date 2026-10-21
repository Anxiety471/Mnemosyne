#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p backups
# Serialize maintenance across shell invocations, including restore.
exec 9>backups/.maintenance.lock
flock -n 9 || { echo 'Another backup or restore is running.' >&2; exit 1; }
# Second resolution keeps the name portable (BSD/macOS date has no %N); the random
# suffix keeps two backups started in the same second from colliding.
name="drawer-$(date -u +%Y%m%dT%H%M%S)-$(openssl rand -hex 4).tar.gz"
# Stopping the writer guarantees SQLite WAL and content blobs agree.
docker compose stop drawer
restart() { docker compose up -d drawer >/dev/null; }
trap restart EXIT
# Archive first, and only then write its checksum: a failed tar must never leave a
# checksum behind for a truncated archive.
if ! docker compose run --rm --no-deps --user root --entrypoint sh drawer -c \
  'tar --exclude="./.maintenance.lock" --exclude="./users/*/.maintenance.lock" --exclude="./pre-restore-*" --exclude="./.restore-stage-*" --exclude="./users/*/transfers/*" -czf "/backups/$1" -C /data .' sh "$name"; then
  echo "Backup failed; no archive or checksum was written." >&2
  exit 1
fi
( cd backups && sha256sum "$name" > "$name.sha256" )
printf 'Backup: %s/backups/%s\n' "$(pwd)" "$name"
printf 'Move the archive and its .sha256 file to a DIFFERENT device or cloud account.\n'
printf 'Timestamps are UTC.\n'
