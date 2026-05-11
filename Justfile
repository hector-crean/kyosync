## kyoso bench harness
##
## Single-command entry points for every test/bench level. Reports
## land in `target/harness-reports/` so AI agents (and humans) can
## parse without invoking `cargo` themselves.
##
## Run `just` (no args) for the full menu.

set shell := ["bash", "-cu"]

REPORT_DIR := "target/harness-reports"

# Default: list every recipe.
default:
    @just --list

# ---------------------------------------------------------------------
# Layer 1 — correctness
# ---------------------------------------------------------------------

# Run every test in the workspace (unit + proptest + integration).
test:
    cargo test --workspace --no-fail-fast

# Just the proptest convergence/lattice-axiom tests.
test-proptest:
    cargo test --workspace --no-fail-fast -- proptest

# ---------------------------------------------------------------------
# Layer 2 — micro-benchmarks (criterion)
# ---------------------------------------------------------------------

# Run every criterion bench in the workspace. Reports land in
# `target/criterion/` (HTML) and per-run JSON in REPORT_DIR.
# Scoped to the four bench-bearing crates — `cargo bench --workspace`
# would also run unit-test harnesses on every other crate, which
# don't recognize `--save-baseline`.
bench-micro:
    @mkdir -p {{REPORT_DIR}}
    cargo bench -p kyoso_crdt -p kyoso_graph_crdt -p kyoso_comments_crdt -p kyoso_server \
        -- --save-baseline harness 2>&1 \
        | tee {{REPORT_DIR}}/criterion-$(date +%s).log
    @echo "criterion HTML reports: target/criterion/report/index.html"

# Per-crate sub-recipes.
bench-crdt:
    cargo bench -p kyoso_crdt
bench-graph-crdt:
    cargo bench -p kyoso_graph_crdt
bench-comments-crdt:
    cargo bench -p kyoso_comments_crdt
bench-server:
    cargo bench -p kyoso_server

# ---------------------------------------------------------------------
# Layer 3 — end-to-end loadgen
# ---------------------------------------------------------------------

# All three load profiles against an in-process server.
bench-load: bench-load-graph bench-load-comments bench-load-mixed

# Graph-model only. 8 clients × 100 ops/s × 10s ≈ 8k ops.
bench-load-graph CLIENTS="8" RATE="100" DURATION="10":
    @mkdir -p {{REPORT_DIR}}
    cargo run --release -p kyoso_loadgen --bin kyoso_loadgen -- \
        --spawn-server \
        --model graph \
        --clients {{CLIENTS}} \
        --rate {{RATE}} \
        --duration {{DURATION}} \
        --output {{REPORT_DIR}}/loadgen-graph.json

bench-load-comments CLIENTS="8" RATE="100" DURATION="10":
    @mkdir -p {{REPORT_DIR}}
    cargo run --release -p kyoso_loadgen --bin kyoso_loadgen -- \
        --spawn-server \
        --model comments \
        --clients {{CLIENTS}} \
        --rate {{RATE}} \
        --duration {{DURATION}} \
        --output {{REPORT_DIR}}/loadgen-comments.json

# 50/50 mix — half of CLIENTS submit graph, the other half comments.
# Tests whether per-handler append-locks let the two models scale
# independently (post per-model-handler refactor they should).
bench-load-mixed CLIENTS="8" RATE="100" DURATION="10":
    @mkdir -p {{REPORT_DIR}}
    cargo run --release -p kyoso_loadgen --bin kyoso_loadgen -- \
        --spawn-server \
        --model mixed \
        --clients {{CLIENTS}} \
        --rate {{RATE}} \
        --duration {{DURATION}} \
        --output {{REPORT_DIR}}/loadgen-mixed.json

# ---------------------------------------------------------------------
# Layer 4a — chaos simulator (deterministic, in-process)
# ---------------------------------------------------------------------

# Run all chaos sweeps. Each sweep runs N seeds against the named
# model under heavy network chaos (drops, reorders, delays) and
# asserts every replica converges to the canonical state. Exit code
# is non-zero if any seed diverges — so a failing run is a real
# CRDT bug.
chaos: chaos-graph chaos-comments

# Default: 5 peers × 200 rounds × 10 seeds, 15% drop, 5-round max delay.
chaos-graph PEERS="5" ROUNDS="200" DROP="0.15" DELAY="5" SEEDS="10":
    @mkdir -p {{REPORT_DIR}}
    cargo run --release --bin kyoso_chaos -- \
        --model graph \
        --peers {{PEERS}} --rounds {{ROUNDS}} \
        --drop-prob {{DROP}} --max-delay {{DELAY}} \
        --seeds {{SEEDS}} \
        --output {{REPORT_DIR}}/chaos-graph.json

chaos-comments PEERS="5" ROUNDS="200" DROP="0.15" DELAY="5" SEEDS="10":
    @mkdir -p {{REPORT_DIR}}
    cargo run --release --bin kyoso_chaos -- \
        --model comments \
        --peers {{PEERS}} --rounds {{ROUNDS}} \
        --drop-prob {{DROP}} --max-delay {{DELAY}} \
        --seeds {{SEEDS}} \
        --output {{REPORT_DIR}}/chaos-comments.json

# Heavy stress: 8 peers × 500 rounds × 25 seeds, 30% drop, 10-round
# max delay. Catches edge cases the default cadence misses. Slower.
chaos-stress:
    @mkdir -p {{REPORT_DIR}}
    cargo run --release --bin kyoso_chaos -- \
        --model graph --peers 8 --rounds 500 \
        --drop-prob 0.3 --max-delay 10 --seeds 25 \
        --output {{REPORT_DIR}}/chaos-graph-stress.json
    cargo run --release --bin kyoso_chaos -- \
        --model comments --peers 8 --rounds 500 \
        --drop-prob 0.3 --max-delay 10 --seeds 25 \
        --output {{REPORT_DIR}}/chaos-comments-stress.json

# ---------------------------------------------------------------------
# Layer 4c — capacity / scaling probes
# ---------------------------------------------------------------------

# Per-op-kind wire-size table for graph + comments storage ops, plus
# representative presence payloads (cursor, viewport, selection). Pure
# encode-and-measure — no server, no clients. Dataset for the
# presence-vs-storage decision.
wire-probe:
    @mkdir -p {{REPORT_DIR}}
    cargo run --release -p kyoso_loadgen --bin kyoso_wire_probe -- \
        --output {{REPORT_DIR}}/wire-sizes.json

# Mixed-tier peer sweep. Holds WRITERS fixed, varies reader count
# across READERS list. Each step measures writer-tier echo latency
# (live broadcast path) plus per-reader ingress and total server
# egress (live + coalesced fanouts).
#
# Default: writer-only baseline at increasing N — directly
# comparable to the pre-tier numbers.
peer-sweep WRITERS="4" READERS="0,16,64,256,1024" RATE="10" DURATION="5":
    @mkdir -p {{REPORT_DIR}}
    @ulimit -n 4096 || true
    cargo run --release -p kyoso_loadgen --bin kyoso_peer_sweep -- \
        --writers {{WRITERS}} --readers {{READERS}} \
        --rate {{RATE}} --duration {{DURATION}} \
        --output {{REPORT_DIR}}/peer-sweep.json

# Push reader count until either the live writer path or the
# coalesced reader path saturates. Demonstrates the Phase 3 win:
# storage-channel writer p99 stays flat while readers grow into
# territory the unified channel could never reach.
peer-sweep-readers:
    @just peer-sweep 4 "0,64,256,1024,2048" 10 5

# ---------------------------------------------------------------------
# All-in-one
# ---------------------------------------------------------------------

# Run every level. Takes a few minutes. Output:
# - target/harness-reports/criterion-*.log
# - target/harness-reports/loadgen-{graph,comments,mixed}.json
# - target/harness-reports/chaos-{graph,comments}.json
# - target/criterion/report/index.html
bench-all: bench-micro bench-load chaos
    @echo ""
    @echo "=== bench-all complete ==="
    @ls -la {{REPORT_DIR}}/

# Compare current criterion benches against the saved `harness` baseline.
bench-compare:
    cargo bench -p kyoso_crdt -p kyoso_graph_crdt -p kyoso_comments_crdt -p kyoso_server \
        -- --baseline harness

# Wipe accumulated bench data.
bench-clean:
    rm -rf target/criterion {{REPORT_DIR}}

# ---------------------------------------------------------------------
# Findings — aggregate every report into a single JSON + Markdown
# document. AI agents read findings.json; humans read findings.md.
# Exit code is non-zero (2) if any Critical finding surfaced — useful
# in CI as a quality gate.
# ---------------------------------------------------------------------

# Read whatever's already in target/harness-reports/ + target/criterion/
# and emit findings.{json,md}. Fast, no rebuild, no test runs.
findings:
    cargo run --release -p kyoso_loadgen --bin kyoso_harness -- summarize

# Run the full bench suite first, then summarize. Self-contained —
# the AI's "give me a fresh report" command.
findings-run:
    cargo run --release -p kyoso_loadgen --bin kyoso_harness -- run

# One iteration of the AI feedback loop: summarize, then print the
# top finding (or "clean run") so it lands in stdout for an agent.
loop-step:
    @just findings >/dev/null
    @if [ -f target/harness-reports/findings.json ]; then \
        echo "--- next_actions ---"; \
        jq -r '.next_actions[]' target/harness-reports/findings.json 2>/dev/null \
            || echo "(install jq for parsed output; raw findings.json is at target/harness-reports/findings.json)"; \
    else \
        echo "no findings.json — run \`just bench-all\` first"; \
    fi
