#!/bin/bash
# JitaCart DB restore.
#
# Usage (from host):
#   docker compose run --rm \
#       -v ~/.age/jitacart.key:/run/age.key:ro \
#       -e BACKUP_AGE_IDENTITY=/run/age.key \
#       -e BACKUP_RESTORE_CONFIRM=jitacart \
#       backup restore 2026-05-10
#
#   ...or `restore latest` to grab the newest dump in the remote.
#
# Required env:
#   POSTGRES_PASSWORD       postgres password (server)
#   BACKUP_RCLONE_REMOTE    rclone remote + path, e.g. "b2:jitacart-backups"
#   RCLONE_CONFIG           path to rclone.conf
#   BACKUP_AGE_IDENTITY     path to age private key file (mounted ad-hoc)
#   BACKUP_RESTORE_CONFIRM  must equal the target DB name. Guard against
#                           accidental prod overwrites — no default, no
#                           way to set this once and forget.
#
# Optional env:
#   POSTGRES_USER             (default: jitacart)
#   POSTGRES_HOST             (default: postgres)
#   POSTGRES_DB               (default: jitacart) — restore target unless
#                             BACKUP_RESTORE_TARGET_DB is set
#   BACKUP_RESTORE_TARGET_DB  restore into a different DB (e.g. "jitacart_restore")
#                             so you can verify before swapping
#
# What it does:
#   1. Resolves <date|latest> to a remote object
#   2. Streams rclone cat → age -d → pg_restore --clean --if-exists
#   3. Logs JSON to stdout

set -euo pipefail

log() {
    local level="$1"; shift
    printf '{"level":"%s","time":"%s","service":"restore","msg":"%s"}\n' \
        "$level" "$(date -u +%FT%TZ)" "$*"
}

die() { log error "$*"; exit 1; }

require() {
    local var="$1"
    [[ -n "${!var:-}" ]] || die "missing required env var: $var"
}

if [[ $# -lt 1 ]]; then
    die "usage: restore <YYYY-MM-DD|latest>"
fi
WHICH="$1"

require POSTGRES_PASSWORD
require BACKUP_RCLONE_REMOTE
require RCLONE_CONFIG
require BACKUP_AGE_IDENTITY
require BACKUP_RESTORE_CONFIRM

[[ -r "$BACKUP_AGE_IDENTITY" ]] || die "age identity not readable: $BACKUP_AGE_IDENTITY"

PGUSER="${POSTGRES_USER:-jitacart}"
PGHOST="${POSTGRES_HOST:-postgres}"
PGDB="${POSTGRES_DB:-jitacart}"
# Default the restore target to the side DB, NOT the live DB. Otherwise
# an operator who sets BACKUP_RESTORE_CONFIRM=jitacart but forgets
# BACKUP_RESTORE_TARGET_DB ends up restoring straight over prod (the
# confirmation check below would still pass — both defaults would
# resolve to "jitacart"). To overwrite prod, set
# BACKUP_RESTORE_TARGET_DB=jitacart explicitly.
TARGET_DB="${BACKUP_RESTORE_TARGET_DB:-jitacart_restore}"

# Confirm token must literally name the DB we're about to overwrite. This
# is the only thing standing between an operator and a destroyed prod —
# don't soften it.
if [[ "$BACKUP_RESTORE_CONFIRM" != "$TARGET_DB" ]]; then
    die "BACKUP_RESTORE_CONFIRM=$BACKUP_RESTORE_CONFIRM does not match target DB '$TARGET_DB'; refusing"
fi

# Resolve which dump to pull.
if [[ "$WHICH" == "latest" ]]; then
    log info "resolving latest backup in $BACKUP_RCLONE_REMOTE"
    NAME="$(rclone --config "$RCLONE_CONFIG" lsf \
                --files-only \
                --include "jitacart-*.dump.age" \
                "$BACKUP_RCLONE_REMOTE" \
            | sort -r \
            | head -n 1)"
    [[ -n "$NAME" ]] || die "no backups found at $BACKUP_RCLONE_REMOTE"
else
    # Validate date format strictly; pg_dump names are predictable.
    [[ "$WHICH" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] \
        || die "expected YYYY-MM-DD or 'latest', got: $WHICH"
    NAME="jitacart-${WHICH}.dump.age"
fi

SOURCE="${BACKUP_RCLONE_REMOTE}/${NAME}"
log info "restoring ${SOURCE} -> ${PGHOST}/${TARGET_DB} (as ${PGUSER})"

# Stream restore. --clean --if-exists makes the restore idempotent against
# an existing schema; --no-owner / --no-acl match how pg_dump was taken.
# pg_restore expects a seekable input for custom format only when using
# parallel jobs (-j); single-stream is fine and matches the dump pipeline.
rclone --config "$RCLONE_CONFIG" cat "$SOURCE" \
  | age -d -i "$BACKUP_AGE_IDENTITY" \
  | PGPASSWORD="$POSTGRES_PASSWORD" pg_restore \
        -U "$PGUSER" \
        -h "$PGHOST" \
        -d "$TARGET_DB" \
        --clean --if-exists \
        --no-owner --no-acl \
        --exit-on-error

log info "restore complete: ${NAME} -> ${TARGET_DB}"
