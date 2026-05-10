#!/usr/bin/env bash
# Opt this clone into the repo's tracked hooks. Run once per checkout.
set -euo pipefail
cd "$(dirname "$0")/.."
git config core.hooksPath scripts/git-hooks
echo "git core.hooksPath -> scripts/git-hooks"
