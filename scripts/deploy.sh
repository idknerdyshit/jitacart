#!/usr/bin/env bash
# Pull + up the prod stack, poll readiness, roll back if it doesn't
# come up. Intended for the standard release flow:
#
#     git pull --ff-only      # fetches CI's "Release: pin images to vX.Y.Z" commit
#     scripts/deploy.sh
#
# The digest rewrite is no longer an operator step — CI's pin-digests
# job lands it on main immediately after each `vX.Y.Z` tag is built.
# See DEPLOY.md § Manual digest pinning for the escape-hatch flow.
#
# Recovery: this script takes a `git rev-parse HEAD` snapshot before
# touching anything; on healthcheck failure it `git checkout`s the
# previous docker-compose.yml back and re-pulls. The snapshot is
# stored in `.deploy-prev-*` files (gitignored) so an operator who
# interrupts the script can still see what state to revert to.
#
# `/readyz` (not `/healthz`) is the source of truth: it returns 503
# when the DB or connection pool is unhappy — which is exactly the
# kind of failure the rollback path needs to catch. `/healthz` is
# liveness only and would happily 200 with a busted DB.

set -euo pipefail

cd "$(dirname "$0")/.."

log() { printf '[deploy] %s\n' "$*"; }
die() { printf '[deploy] ERROR: %s\n' "$*" >&2; exit 1; }

# 1. Guard rails.
command -v docker >/dev/null || die "docker not on PATH"
command -v git >/dev/null    || die "git not on PATH"

if [[ -n "$(git status --porcelain)" ]]; then
    die "working tree not clean; commit or stash before deploying"
fi

PREV_SHA="$(git rev-parse HEAD)"
log "current HEAD: $PREV_SHA"

# 2. Refuse to deploy if the branch is behind its upstream — that
#    would silently activate code the operator did not review. The
#    documented prerequisite is `git pull --ff-only` BEFORE invoking
#    this script; enforce it here rather than re-pulling under the
#    operator's feet.
UPSTREAM_SHA="$(git rev-parse '@{u}' 2>/dev/null || true)"
if [[ -z "$UPSTREAM_SHA" ]]; then
    die "current branch has no upstream; set one or check out a tracking branch"
fi
if [[ "$UPSTREAM_SHA" != "$PREV_SHA" ]]; then
    die "branch is not up to date with upstream ($UPSTREAM_SHA); run 'git pull --ff-only' first"
fi

# 3. Snapshot. Captures both the SHA (so we know exactly what to
#    `git checkout` back to) and the image set we're moving away from
#    (so the post-success summary can diff old → new).
echo "$PREV_SHA" > .deploy-prev-sha
docker compose config --images > .deploy-prev-images

# 4. Refuse to deploy unpinned or placeholder digests. The compose
#    file on `main` is normally pinned by CI's pin-digests job after
#    each release; if it isn't, `docker compose pull` would happily
#    resolve `:latest` to whatever is on GHCR right now — defeating
#    the audit-trail-in-git model.
if grep -qE '^\s*image:.*jitacart-(backend|frontend|backup):[^@[:space:]]+$' docker-compose.yml; then
    die "docker-compose.yml has unpinned jitacart images; CI's pin-digests job has not yet committed (or you've checked out a pre-release commit). Run 'git pull --ff-only' or scripts/bump-image-digests.sh."
fi
if grep -qE '@sha256:0{64}' docker-compose.yml; then
    die "docker-compose.yml still has placeholder digests; run scripts/bump-image-digests.sh first"
fi

# 5. Pull images (digest-pinned in docker-compose.yml; `pull` is a
#    no-op when the local store already has the digests).
log "docker compose pull"
docker compose pull

# 6. Bring up. compose's existing depends_on: service_healthy edges
#    handle ordering; we don't have to re-implement startup sequencing.
log "docker compose up -d"
docker compose up -d

# 7. Poll readiness. /readyz exec'd from inside the api container, so
#    we don't depend on Caddy / DNS / cert health to know the app is
#    serving. Worker /healthz on its in-container loopback.
poll_ready() {
    local deadline=$(( $(date +%s) + 90 ))
    local api_ok=0 worker_ok=0
    while (( $(date +%s) < deadline )); do
        if (( !api_ok )) \
            && docker compose exec -T api \
                curl -fsS --max-time 5 http://localhost:8080/readyz \
                >/dev/null 2>&1; then
            api_ok=1
            log "api /readyz green"
        fi
        if (( !worker_ok )) \
            && docker compose exec -T worker \
                curl -fsS --max-time 5 http://127.0.0.1:9091/healthz \
                >/dev/null 2>&1; then
            worker_ok=1
            log "worker /healthz green"
        fi
        if (( api_ok && worker_ok )); then
            return 0
        fi
        sleep 3
    done
    return 1
}

if poll_ready; then
    log "deploy verified"
    # Summary diff. Compose already lists images post-pull; pair them
    # up so operators see exactly what landed.
    log "image changes:"
    diff -u .deploy-prev-images <(docker compose config --images) || true
    rm -f .deploy-prev-sha .deploy-prev-images
    exit 0
fi

# 8. Rollback. Revert just docker-compose.yml (leave the rest of the
#    tree alone — it may contain doc-only commits the operator
#    explicitly wants on disk), pull the previous digests, bring the
#    stack back up. One retry — if even the previous good state
#    fails, hand off to the operator rather than thrashing.
log "FAILED: readiness did not converge in 90s; rolling back to $PREV_SHA"
git checkout "$PREV_SHA" -- docker-compose.yml \
    || die "could not restore previous docker-compose.yml — manual recovery"

log "docker compose pull (rollback)"
docker compose pull
log "docker compose up -d (rollback)"
docker compose up -d

if poll_ready; then
    die "deploy failed; rollback succeeded — previous digests are serving. Investigate the new digests before retrying."
fi

die "deploy failed AND rollback failed; operator intervention required. Snapshot in .deploy-prev-{sha,images}."
