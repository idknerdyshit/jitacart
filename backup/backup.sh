#!/bin/bash
# JitaCart nightly DB backup.
#
# Required env vars (set in compose .env):
#   POSTGRES_USER         (default: jitacart)
#   POSTGRES_PASSWORD     postgres password
#   POSTGRES_HOST         (default: postgres)
#   POSTGRES_DB           (default: jitacart)
#   BACKUP_AGE_RECIPIENT  age public key, e.g. "age1abc..."
#   BACKUP_RCLONE_REMOTE  rclone remote name + path, e.g. "b2:jitacart-backups"
#   RCLONE_CONFIG         path to rclone.conf (we mount it read-only)
#
# Optional:
#   BACKUP_RETAIN_DAILY   how many daily backups to keep (default 30)
#
# Behavior:
#   1. pg_dump (custom format, compressed) → age-encrypt → rclone rcat
#      to <remote>/jitacart-YYYY-MM-DD.dump.age
#   2. rclone delete --min-age <RETAIN> to prune old daily backups
#   3. Logs JSON to stdout for compose log aggregation.

set -euo pipefail

log() {
    local level="$1"; shift
    local msg="$*"
    # JSON line for log shippers; quote-safe via printf %s.
    printf '{"level":"%s","time":"%s","service":"backup","msg":"%s"}\n' \
        "$level" "$(date -u +%FT%TZ)" "$msg"
}

require() {
    local var="$1"
    if [[ -z "${!var:-}" ]]; then
        log warn "missing required env var: $var; skipping this run"
        exit 0
    fi
}

require POSTGRES_PASSWORD
require BACKUP_AGE_RECIPIENT
require BACKUP_RCLONE_REMOTE
require RCLONE_CONFIG

PGUSER="${POSTGRES_USER:-jitacart}"
PGHOST="${POSTGRES_HOST:-postgres}"
PGDB="${POSTGRES_DB:-jitacart}"
RETAIN="${BACKUP_RETAIN_DAILY:-30}"

DATE="$(date -u +%F)"
NAME="jitacart-${DATE}.dump.age"
TARGET="${BACKUP_RCLONE_REMOTE}/${NAME}"

log info "starting backup ${NAME} -> ${TARGET}"

# Pipeline: pg_dump → age encrypt → rclone rcat (streams, no temp files).
# `set -o pipefail` above means a failure anywhere short-circuits.
PGPASSWORD="$POSTGRES_PASSWORD" pg_dump \
    -U "$PGUSER" \
    -h "$PGHOST" \
    -d "$PGDB" \
    --format=custom \
    --compress=9 \
    --no-owner --no-acl \
    --quote-all-identifiers \
  | age -r "$BACKUP_AGE_RECIPIENT" \
  | rclone --config "$RCLONE_CONFIG" rcat "$TARGET"

log info "backup uploaded: ${NAME}"

# Prune. `--min-age <N>d` matches files modified more than N days ago.
log info "pruning backups older than ${RETAIN} days"
rclone --config "$RCLONE_CONFIG" delete \
    --min-age "${RETAIN}d" \
    --include "jitacart-*.dump.age" \
    "$BACKUP_RCLONE_REMOTE" 2>&1 \
  | while IFS= read -r line; do log info "rclone-delete: $line"; done || true

log info "backup run complete"
