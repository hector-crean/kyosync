# Topology

_Status: draft. Where does the code run, and what are the
alternatives we considered._

## v1 topology — server-authoritative, single region

```
                ┌──────────────────────────────┐
                │  Cloud (single region)       │
                │                              │
                │  ┌─────────────────────────┐ │
                │  │  kyoso_server replicas  │ │
                │  │  (axum + tokio)         │ │
                │  │  - WS termination       │ │
                │  │  - per-room handlers    │ │
                │  │  - per-room compute     │ │
                │  │    orchestrator         │ │
                │  └────────┬────────────────┘ │
                │           │                  │
                │  ┌────────▼────────────────┐ │
                │  │  Postgres (HA)          │ │
                │  │  - op log (partitioned) │ │
                │  │  - snapshots            │ │
                │  │  - auth                 │ │
                │  │  - jobs                 │ │
                │  │  - cache (small)        │ │
                │  └─────────────────────────┘ │
                │                              │
                │  ┌─────────────────────────┐ │
                │  │  Object store (S3)      │ │
                │  │  - large snapshots      │ │
                │  │  - compute outputs >1MB │ │
                │  └─────────────────────────┘ │
                │                              │
                │  ┌─────────────────────────┐ │
                │  │  Worker pool            │ │
                │  │  (kyoso_worker × N)     │ │
                │  └─────────────────────────┘ │
                │                              │
                │  ┌─────────────────────────┐ │
                │  │  Inference workers      │ │
                │  │  (GPU nodes × M)        │ │
                │  └─────────────────────────┘ │
                └──────┬───────────────────────┘
                       │
                       │ WebSocket
                       │
              ┌────────▼─────────────┐    ┌────────────────┐
              │  Browser / desktop   │    │  Local worker  │
              │  client (Bevy)       │◄──►│  (optional;    │
              │                      │    │   user's       │
              │                      │    │   machine)     │
              └──────────────────────┘    └────────────────┘
```

**Server replicas** can be more than one but each room is sticky to
one replica (the one holding the in-memory mirror + orchestrator).
Round-robin LB based on `room_id`. Rebalancing on replica death is
"the next client to connect to that room lands on a different
replica, which boots the room from Postgres". No live migration in
v1.

**Postgres HA** via standard primary + read replica. All writes go to
primary; we don't try to scale reads to the replica because the
op-log access pattern is already write-dominant.

**Object store** is S3 in production, MinIO in docker-compose for
local dev.

**Worker pool** scales by autoscaler watching `jobs` queue depth +
per-kind p95 latency.

**Inference workers** run on GPU nodes. Sized manually per stage
(SAM3 worker pool, embedding worker pool, etc.).

**Local workers** are optional; the `worker_registrations` table
tracks per-user local workers and the orchestrator prefers them when
available. See [04_workers.md](04_workers.md).

## Room lifecycle — hydrate on join, evict on leave

Per-room memory is the dominant resource on each server replica.
Rooms have a clear lifecycle that maps onto user activity:

**Today (no eviction):**
- Room registry: `DashMap<RoomId, Arc<Room>>` in `RoomManager`
  ([apps/kyoso_server/src/services/room.rs:241](../../apps/kyoso_server/src/services/room.rs:241)).
- **Hydrate on first join is implemented.**
  `RoomManager::get_or_create`
  ([room.rs:255](../../apps/kyoso_server/src/services/room.rs:255))
  lazily calls `Room::restore` which rebuilds the per-handler
  in-memory mirror from the `OpStore`. Today the OpStore is
  in-memory so "restore" is a no-op replay; once
  [01_storage.md](01_storage.md) lands and OpStore is Postgres,
  restore performs the actual range scan from the latest snapshot
  to head.
- **Eviction is NOT implemented.** `Room::release_peer`
  ([room_ws.rs](../../apps/kyoso_server/src/handlers/room_ws.rs))
  clears that peer's ack row + presence; the `Room` itself stays in
  the DashMap until server restart. Means today: every room ever
  touched accumulates in memory.

**Per-room memory cost (back-of-envelope):**
- Empty room: ~30 KB (DashMap entry + 256-deep broadcast buffer +
  handler structs).
- Loaded room (10k graph nodes + 1k comments): ~5-10 MB,
  dominated by the in-memory graph mirror + comments log.
- A 64 GB machine therefore holds ~5,000-10,000 actively loaded
  rooms comfortably. Untested — `RoomManager::count()`
  ([room.rs:272](../../apps/kyoso_server/src/services/room.rs:272))
  exists but isn't exposed as a metric, and the harness measures
  peers per room, not rooms per server.

**Planned eviction (gated on storage slice):**

When the OpStore is durable (Postgres), eviction becomes safe:
dropping a `Room` from the `DashMap` discards the in-memory mirror,
not the data. The next `get_or_create` rebuilds it from Postgres —
exactly the path that already exists for first-join hydration.

Mechanism:
- `Room` gains `last_peer_left_at: Option<Instant>`, set on the
  transition "presence size 1 → 0" and cleared on any new
  `assign_peer`.
- The scheduler tick
  ([apps/kyoso_server/src/services/scheduler.rs](../../apps/kyoso_server/src/services/scheduler.rs))
  sweeps the registry; rooms whose `last_peer_left_at + grace` is
  in the past get dropped from the DashMap.
- Default grace: 5 minutes. Configurable per-deployment via env
  var. Long enough to absorb any realistic reconnect; short enough
  to keep memory bounded.

This pushes "rooms in memory" from "every room ever touched" to
"currently active + recently active". Combined with bigger machines,
that's plenty for a long time before sharding becomes necessary.

**Why eviction waits for the storage slice.** Without durable
OpStore, eviction is data loss. The dependency order is strict:
[01_storage.md](01_storage.md) → eviction → (much later, if needed)
sharding.

**Sharding deferred — when it does become the right answer.**

Sharding (room → replica routing) is the next-tier scaling lever
beyond eviction + bigger machines. It becomes the right move when:

- Active room state (loaded rooms × per-room memory) exceeds one
  machine's RAM even with eviction working.
- Cross-region latency is intolerable enough that we want regional
  replicas owning regional rooms.
- A single replica's tokio executor is CPU-bound across all the
  rooms it holds.

None of these are in evidence today. Even at 10,000 active rooms
with 5 MB each, a single 64 GB server has headroom. Eviction +
vertical scaling probably gets us tens of thousands of total rooms
with thousands active before sharding is unavoidable.

When sharding does become the answer, the path is **consistent-hash
routing of `room_id` to replica index** (rendezvous hashing — no
central coordinator). The LB makes connections sticky on `room_id`.
On replica add/remove, a small fraction of rooms re-bind to a new
replica — that replica boots them fresh from Postgres via
`Room::restore`. **Same mechanism eviction relies on.**

## Local server in-process — a real option later

The architecture allows running kyoso_server inside the client
process for offline / single-user / privacy modes. Two flavors:

**a. Single-user offline mode.**
- kyoso_server runs in-process inside the Bevy app.
- `OpStore` impl backed by SQLite (alternative to Postgres).
- No WS at all — handlers consume in-process events.
- On reconnect to cloud: replay local op log to remote (becoming an
  interesting peer-with-a-lot-of-history), fetch remote ops, apply
  both. Replicache-style.

**b. Local-first multiplayer (a kind of edge/proxy mode).**
- Each client runs a local kyoso_server.
- Local servers gossip with each other and/or with a cloud server.
- The cloud server is just one peer in the mesh, not the source of
  truth.
- Solves: latency-sensitive collaborative work (e.g., two designers
  in the same room over a slow link both feel local responsiveness).
- Requires: distributed `GlobalSeq` (Lamport / hybrid logical
  clocks) instead of server-stamped seq. **This breaks the
  current protocol simplification** that makes server-mediated total
  order easy.

**Tentative position:**
- (a) is a future extension, modest effort, doesn't fork the
  protocol. Worth keeping the door open for.
- (b) is a much bigger undertaking and a different system.
  Considered, deferred indefinitely.

## Multi-region — deferred

Current scope is single-region. Multi-region introduces:
- Cross-region latency on every Submit → bad for writer p99.
- Cross-region Postgres replication conflicts on the op log.
- Geographic data residency concerns for some users.

Two paths to multi-region eventually:
- **Per-room region pinning**: every room lives in exactly one
  region. Users from other regions take the latency hit. Simple to
  implement, bad UX for distributed teams.
- **Multi-region active-active**: room state replicated across
  regions; writes resolve via CRDT. Requires the Lamport-clock work
  from local-first multiplayer above.

**Defer until there's a real product reason.** We're nowhere near
needing it.

## Alternatives we considered

| Option | What it is | Why not (yet) |
|---|---|---|
| **Pure peer-to-peer** (Yjs/Automerge style) | No central server, peers gossip directly | Loses server-mediated total order; harder reconnect; NAT traversal; conflicts with multi-model auth model |
| **SpacetimeDB** | DB IS the multiplayer server; reducers run inside the DB | Replaces our entire backend. Multi-month rewrite of CRDT, sync, model handlers. Considered; rejected |
| **Convex** | Hosted reactive DB with subscriptions | Same shape as SpacetimeDB; SaaS-only; same rewrite cost |
| **Edge function / serverless** for kyoso_server | Stateless servers that re-hydrate from DB on each WS connection | The per-room in-memory mirror is a 100ms-1s cold start. WS is long-lived; cold start tax is amortized — but some serverless platforms don't support long-lived connections well. Could work; not a good fit for our architecture |
| **Multi-tier architecture** (gateway + room nodes + DB nodes) | Standard "split routing from logic from storage" | Premature. One axum process per replica is plenty until we have evidence we need to split routing from compute. |

## Implementation plan

There isn't really one — topology is the *consequence* of the
storage / auth / compute / worker designs, not its own deliverable.
What matters for the implementation roadmap:

- Slice 5.A — explicit deployment doc + docker-compose for local
  dev. Postgres + MinIO + kyoso_server + 1 worker. Should be one
  command to bring up.
- Slice 5.B — production deployment story (terraform / helm /
  whatever). Out of scope for the engineering plan; product/ops
  decision when we have a real customer.

## OPEN decisions

1. **Sticky room routing scheme.**
   - Hash room_id → replica index = simple but terrible on
     replica add/remove (consistent hashing helps).
   - External coordinator (Redis) tracks "room R is served by
     replica X" = more moving parts but recovers cleanly.
   - **Lean: hash with consistent hashing (rendezvous), no
     coordinator. Re-binding on replica change is fine because each
     room boots fresh from Postgres.**
2. **Replica count for v1.** One. Single replica is fine for the
   measured workload. Add a second when we need HA or hit per-replica
   limits.
3. **Postgres connection pooling.** PgBouncer in front of primary or
   per-app connection pool? Per-app is simpler; PgBouncer matters
   when you have many app processes or short-lived connections.
   Probably per-app for v1 (deadpool or sqlx pool).
4. **Local worker installation UX.** If we want users to run local
   inference workers, how do they install them? Auto-detected
   companion app? Manual download? Browser-extension model? **Big
   product question, defer to product team.**
5. **Region selection.** US-east, EU-west? Whichever is cheapest /
   closest to early users. Not a technical decision.
