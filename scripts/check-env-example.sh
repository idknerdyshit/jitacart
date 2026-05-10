#!/usr/bin/env bash
# Lints that every env var the stack relies on has a placeholder in
# .env.example. Failure mode this catches: someone adds `${NEW_VAR}`
# to compose or a new `std::env::var("NEW_VAR")` to a binary, ships
# it, and the next operator's deploy silently breaks because their
# .env (copied from .env.example) is missing the key.
set -euo pipefail

cd "$(dirname "$0")/.."

# Vars composed inside containers / synthesized at runtime — never
# expected to live in .env.example even though they're set by compose.
EXCLUDE=(
    DATABASE_URL          # composed from POSTGRES_* in compose env: stanza
    POSTGRES_HOST         # set by service environment, not .env
    POSTGRES_USER         # likewise
    POSTGRES_DB           # likewise
    SERVER__BIND          # set by api environment
    WORKER__HEALTHZ_BIND  # set by worker environment
    RCLONE_CONFIG         # set by backup environment
    NODE_ENV PORT HOST    # set by frontend Dockerfile
    PATH HOME TZ          # OS-provided
)

is_excluded() {
    local needle=$1 candidate
    for candidate in "${EXCLUDE[@]}"; do
        [[ "$candidate" == "$needle" ]] && return 0
    done
    return 1
}

# Collect referenced vars: compose ${VAR}, ${VAR:-...}, ${VAR:?...} +
# Rust std::env::var("VAR") calls.
referenced=$({
    grep -rohE '\$\{[A-Z][A-Z0-9_]+' docker-compose*.yml 2>/dev/null \
        | sed 's/^\${//' || :
    grep -rhE 'std::env::var\("[A-Z][A-Z0-9_]+"\)' backend/crates 2>/dev/null \
        | sed -E 's/.*std::env::var\("([A-Z0-9_]+)"\).*/\1/' || :
} | sort -u)

declared=$(grep -E '^[A-Z][A-Z0-9_]+=' .env.example | sed -E 's/=.*//' | sort -u || :)

missing=()
while IFS= read -r v; do
    [[ -z "$v" ]] && continue
    is_excluded "$v" && continue
    if ! grep -qx "$v" <<< "$declared"; then
        missing+=("$v")
    fi
done <<< "$referenced"

if (( ${#missing[@]} > 0 )); then
    echo "check-env-example: vars referenced by compose/code but missing from .env.example:" >&2
    printf '  - %s\n' "${missing[@]}" >&2
    echo "Add them (with placeholder values + a one-line comment) or, if they're" >&2
    echo "synthesized at runtime, allow-list in scripts/check-env-example.sh." >&2
    exit 1
fi

ref_count=$(printf '%s\n' "$referenced" | grep -c '^[A-Z]' || :)
dec_count=$(printf '%s\n' "$declared" | grep -c '^[A-Z]' || :)
echo "check-env-example: ok (${ref_count:-0} referenced, ${dec_count:-0} declared)"
