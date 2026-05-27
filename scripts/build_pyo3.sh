#!/usr/bin/env bash
# Build the kyoso_agent_py PyO3 extension and install it into
# sdk/python's venv. Run from anywhere — the script resolves paths.
#
# Prereqs (one-time):
#   cd sdk/python && uv sync --extra dev   # creates .venv + installs maturin
#
# Then:
#   ./scripts/build_pyo3.sh                # debug build (fast, slow runtime)
#   ./scripts/build_pyo3.sh --release      # release build (slow, fast runtime)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
PYSDK="$ROOT/sdk/python"
NATIVE="$ROOT/crates/kyoso_agent_py"

if [[ ! -d "$PYSDK/.venv" ]]; then
    echo "✗ no virtualenv at $PYSDK/.venv — run \`cd sdk/python && uv sync --extra dev\` first"
    exit 1
fi

MODE="${1:-}"
EXTRA=""
if [[ "$MODE" == "--release" ]]; then
    EXTRA="--release"
fi

# `maturin develop` installs the built wheel into the venv's site-packages.
# We invoke it via uv so the right Python interpreter is picked up.
cd "$NATIVE"
VIRTUAL_ENV="$PYSDK/.venv" \
    uv run --project "$PYSDK" maturin develop $EXTRA

echo "✓ kyoso_agent_py installed into $PYSDK/.venv"
