# Bench harness

Single-command entry points for testing the system at every layer.
Designed to be invoked by AI agents (Claude Code, etc.) as well as
humans — every recipe writes machine-parseable reports under
`target/harness-reports/` so the caller doesn't have to scrape `cargo`
output.

Install [`just`](https://github.com/casey/just) once, then:

```bash
just                      # list every recipe
just bench-all            # the full suite (~5 min)
just bench-micro          # criterion only
just bench-load           # WS loadgen, all three profiles
just bench-load-graph     # one profile, default 8 clients × 100 ops/s × 10s
just test                 # cargo test --workspace
```

## Layers

### Layer 1 — Correctness (`just test`)

`cargo test --workspace`. 162 tests across:

- **Lattice + primitive proptest** (`crates/kyoso_crdt/tests/proptest_lattice.rs`)
  — random op sequences across N replicas, asserts convergence,
  commutativity, idempotency, order-independence. The bug-finding
  test for primitives.
- **Per-backend round-trip tests** (`crates/kyoso_graph_crdt/tests/`,
  `crates/kyoso_comments_crdt/tests/`) — `submit → apply → echo`
  through an in-memory log shared between two replicas, checking
  cross-model anchor references resolve identically.
- **Multi-app end-to-end** — `crates/kyoso_graph_sync/tests/two_apps.rs`,
  `crates/kyoso_comments_sync/tests/two_apps.rs`,
  `apps/kyoso_server/tests/multi_model.rs` — full Bevy apps + real
  WebSocket + real server.

What it catches: CRDT bugs, regressions in the apply pipeline,
multi-model integration breakage. Run before every commit.

### Layer 2 — Micro-benchmarks (`just bench-micro`)

`criterion` benches, one binary per crate. Outputs:

- **HTML reports**: `target/criterion/report/index.html` — visual
  per-bench plots with statistical comparison against the saved
  baseline.
- **JSON**: `target/criterion/<group>/<bench>/new/estimates.json` —
  raw per-iteration timings for downstream analysis.

| Crate | Bench file | What it measures |
|-------|-----------|------------------|
| `kyoso_crdt` | `benches/primitives.rs` | `LwwRegister`/`OrSet`/`PnCounter` apply, mutate, join; `postcard` encode/decode at varying op counts |
| `kyoso_graph_crdt` | `benches/backend.rs` | `CrdtBackend::apply_remote` replay rate (100/1k/10k ops); `snapshot` + `restore` cost; `would_create_cycle` on tall trees |
| `kyoso_comments_crdt` | `benches/backend.rs` | `CommentsBackend` apply replay; snapshot/restore at varying comment counts; `EditBody` LWW path |
| `kyoso_server` | `benches/handler.rs` | Per-handler `submit` (graph + comments); `welcome_for` cost growth (100/1k/10k ops); `Room` router lookup |

Save a baseline:

```bash
just bench-micro          # writes baseline named "harness"
# ... refactor ...
just bench-compare        # diff against saved baseline
```

What it catches: O(n) → O(n²) regressions, allocation regressions in
hot paths, encoding-cost growth.

### Layer 3 — End-to-end loadgen (`just bench-load*`)

The `kyoso_loadgen` binary spawns N concurrent WS clients, drives a
target submit rate, and records per-op submit-to-echo latency in an
HDR histogram.

Each profile writes `target/harness-reports/loadgen-<profile>.json`:

```json
{
  "config": {
    "clients": 8, "rate_per_client": 100, "duration_s": 10,
    "model": "graph", "room": "loadgen"
  },
  "ops_submitted": 8000,
  "ops_echoed": 8000,
  "errors": 0,
  "elapsed_s": 10.42,
  "throughput_ops_per_sec": 767.8,
  "latency_us": {
    "p50": 5400, "p90": 11200, "p95": 12800,
    "p99": 25000, "p999": 41000, "max": 84000,
    "mean": 6840.4, "stddev": 3120.1
  }
}
```

Three profiles:

- `bench-load-graph` — graph-only.
- `bench-load-comments` — comments-only.
- `bench-load-mixed` — half the clients submit graph, half submit
  comments. The interesting comparison: with the post-refactor
  per-handler append-locks, mixed throughput should be roughly
  additive (each model has its own lock; they don't serialise against
  each other). If it isn't, something regressed in the handler split.

All recipes accept positional overrides:

```bash
just bench-load-graph 32 200 30   # 32 clients × 200 ops/s × 30s
```

In-process server is the default (`--spawn-server`). To target an
external server:

```bash
cargo run --release -p kyoso_loadgen -- \
    --url ws://my-server:7878/ws \
    --room bench --model mixed \
    --clients 64 --rate 500 --duration 60 \
    --output report.json
```

What it catches: throughput ceiling regressions, p99 latency cliffs,
broadcast-channel saturation, per-handler-lock-isolation regressions.

### Layer 4a — Logical chaos simulator (`just chaos`)

Pure in-process simulator. Spawns N replicas of any `CrdtModel`,
fans ops through a virtual broadcast that drops / reorders / delays
with a seeded RNG, then asserts every replica's snapshot matches a
canonical replica (one that received every op in stamped order).
**No tokio, no WebSocket, no real timing** — fully deterministic
given the seed.

Each sweep writes `target/harness-reports/chaos-<model>.json`:

```json
{
  "all_converged": true,
  "runs": [
    {
      "config": {
        "peers": 5, "op_rounds": 200,
        "drop_probability": 0.15, "max_delay_rounds": 5,
        "seed": 3405705229
      },
      "converged": true,
      "ops_issued": 367,
      "deliveries": 1835,
      "re_delivered_after_drop": 309,
      "peer_applied_seqs": [367, 367, 367, 367, 367]
    },
    …
  ]
}
```

Exit code is non-zero if any seed diverges — a failing run is a
real CRDT bug, with the seed printed so it reproduces bit-for-bit.

What it catches:

- **Convergence under reordering**: ops applied in different orders
  across peers converge to the same state.
- **Idempotency under retransmit**: dropped ops re-delivered later
  (when "the link recovers") still converge.
- **N-peer interactions**: hand-written tests max out at 2-3 peers;
  the chaos sim runs N=10+ trivially.

Recipes (positional overrides for tuning):

```bash
just chaos                        # graph + comments default sweep
just chaos-graph                  # 5 peers × 200 rounds × 10 seeds, 15% drop
just chaos-graph 8 500 0.30 10 25 # peers, rounds, drop, delay, seeds
just chaos-comments
just chaos-stress                 # heavy: 8×500, 30% drop, 25 seeds, both models
```

#### Findings

##### Resolved

- **`CrdtBackend` cascade-tombstone race on local-pre-applied edges.**
  Symptom: when peer A's `add_edge` is locally pre-inserted in the
  same round that peer B's `remove_node` (with a lower stamped seq)
  is broadcast, peer A's `apply_remote` of the RemoveNode
  cascade-tombstoned the freshly-inserted edge; peer A's own echoed
  AddEdge was a no-op via `or_insert`, so the peer ended up
  tombstoned where canonical stayed live.

  Surfaced by: this very chaos sim. The harness's `findings.json`
  pointed at `crates/kyoso_graph_crdt/src/backend.rs` with a
  one-line repro command per divergent seed.

  Fix: `apply_remote` for `OpKind::AddRefEdge` now uses `and_modify`
  to clear the tombstone on the existing entry instead of `or_insert`
  (which silently preserved it). Safe because edge ids are unique
  per `CrdtId` — the only way an entry could exist tombstoned at
  AddRefEdge apply time is via a lower-seq cascade, and AddRefEdge's
  higher seq legitimately supersedes it. Any RemoveRefEdge for the
  same id necessarily has an even higher seq and re-tombstones in
  apply order. (See `crates/kyoso_graph_crdt/src/backend.rs:apply_remote`.)

  Verified: 25 seeds × 8 peers × 500 rounds × 30% drop × 10-round
  delay all converge. The default `graph_mutate` workload now
  includes `remove_node` so this case acts as a permanent regression
  test against re-introducing the bug.

- **`move_node` concurrent-swap divergence (local pre-apply +
  per-replica cycle check).** Symptom: peer X locally pre-applied
  `move_node(N1, P1)` and peer Y locally pre-applied
  `move_node(P1, N1)` in the same logical moment. Each replica's
  `apply_remote` re-ran the cycle check against its OWN tree state
  (which had the pre-apply baked in), so X and Y rejected DIFFERENT
  ops, ending up at different final trees. Canonical (no pre-apply)
  picked one winner deterministically and every replica diverged
  from it in opposite directions.

  Surfaced by: chaos seed `0xCAFEF038` after extending
  `graph_mutate` to exercise `move_node` (~6%). Bisected to a
  3-line focused test in
  [`crates/kyoso_graph_crdt/tests/move_race.rs`](crates/kyoso_graph_crdt/tests/move_race.rs).

  Fix: `move_node` no longer pre-applies locally. The op is
  enqueued and the parent change only takes effect when the
  server-stamped echo lands in `apply_remote`, which runs the cycle
  check against the same tree state every other replica sees. A
  small `pending_moves: HashMap<CrdtId, CrdtId>` tracks the
  in-flight target → proposed-parent so the Bevy detection layer
  can suppress the echo round-trip. (See
  `crates/kyoso_graph_crdt/src/backend.rs::move_node`.)

- **`would_create_cycle` tombstone-walk divergence.** Symptom: same
  shape as above but one layer down. After the move-pre-apply fix
  landed, chaos seed `0xCAFEF026` still diverged at peers=8
  rounds=200 drop=0.30. Diff was a single node `(peer=6, seq=2)`
  with a different `tree_parent` between canonical and peer 6.
  Root cause: `would_create_cycle` walked
  `tree_parent` but **stopped at tombstoned nodes** — and
  `remove_node` STILL pre-applies tombstones locally. So peers
  whose local pre-applied tombstones short-circuited the walk
  decided cycles differently from canonical (which hadn't applied
  those tombstones yet at decision time).

  Fix: walk `tree_parent` regardless of tombstone state.
  Tombstoned nodes still carry valid `tree_parent` fields, and
  removing the filter makes the cycle decision a pure function of
  ops applied in seq order. (See
  `crates/kyoso_graph_crdt/src/backend.rs::would_create_cycle`.)

  Verified together: 50 seeds × 8 peers × 1000 rounds × 30% drop ×
  10-round delay (`just chaos-stress`) all converge after both
  fixes. Workspace tests pass.

### Layer 4b — WS-layer reconnect/chaos coverage (`cargo test -p kyoso_server --test reconnect`)

Real WebSocket / real `axum` server / real `tokio_tungstenite` —
not deterministic at the byte level, but the asserts are robust
because every test awaits its own response frames before continuing.
What it covers (none of which `two_clients.rs` / `multi_model.rs` did):

- **Mid-traffic disconnect + reconnect**: peer drops, reconnects with
  `since: N`, gets a Welcome whose `diff` carries every op that
  landed while it was away.
- **Reconnect across a snapshot boundary**: while the peer is
  disconnected, the server takes a snapshot. The reconnecting peer's
  Welcome includes `snapshot_payload = Some(…)` plus the tail diff.
- **Multi-model reconnect**: peer subscribes to graph + comments;
  while away, ops land on both models; on reconnect both per-model
  greetings carry the missed ops independently.
- **Stress**: N peers cycle through random disconnect/reconnect for
  a fixed number of ops; the final reconnect's diff coverage matches
  the full submitted set.

Run with `cargo test -p kyoso_server --test reconnect` (covered by
`just test`).

#### Future: madsim/turmoil

For *fully* deterministic WS-layer tests (reproducible-by-seed
network drops at the byte level), `madsim` or `turmoil` would
shim out tokio + `tokio_tungstenite`. Estimated ~1 day. Worth
doing if a flaky test surfaces in CI that the current real-WS
suite can't reproduce. Until then, the existing tests + Layer 4a
chaos cover the same surface area with a fraction of the
infrastructure.

### Layer 4c — capacity / scaling probes

Two complementary tools answer "how big can a room get, and what
should travel on the storage broadcast vs an ephemeral presence
channel?":

- `just wire-probe` — encodes one realistic instance of every
  storage op variant (graph + comments) plus representative
  presence payloads (cursor, viewport, selection at 1/5/20 ids,
  combined awareness) and reports payload / submit / apply bytes.
  Pure encode, no server. Output: `wire-sizes.json`.
- `just peer-sweep` — varies N peers against one in-process
  server and records throughput, p50/p99 latency, per-peer
  ingress, server egress. Default sweep `2..128` finds the
  latency knee; `just peer-sweep-high` (`128..512`) pushes past
  saturation. Output: `peer-sweep.json` /
  `peer-sweep-high.json`.

#### Findings — wire sizes

| op kind                                  | apply bytes |
|------------------------------------------|------------:|
| graph `AddNode`                          |          13 |
| graph `RemoveNode` / `RemoveRefEdge`     |          15 |
| graph `Move (root, 8B pos)`              |          25 |
| graph `Move (parented, 8B pos)`          |          27 |
| graph `AddRefEdge`                       |          18 |
| graph `SetNodeProperty (Lww<f32>)`       |          31 |
| graph `SetNodeProperty (Lww<32B str>)`   |          57 |
| comments `AddComment (root, 16B)`        |          36 |
| comments `AddComment (reply, 64B)`       |          87 |
| comments `EditBody (32B)`                |          53 |
| comments `DeleteComment`                 |          18 |
| presence `cursor (xy + modifier)`        |          13 |
| presence `viewport (xywh + zoom)`        |          23 |
| presence `selection (5 ids)`             |          14 |
| presence `combined awareness`            |          48 |

Envelope overhead is 8B (graph) / 11B (comments) per Submit; the
extra ~3B is the longer model-id slug. Note that **a cursor
presence payload is the same size as an `AddNode` storage op** —
which means the case for an ephemeral presence channel is *not*
about wire size, it's about persistence cost (no log append, no
permanent retention) and late-joiner cost (no catchup
replay).

#### Findings — peer-sweep

`peer-sweep` at 10 ops/s/peer with `AddNode` (smallest op, best
case for the broadcast channel):

| N peers | throughput | p50    | p99    | per-peer ingress | server egress | errors |
|--------:|-----------:|-------:|-------:|-----------------:|--------------:|-------:|
|       2 |     19/s   |  0.5ms |  1.1ms |      0.2 KB/s    |    0.5 KB/s   |     0  |
|      16 |    148/s   |  0.6ms |  0.9ms |      2.0 KB/s    |   32.1 KB/s   |     0  |
|      64 |    593/s   |  3.1ms |  6.0ms |      8.1 KB/s    |  516.7 KB/s   |     0  |
|     128 |   1184/s   | 10.4ms | 19.1ms |     16.2 KB/s    | 2070.7 KB/s   |     0  |
|     256 |   2353/s   | 17.7ms | 36.1ms |     33.2 KB/s    | 8489.7 KB/s   |     0  |
|     384 |   2633/s   |  897ms |  1.7s  |     37.7 KB/s    |14463.8 KB/s   |     0  |
|     512 |   2017/s   |  1.4s  |  2.6s  |     28.6 KB/s    |14640.8 KB/s   |     0  |

The broadcast-channel saturation knee is between **N=256
(healthy: 18ms p50)** and **N=384 (broken: 897ms p50)**. At and
above N=384 the server starts dropping clients with `broadcast
lag; dropping client missed=11..26` warnings — the per-client
broadcast queue overflows faster than the consumer drains it.
Throughput plateaus around 2600 ops/s and drops back as clients
get evicted.

Per-peer ingress scales linearly with N (each peer hears every
other peer's traffic); server egress is quadratic in N. At N=256
× 10 ops/s × 13B `AddNode`, server egress is already 8.5 MB/s —
which is well within network capability but saturates the
in-process broadcast channel's per-client buffer.

#### Implications — presence vs storage broadcast

The `AddNode` baseline above tells us how many *low-rate
mutation* ops one room can carry. Cursor traffic at realistic
rates would push that ceiling much lower:

- 256 peers × **60 cursor moves/sec** × 13B × 255 fanout ≈
  **51 MB/s** server egress. That's 6× the storage load at
  N=256 and would push the broadcast channel into the
  N=384-style saturation regime within seconds.
- Same workload on a *separate presence channel* (no log
  append, no retention, dedicated broadcast queue) needs the
  same wire bandwidth but doesn't share buffer headroom with
  storage — so storage stays at its measured ~256-peer
  ceiling while presence can independently saturate at its
  own (much higher, since it skips disk + log + catchup).
- Putting cursors on storage would also append **15,360
  ops/sec to the log forever** at N=256/60Hz — late joiners
  would download cursor history they never asked for.

**Recommendation.** Anything emitted at >5 Hz/peer
(cursor/viewport/transient selection) belongs on the presence
channel. Anything edit-shaped (AddNode, EditBody, Move) belongs
on storage regardless of rate. The N=256 storage ceiling is the
operational budget; presence sits outside that budget. If a
single room must hold more than ~256 active editors, the
storage-channel architecture needs a sharded broadcast
(per-subset of peers) — beyond the current scope.

#### Tiered fanout (Phase 1–3, shipped)

The `Hello` envelope now declares a `Tier` (`ReadWrite` /
`Read`); the server runs two fanout paths per room:

- **Live path** — `ReadWrite` connections receive each `Apply`
  individually (uncoalesced). Writer-tier latency budget
  ≤ 50ms p99.
- **Coalesced path** — `Read` connections buffer received
  payloads per-model and flush as `ApplyBatch` frames every
  250ms (configurable via `KYOSO_READER_COALESCE_MS`). Trades
  ~quarter-second freshness for ~10× more peers per room.

Per-model handlers gate observer-tier writes via
`RoomModelHandler::allows_submit(tier, payload)`. Default:
writer-only. Comments handler overrides to allow comments from
readers — anyone in the room may comment.

##### Phase-3 `peer-sweep-readers` results

`just peer-sweep-readers` — 4 writers @ 10 ops/s, varying reader
count:

| readers | writer thr | writer p50 | writer p99 | server egress | errors |
|--------:|-----------:|-----------:|-----------:|--------------:|-------:|
|       0 |     37/s   |    0.7ms   |    2.4ms   |    2.0 KB/s   |     0  |
|      64 |     37/s   |    1.0ms   |    2.2ms   |   19.1 KB/s   |     0  |
|     256 |     37/s   |    0.9ms   |    3.9ms   |   69.4 KB/s   |     0  |
|    1024 |     37/s   |    2.1ms   |   19.4ms   |  275.2 KB/s   |     0  |
|    2048 |     18/s   |    3.5ms   |   13.7ms   |  255.0 KB/s   |     7  |

Writer p99 stays under 20ms with 1024 readers attached — a
regime the pre-Phase-3 unified channel could not reach (it
broke between N=256 and N=384 peers). The 2048-reader step
loses throughput (writer thr halves, 7 errors) because the
test rig is hitting `ulimit -n 4096` headroom; not an
architectural ceiling.

The "presence belongs off storage" recommendation above still
stands — high-frequency cursor/viewport traffic should never
touch storage regardless of tier — but for ordinary edit-shaped
ops the tiered fanout means a 1000-observer room is a routine
configuration, not an emergency.

## Feedback loop — `just findings`

The harness layers above all write structured reports under
`target/harness-reports/` + `target/criterion/`. The findings
orchestrator reads every report and emits one unified document per
sweep, optimised for AI consumption:

```bash
just bench-all          # produces all the per-layer reports
just findings           # aggregates them into findings.{json,md}
just loop-step          # findings + prints next_actions for an agent
```

`target/harness-reports/findings.json` schema:

```json
{
  "generated_at": "2026-05-09T10:28:08+00:00",
  "summary": {
    "chaos": {"sweeps_run": 2, "total_seeds": 15, "diverged_seeds": 5,
              "all_converged": false},
    "loadgen": {"profiles": {"graph": {…}, "mixed": {…}}},
    "criterion": {"bench_count": 3, "bench_names": ["lww_register/…", …]},
    "reconnect": {"passed": null}
  },
  "findings": [
    {
      "severity": "critical",
      "layer": "chaos",
      "title": "graph chaos sim diverged at seed 0xCAFEF00D",
      "details": "Peers reached the same applied_seq but snapshots diverge…",
      "repro": "just chaos-graph 5 100 0.1 3 1 && # rerun with --first-seed …",
      "suspected_files": [
        "crates/kyoso_graph_crdt/src/backend.rs",
        "crates/kyoso_graph_crdt/src/op.rs"
      ]
    }
  ],
  "next_actions": [
    "[chaos] graph chaos sim diverged at seed 0xCAFEF00D"
  ]
}
```

Findings are sorted `Critical → High → Medium → Low → Info`.
`next_actions` is the top-5 high-severity titles. The orchestrator
exits non-zero (2) if any `Critical` finding exists — useful as a
CI gate.

### The agent loop

For Claude Code (or another AI coding agent) iterating on the codebase:

1. Run `just findings-run` (or `just bench-all` + `just findings`).
2. Read `target/harness-reports/findings.json`.
3. Pick the top entry in `findings` (it'll be the highest severity).
4. Open `suspected_files` and the source they point at.
5. Make a fix.
6. Re-run `just findings-run` (or just the affected layer + `just findings`).
7. Verify the finding cleared from `next_actions` and the run is now
   `all_converged: true` / `errors: 0` / etc.

The `just loop-step` recipe is a one-shot version of step 1+2+3:
runs `just findings`, then prints `next_actions` to stdout. An
agent can loop on it.

### Adding a finding rule

`crates/kyoso_loadgen/src/findings.rs` is where new severity rules
live. To raise an `Info` observation to `High` (e.g. p99 above some
threshold), edit `parse_loadgen` and add a new `Finding`. To map
new layer outputs (e.g. a new `tests-summary.json`), add a
`parse_<layer>` function and call it from `summarize`.

The `suspected_files` field is hand-mapped per finding type — extend
the `divergence_finding` / equivalent helpers to add coverage. The
mapping is intentionally hardcoded rather than heuristic so the
agent sees stable, predictable hints.

## Output schema reference

For agents parsing reports under `target/harness-reports/`:

| File | Format | Source |
|------|--------|--------|
| `loadgen-{graph,comments,mixed}.json` | JSON, top-level `LoadReport` (see above) | `kyoso_loadgen` binary |
| `chaos-{graph,comments,…}.json` | JSON, top-level `SweepReport { all_converged, runs: [ChaosReport, …] }` | `kyoso_chaos` binary |
| `criterion-*.log` | Mixed text + JSONL (criterion's `--message-format=json` lines, prefixed by status) | `cargo bench --message-format=json` |
| `target/criterion/<group>/<bench>/new/estimates.json` | JSON, criterion's per-bench statistics | `criterion` itself |
| `findings.json` | JSON, top-level `Findings { summary, findings, next_actions }` | `kyoso_harness summarize` |
| `findings.md` | Human-readable Markdown rendering of `findings.json` | `kyoso_harness summarize` |

The Rust source for the JSON shapes:

- Loadgen: `crates/kyoso_loadgen/src/lib.rs` — `LoadReport`,
  `LoadConfigSer`, `LatencyPercentiles`.
- Chaos: `crates/kyoso_loadgen/src/sim.rs` — `ChaosConfig`,
  `ChaosReport`, `SweepReport`.
- Criterion: documented at
  https://bheisler.github.io/criterion.rs/book/user_guide/csv_output.html
  (the JSON shape mirrors the CSV).

## Adding a new bench

1. **Micro-bench**: drop `benches/<name>.rs` in the relevant crate,
   declare `[[bench]] name="<name>" harness=false` in its
   `Cargo.toml`, add `criterion` as a dev-dep. Use `criterion_group!`
   + `criterion_main!`.
2. **Load profile**: extend `LoadModel` in `kyoso_loadgen/src/lib.rs`
   with a new variant + a `client_loop` arm; add a `bench-load-<name>`
   recipe in the `Justfile`.
3. **Document it here** under the matching layer.
