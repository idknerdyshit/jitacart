#!/bin/bash
# Long-running cron loop. Sleeps until 03:00 UTC each day, runs the
# backup, repeats. Simpler than crond + handles container restarts
# cleanly because we always re-compute the next-fire time.

set -euo pipefail

log() {
    printf '{"level":"info","time":"%s","service":"backup","msg":"%s"}\n' \
        "$(date -u +%FT%TZ)" "$*"
}

# `BACKUP_HOUR_UTC=3` by default — overridable for tests / staggering.
HOUR="${BACKUP_HOUR_UTC:-3}"

# Optional: run once on startup. Useful in CI / first-deploy smoke
# tests; disabled by default so a freshly-restarted container doesn't
# create a duplicate same-day backup.
if [[ "${BACKUP_RUN_ON_START:-false}" == "true" ]]; then
    log "BACKUP_RUN_ON_START=true; running backup immediately"
    /usr/local/bin/backup.sh || log "initial backup failed (continuing)"
fi

while true; do
    NOW="$(date -u +%s)"
    # Next fire = today at $HOUR:00 UTC, or tomorrow if past.
    TARGET="$(date -u -d "today ${HOUR}:00:00" +%s 2>/dev/null \
        || date -u -j -f "%H:%M" "${HOUR}:00" +%s)"
    if (( TARGET <= NOW )); then
        # Past — bump 24h.
        TARGET=$(( TARGET + 86400 ))
    fi
    SLEEP=$(( TARGET - NOW ))
    log "next backup at $(date -u -d "@$TARGET" +%FT%TZ 2>/dev/null || date -u -r "$TARGET" +%FT%TZ); sleeping ${SLEEP}s"
    sleep "$SLEEP"
    /usr/local/bin/backup.sh || log "backup run failed (continuing)"
done
