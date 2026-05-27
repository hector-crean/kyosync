#!/usr/bin/env bash
# Regenerate cross-language SDK types from the Rust agent surface.
#
# Pipeline:
#   1. cargo run -p kyoso_agent --bin emit_schema → sdk/{python,typescript}/schemas/schema.json
#   2. datamodel-codegen (Pydantic v2) → sdk/python/src/kyoso_agent/_generated.py
#   3. json-schema-to-typescript          → sdk/typescript/src/_generated.ts  (skipped until TS SDK lands)
#
# Toolchain:
#   - cargo: required, used to emit the schema
#   - uv:    required for the Python step (runs datamodel-codegen in a managed env)
#   - npx:   required for the TypeScript step (json-schema-to-typescript)
#
# Invocation:
#   ./scripts/codegen.sh          # everything
#   ./scripts/codegen.sh schema   # just schema.json
#   ./scripts/codegen.sh python   # schema + Pydantic
#   ./scripts/codegen.sh ts       # schema + TypeScript

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

TARGETS="${1:-all}"

emit_schema() {
    echo "→ emitting schema.json"
    cargo run -q -p kyoso_agent --bin emit_schema > sdk/python/schemas/schema.json
    cp sdk/python/schemas/schema.json sdk/typescript/schemas/schema.json
    echo "  wrote sdk/{python,typescript}/schemas/schema.json"
}

emit_python() {
    if ! command -v uv >/dev/null 2>&1; then
        echo "✗ uv not found — install from https://docs.astral.sh/uv/"
        exit 1
    fi
    echo "→ datamodel-codegen → sdk/python/src/kyoso_agent/_generated.py"
    cd sdk/python
    uv run --no-project --with datamodel-code-generator datamodel-codegen \
        --input schemas/schema.json \
        --input-file-type jsonschema \
        --output src/kyoso_agent/_generated.py \
        --output-model-type pydantic_v2.BaseModel \
        --target-python-version 3.10 \
        --use-double-quotes \
        --use-schema-description \
        --use-field-description \
        --use-standard-collections \
        --use-union-operator \
        --field-constraints \
        --snake-case-field
    cd "$ROOT"
}

emit_typescript() {
    if ! command -v npx >/dev/null 2>&1; then
        echo "✗ npx not found"
        exit 1
    fi
    echo "→ json-schema-to-typescript → sdk/typescript/src/_generated.ts"
    npx --yes json-schema-to-typescript \
        sdk/typescript/schemas/schema.json \
        --output sdk/typescript/src/_generated.ts \
        --bannerComment "// Auto-generated from schema.json — DO NOT EDIT. Regenerate via scripts/codegen.sh."
}

case "$TARGETS" in
    schema) emit_schema ;;
    python) emit_schema; emit_python ;;
    ts|typescript) emit_schema; emit_typescript ;;
    all) emit_schema; emit_python; emit_typescript ;;
    *) echo "unknown target: $TARGETS (use: schema | python | ts | all)"; exit 1 ;;
esac
