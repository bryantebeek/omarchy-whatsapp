#!/usr/bin/env bash
set -euo pipefail

if command -v mise >/dev/null 2>&1; then
  exec mise exec rust@nightly-2026-06-16 -- cargo "$@"
fi

exec cargo "$@"
