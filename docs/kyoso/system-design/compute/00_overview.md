# System design — overview

_Status: living draft. Last updated 2026-05-09._

## What we're designing

A coherent architecture for the next major slice of kyoso, covering:

1. **Persistent storage** — durable op log + snapshots + auth + worker
   queue (currently in-memory only).
2. **Real authorization** — replace the phase-1 stub that grants any
   tier the client requests. Token-based identity, role-based room
   permissions.
3. **Presence channel split** — formalize and lock in that presence is
   *never* persisted. (Already structurally separated since phase 3
   tiered fanout; this codifies the rule.)
4. **Compute layer** — server-coordinated dirty-propagation across
   user-authored compute graphs, with three dispatch modes (in-process,
   queue, RPC) for three workload classes.
5. **Worker pools** — separately-scaled compute resources accessible
   across rooms; not per-room peers.
6. **Topology** — server-authoritative as today, with an explicit story
   for *local* compute (running heavy workers on the user's own
   machine when possible).

Each of these is its own doc:
- [01_storage.md](01_storage.md)
- [02_authorization.md](02_authorization.md)
- [03_compute.md](03_compute.md)
- [04_workers.md](04_workers.md)
- [05_topology.md](05_topology.md)
- [06_data_formats.md](06_data_formats.md)

## Where we are today (2026-05-09)

- Multi-model CRDT framework (`kyoso_crdt`) with graph and comments
  models implemented.
- WebSocket envelope protocol with `Tier::ReadWrite | Tier::Read`,
  per-model `allows_submit` policy, and dual-fanout broadcast (live for
  writers, 250ms-coalesced `ApplyBatch` for readers).
- 5-layer test harness ([HARNESS.md](../../HARNESS.md)) — correctness
  tests, criterion benches, end-to-end loadgen, deterministic chaos
  simulator, real-WS reconnect tests, capacity probes.
- Validated in [layer 4c](../../HARNESS.md#layer-4c--capacity--scaling-probes):
  4 writers + 1024 readers stays at sub-20ms p99 writer echo. Pre-tier
  unified channel broke between N=256 and N=384 peers.

## What's NOT yet there

- Storage is **in-memory only** — server restart loses all op history.
- Auth is a **stub** — any client requesting `Tier::ReadWrite` gets
  it; there is no notion of identity or per-room permissions.
- Workers exist only as a future concept; the architecture must
  accommodate three very different workload classes (real-time
  in-process, heavy queued, ML inference RPC).
- No persistence story for compute results, no cache, no dirty
  propagation.

## Goals

- **Single-DB storage substrate** for v1 (Postgres). One thing to back
  up, one schema-migration story, one transactional boundary. Plan a
  clear extraction path for the op log specifically if it ever needs
  to leave (Kafka/FoundationDB/sharded PG).
- **Server-authoritative model preserved.** The `GlobalSeq`-stamping
  central authority is what makes the protocol simple; we don't go
  pure-p2p.
- **Workers scale independently of rooms.** A pool of N inference
  workers serves R rooms, not R workers per room.
- **Compute layer reuses the rabona `flow` kernel** rather than
  reinventing it. The pipeline-as-data + Arrow-port-schemas + dispatch
  trait pattern is the right one and is already designed.
- **Local-first compute is a first-class option.** A worker running on
  the user's own machine is the same shape as one running in our
  cluster — different `WorkerLocator`, same protocol.
- **Presence stays out of storage forever.** No matter how convenient
  it might look later. Asserted at runtime in v2.

## Non-goals (for this slice)

- Multi-region / geographic routing. Single region for v1.
- Pure peer-to-peer mode (no server). Out of scope.
- A bespoke distributed database. We use Postgres until measurement
  forces a change.
- Sharded broadcast for >1000 active writers per room. The current
  ceiling is fine for the target use case; revisit if a real workload
  appears that needs it.
- Full SpacetimeDB / Convex-style "the database is the server"
  rewrite. Considered and rejected — see
  [06_data_formats.md](06_data_formats.md).

## Reference architectures we're learning from

- **rabona** ([`docs/architecture/`](../../../workspaces/rabona/docs/architecture/))
  — pipeline DAG, stage trait, three dispatch modes, content-addressed
  caching, Arrow ports. Most of the compute design lifts directly from
  here.
- **rerun** — Arrow-everything, time as a first-class column,
  component-based decomposition, columnar query store. Influences our
  compute-result format and a future history-query story.
- **Salsa / Bazel** — content-addressed cache key for incremental
  computation. Our compute orchestrator uses the same pattern.
- **Linear / Replicache / Jazz** — local-first patterns. Considered for
  the topology story; we land on server-authoritative with optional
  local workers as a hybrid.

## Open architectural questions

These need resolution before the corresponding sub-doc can move from
draft to settled. Listed here so they don't get lost.

1. **Compute nodes: user-visible (Weave-style) or server-internal?**
   — see [03_compute.md](03_compute.md). Affects authz model.
2. **Worker authentication: service accounts or per-job tokens?** —
   see [04_workers.md](04_workers.md). Service accounts for v1, JWT
   per-job for v2 if needed.
3. **Cache scope: per-room, per-org, global?** — see
   [01_storage.md](01_storage.md). Probably per-org with global
   sub-cache for content-addressable model outputs.
4. **Compute output transport: postcard via existing Submit path, or
   Arrow via a new path?** — see [06_data_formats.md](06_data_formats.md).
5. **Local-server option: just compute workers, or full kyoso_server
   in-process for offline mode?** — see [05_topology.md](05_topology.md).

## Sequencing

Each sub-doc has its own implementation plan. The dependency order:

```
01_storage   ─┐
02_authz     ─┼──► 03_compute ──► 04_workers
              │       (orchestrator)   (queue + worker proc)
05_topology   │
              ▼
       06_data_formats
       (cross-cutting reference)
```

Storage and authz can land in parallel (they share Postgres but no
code paths). Compute orchestrator depends on both (needs persistence
for the cache + auth for service accounts). Workers depend on the
orchestrator. Topology and data-format docs are reference, not
implementation steps.

Estimated total: 4-5 weeks of focused work for v1 of all five
slices. Each slice is independently shippable; partial completions
still produce a usable system.
