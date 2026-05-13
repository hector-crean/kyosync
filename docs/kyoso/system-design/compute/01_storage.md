# Storage layer

_Status: draft. Decisions marked **OPEN** still need confirmation._

## What we persist

| Data class | Access pattern | Volume estimate | Where |
|---|---|---|---|
| **Op log** (per-`(room, model)`) | Append-only; range scan from `since` to `head` | High write rate at room peak (~100s ops/sec/room), TBs over time | Postgres, partitioned table |
| **Snapshots** (per-`(room, model)`) | Latest-per-key reads on Welcome; periodic background writes | KB–MB per snapshot, one per room/model | Postgres `bytea` for ≤MB; object store for >MB |
| **Ack table** (per-`(room, model, peer)`) | Read on GC, write on every Ping | Tiny rows, many of them | Postgres |
| **Auth** (users, rooms, members, tokens) | Read on every Hello (cached) | Modest | Postgres |
| **Job queue** | Hot dequeue with `SKIP LOCKED`; modest enqueue | Bursty | Postgres |
| **Compute cache** (content-addressed) | Read-after-hash; very-write-heavy on cold start | Variable; bounded by retention | Postgres for small; object store for large |

Two data substrates: Postgres for transactional + queryable state,
object store (S3 / local FS) for blobs whose size makes Postgres
inefficient. **One of each, no more.**

## Why Postgres for v1

Detailed comparison in [00_overview.md](00_overview.md). Short version:

- We measured the workload (HARNESS.md layer 4c). Even at
  10 ops/sec/writer × 256 writers = 2560 ops/sec aggregate, a single
  Postgres instance handles this with room to spare. We're not at the
  scale that justifies DynamoDB / FoundationDB / etc.
- Workers (job queue) and authz (relational) have natural Postgres
  fits that NoSQL alternatives weaken.
- One backup story, one schema-migration tool (sqlx-migrate / sea-orm
  / etc.), one connection pool to size.
- Trait-based abstraction (`OpStore` + `SnapshotStore` etc.) means
  swapping out individual layers is a 1-week job, not a rewrite. We
  can move the op log to Kafka without touching auth.

## Schema

```sql
-- =====================================================================
-- Op log: append-only, dense seq per (room, model).
-- =====================================================================
CREATE TABLE op_log (
    room        text       NOT NULL,
    model       text       NOT NULL,
    seq         bigint     NOT NULL,
    op_id_peer  integer    NOT NULL,
    op_id_seq   bigint     NOT NULL,
    payload     bytea      NOT NULL,        -- postcard-encoded Op<K_M>
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (room, model, seq)
) PARTITION BY HASH (room);

-- 16 hash partitions to start. Pick a higher number than you think
-- you need; can't easily change later. Each partition is a separate
-- physical table — vacuum, indexing, replication scope is per-part.
CREATE TABLE op_log_p0 PARTITION OF op_log FOR VALUES WITH (modulus 16, remainder 0);
-- ... (15 more)

-- Op id is the original CrdtId from the producing peer (kept for
-- idempotency; replayed Submits with the same op_id should no-op).
CREATE UNIQUE INDEX op_log_idem ON op_log (room, model, op_id_peer, op_id_seq);

-- =====================================================================
-- Snapshots: latest-per-(room, model). Old ones GC'd after a window.
-- =====================================================================
CREATE TABLE snapshots (
    room        text       NOT NULL,
    model       text       NOT NULL,
    at_seq      bigint     NOT NULL,
    payload     bytea,                       -- inline if small; NULL + s3_key otherwise
    s3_key      text,                        -- pointer for >1MB blobs
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (room, model, at_seq),
    CHECK ((payload IS NOT NULL) <> (s3_key IS NOT NULL))
);

CREATE INDEX snapshots_latest ON snapshots (room, model, at_seq DESC);

-- =====================================================================
-- Per-peer acks: used for compaction safety.
-- =====================================================================
CREATE TABLE acks (
    room          text       NOT NULL,
    model         text       NOT NULL,
    peer          integer    NOT NULL,
    applied_seq   bigint     NOT NULL,
    last_seen_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (room, model, peer)
);

-- =====================================================================
-- Auth.
-- =====================================================================
CREATE TABLE users (
    id           uuid       PRIMARY KEY,
    email        text       UNIQUE NOT NULL,
    display_name text       NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE rooms (
    id          text       PRIMARY KEY,    -- matches RoomId on the wire
    org_id      uuid       NOT NULL,        -- for permission scoping
    created_by  uuid       REFERENCES users(id),
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TYPE room_role AS ENUM ('owner', 'editor', 'commenter', 'viewer');

CREATE TABLE room_members (
    room_id   text       NOT NULL REFERENCES rooms(id),
    user_id   uuid       NOT NULL REFERENCES users(id),
    role      room_role  NOT NULL,
    PRIMARY KEY (room_id, user_id)
);

-- Service accounts: identity for worker processes. Authz code
-- recognizes these and may grant cross-room permissions.
CREATE TABLE service_accounts (
    id         uuid       PRIMARY KEY,
    name       text       NOT NULL UNIQUE,    -- "kyoso-inference-worker"
    capabilities text[]   NOT NULL,           -- e.g. ['compute.image_segment']
    created_at timestamptz NOT NULL DEFAULT now()
);

-- =====================================================================
-- Job queue: SKIP LOCKED dequeue.
-- =====================================================================
CREATE TYPE job_status AS ENUM ('pending', 'running', 'succeeded', 'failed');

CREATE TABLE jobs (
    id              uuid       PRIMARY KEY DEFAULT gen_random_uuid(),
    kind            text       NOT NULL,        -- "stitch_video", "image_segment", ...
    payload         jsonb      NOT NULL,
    status          job_status NOT NULL DEFAULT 'pending',
    created_at      timestamptz NOT NULL DEFAULT now(),
    started_at      timestamptz,
    finished_at     timestamptz,
    locked_by       text,                       -- worker id
    lock_expires_at timestamptz,
    last_error      text,
    attempts        integer    NOT NULL DEFAULT 0,
    -- Routing: which room the result goes back into.
    room_id         text       REFERENCES rooms(id),
    target_node     text                          -- compute node id
);

CREATE INDEX jobs_pending ON jobs (kind, created_at)
    WHERE status = 'pending';
CREATE INDEX jobs_recover ON jobs (lock_expires_at)
    WHERE status = 'running';

-- =====================================================================
-- Compute cache: content-addressed.
-- =====================================================================
CREATE TABLE compute_cache (
    cache_key   bytea      PRIMARY KEY,    -- H(stage_id, version, config, input_hash)
    stage_id    text       NOT NULL,
    payload     bytea,                      -- inline if small
    s3_key      text,                       -- pointer for large
    output_kind text       NOT NULL,        -- mime-ish: "arrow.recordbatch", "image/png", ...
    created_at  timestamptz NOT NULL DEFAULT now(),
    accessed_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((payload IS NOT NULL) <> (s3_key IS NOT NULL))
);

CREATE INDEX compute_cache_lru ON compute_cache (accessed_at);
```

## Trait abstractions

The handler holds traits, not concrete `InMemoryOpLog`s. Lets us
unit-test against in-memory while running production against
Postgres, and lets us swap the op-log substrate later without
touching handler code.

```rust
// kyoso_storage crate (new)

#[async_trait]
pub trait OpStore: Send + Sync + 'static {
    async fn append(&self, payload: &[u8], op_id: CrdtId) -> Result<GlobalSeq>;
    async fn slice(&self, since: GlobalSeq, head: GlobalSeq) -> Result<Vec<Vec<u8>>>;
    async fn head(&self) -> Result<GlobalSeq>;
    /// GC ops below `up_to` (called after a snapshot covers them).
    async fn truncate_below(&self, up_to: GlobalSeq) -> Result<u64>;
}

#[async_trait]
pub trait SnapshotStore: Send + Sync + 'static {
    async fn put(&self, at_seq: GlobalSeq, payload: &[u8]) -> Result<()>;
    async fn latest(&self) -> Result<Option<(GlobalSeq, Vec<u8>)>>;
    async fn gc_below(&self, threshold: GlobalSeq) -> Result<()>;
}

#[async_trait]
pub trait AckStore: Send + Sync + 'static {
    async fn record(&self, peer: PeerId, applied_seq: GlobalSeq) -> Result<()>;
    async fn release(&self, peer: PeerId) -> Result<()>;
    async fn min_ack(&self) -> Result<GlobalSeq>;
}

#[async_trait]
pub trait JobQueue: Send + Sync + 'static {
    async fn enqueue(&self, kind: &str, payload: serde_json::Value, target: JobTarget) -> Result<Uuid>;
    async fn dequeue(&self, kinds: &[&str], lock_for: Duration) -> Result<Option<Job>>;
    async fn complete(&self, job_id: Uuid, output_kind: &str, output: &[u8]) -> Result<()>;
    async fn fail(&self, job_id: Uuid, error: &str, retryable: bool) -> Result<()>;
    async fn extend_lock(&self, job_id: Uuid, lock_for: Duration) -> Result<()>;
}

#[async_trait]
pub trait ComputeCache: Send + Sync + 'static {
    async fn get(&self, key: &[u8]) -> Result<Option<CacheEntry>>;
    async fn put(&self, key: &[u8], stage_id: &str, output_kind: &str, payload: &[u8]) -> Result<()>;
    async fn touch(&self, key: &[u8]) -> Result<()>;     // LRU bookkeeping
}
```

Each handler is constructed with all of these traits as `Arc<dyn …>`.
The `HandlerFactory` produces a handler holding the right impls;
`AppState::new_in_memory()` wires in-memory impls, `AppState::new_postgres(pool)`
wires Postgres impls. Tests use in-memory, prod uses Postgres.

## Implementation plan

**Slice 1.A** — extract storage traits into `kyoso_storage`. Lift
existing `InMemoryOpLog` into `InMemoryOpStore` (and the rest). All
existing tests should pass without touching them.

**Slice 1.B** — write `PostgresOpStore` + `PostgresSnapshotStore` +
`PostgresAckStore`. Use `sqlx` (battle-tested) or `tokio-postgres` +
`deadpool` if we want lower-level control. Wire via `AppState`
constructor variants.

**Slice 1.C** — testcontainers integration in CI: every reconnect
test + chaos sim runs against a real Postgres in a container.

**Slice 1.D** — `compute_cache` + `jobs` schema and traits land in
preparation for compute work.

**Slice 1.E** — auth schema (`users`, `rooms`, `room_members`,
`service_accounts`) lands. Empty tables; the actual auth logic is
[02_authorization.md](02_authorization.md).

Each slice is a PR-sized chunk. 1.A through 1.D should land in
~1 week; 1.E is tiny.

**Unblocks: room eviction.** Once `PostgresOpStore` lands, dropping
a `Room` from the in-memory registry is safe — the data lives in
Postgres and `Room::restore` rebuilds the mirror on next access.
Until then, eviction = data loss. The room-lifecycle work
(documented in
[05_topology.md § Room lifecycle](05_topology.md#room-lifecycle--hydrate-on-join-evict-on-leave))
is gated on slice 1.B specifically.

## OPEN decisions

1. **Cache scope: per-room, per-org, global, or content-addressed-global?**
   - Pure content-addressed = global cache. Same prompt + model →
     same output, regardless of room. Maximally efficient but might
     need a per-org sub-cache for "private" outputs (don't reuse a
     competitor's image).
   - Per-org probably right default. Add per-room only if some
     outputs MUST not leak between rooms in same org (rare).
   - **Tentative: global content-addressed cache + per-org
     sub-cache, marked at insert time by the stage.**
2. **Object store choice for v1.**
   - S3 in production. For local dev: minio in docker-compose.
   - Should the trait abstract over both, or just point at
     `s3://...` URLs and let env vars select the backend?
3. **Op-log retention policy.** With snapshots covering history, do
   we ever need the raw op log past N days? Probably yes for audit /
   replay / debugging. Default retention 90 days is a reasonable
   start; configurable per-room if some are short-lived (ephemeral
   experiments) vs durable (real projects).
4. **Schema migrations: `sqlx-migrate`, `refinery`, plain SQL
   files?** Probably `sqlx-migrate` to match the query layer; minor
   choice.
