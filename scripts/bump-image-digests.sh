#!/usr/bin/env bash
# Rewrite the four `image:` lines in docker-compose.yml to point at a
# specific GHCR tag + its multi-arch index digest.
#
# Usage:
#     scripts/bump-image-digests.sh 0.3.0
#     scripts/bump-image-digests.sh v0.3.0        # leading `v` is stripped
#     scripts/bump-image-digests.sh latest        # tracks mutable :latest
#
# CI's pin-digests job runs this automatically on every `vX.Y.Z` tag
# and commits the result to main, so the standard release flow does
# NOT need an operator to invoke this. Kept as a manual escape hatch
# for: retargeting a fork's images, pinning to a tag that bypassed CI,
# hotfix workflows, or recovering when the CI commit failed to push.
#
# The script resolves each tag's multi-arch index digest via
# `docker buildx imagetools inspect`, edits docker-compose.yml in
# place, prints the diff, and suggests a commit message.
#
# Why digests: an `image: ghcr.io/.../X:v0.3.0` reference can be
# clobbered upstream after release. An `image: ghcr.io/.../X:v0.3.0@sha256:...`
# reference is content-addressed — `docker compose pull` produces
# bit-for-bit identical containers no matter when it runs, and
# `git revert` rolls back to the previous trusted state.

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <vX.Y.Z|X.Y.Z|latest>" >&2
    exit 1
fi
# Normalize a leading `v`: git tags are `vX.Y.Z`, but docker/metadata-action
# tags the images `X.Y.Z` (semver pattern drops the `v`). CI's pin-digests
# job passes `github.ref_name` (the `v`-prefixed git tag) straight through,
# so strip it here rather than making every caller remember to. `latest`
# and other non-version tags pass through untouched.
VERSION="${1#v}"

cd "$(dirname "$0")/.."

# Owner defaults to the in-tree default; override via env to lock to
# a fork.
OWNER="${JC_IMAGE_OWNER:-idknerdyshit}"

COMPOSE=docker-compose.yml
[[ -f "$COMPOSE" ]] || { echo "no $COMPOSE in $(pwd)" >&2; exit 1; }

command -v docker >/dev/null || { echo "docker not on PATH" >&2; exit 1; }

# `docker buildx imagetools inspect <tag>` prints the manifest-index
# digest on its own `Digest:` line:
#
#     Name:      ghcr.io/.../X:tag
#     MediaType: application/vnd.oci.image.index.v1+json
#     Digest:    sha256:DIGEST
#
# That index digest is what we want — it resolves to the right per-arch
# image at pull time. (The `@sha256:...` refs under `Manifests:` are the
# *per-arch* digests, not the index — don't grab those.)
#
# Capture the inspect output into a variable BEFORE awk-ing it. Piping
# `docker buildx imagetools inspect` straight into `awk '... exit'`
# SIGPIPEs docker the moment awk exits on the first match — docker is
# still streaming the `Manifests:` section — and under `set -e -o
# pipefail` that aborts the whole script before the caller's
# empty-digest check can run. `printf | awk` is safe: printf dumps the
# (tiny) output into the pipe buffer and returns before awk reads it.
# The release job in ci.yml works around the identical trap the same
# way. A failed inspect (missing tag, network) yields empty output and
# exit 0, so the caller's `[[ -z ]]` branch reports it cleanly.
resolve_digest() {
    local image="$1"
    local out
    out="$(docker buildx imagetools inspect "$image" 2>/dev/null)" || return 0
    printf '%s\n' "$out" | awk '/^Digest:/ { print $2; exit }'
}

rewrite_one() {
    local svc="$1"    # api / worker / frontend / backup
    local img="$2"    # backend / frontend / backup
    local ref="ghcr.io/${OWNER}/jitacart-${img}:${VERSION}"
    local digest
    digest="$(resolve_digest "$ref")"
    if [[ -z "$digest" ]]; then
        echo "could not resolve digest for $ref" >&2
        echo "(does the tag exist? are you logged in to ghcr.io?)" >&2
        return 1
    fi
    echo "  $svc: $ref @ $digest"

    # Match `    image: ghcr.io/<owner-or-interp>/jitacart-<img>:...`
    # at the start of the line. The owner segment may be a literal or
    # the `${JC_IMAGE_OWNER:-idknerdyshit}` interpolation — keep
    # whichever the file already has. Anchor on `jitacart-<img>:` so
    # we don't touch postgres / caddy / unrelated services.
    python3 - "$COMPOSE" "$img" "$VERSION" "$digest" <<'PY'
import re, sys
path, img, ver, digest = sys.argv[1:]
with open(path) as f:
    src = f.read()
pat = re.compile(
    r'^(\s*image:\s+ghcr\.io/[^/]+/jitacart-' + re.escape(img) + r'):[^@\s]+(?:@sha256:[0-9a-f]+)?',
    re.MULTILINE,
)
# Use a function replacement, not a template string: `ver` is attacker-ish
# input (a git tag name) and a template would interpret backslashes / \g<n>
# group references inside it.
new, n = pat.subn(lambda m: f'{m.group(1)}:{ver}@{digest}', src)
if n == 0:
    sys.exit(f"no image: line matched for jitacart-{img}")
with open(path, 'w') as f:
    f.write(new)
PY
}

echo "resolving digests for ${VERSION} (owner=${OWNER}):"
# NOTE: `api` and `worker` both run the jitacart-backend image, and the
# rewrite is keyed on `jitacart-<img>:` — so each of these two calls
# (re)writes *both* backend `image:` lines. That's idempotent today
# because they resolve to the same digest. If a third service ever runs
# jitacart-backend at a *different* version, this loop can no longer
# express that; switch rewrite_one to target a specific service block.
rewrite_one api      backend
rewrite_one worker   backend
rewrite_one frontend frontend
rewrite_one backup   backup

echo
echo "diff:"
git --no-pager diff -- "$COMPOSE" || true

cat <<EOF

Suggested commit (manual flow only — CI does this for you on tag push):

  git add $COMPOSE
  git commit -m "Release: pin images to ${VERSION}"

Then deploy:

  scripts/deploy.sh
EOF
