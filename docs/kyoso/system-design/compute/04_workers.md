# Worker pools

_Status: draft. This doc contains the architectural answer to your
"how do workers join rooms?" question._

## Workers are NOT room participants

The naive design is "a worker is a peer that joins each room it
serves". That falls over immediately:

- 1000 rooms × 5 worker processes = 5000 connections, most idle.
- The worker has no useful state to maintain across the room's
  lifetime — it's stateless compute.
- Routing a job to "the worker that's in this room" is harder than
  routing it to "any worker that can handle this kind".

**Workers are pull-based queue consumers + opportunistic submitters.**
They don't subscribe to rooms; they pull jobs whose payload tells
them everything they need (inputs, target room, target node), do
the work, and submit a single result op.

```
                ┌────────────┐
                │  Postgres  │
                │  jobs      │
                │  table     │
                └─────┬──────┘
   enqueue(kind,      │
   payload, target)   │      dequeue(kinds=[…])
                      │      FOR UPDATE SKIP LOCKED
   ┌──────────────┐   │           ┌────────────┐
   │ Orchestrator │ ──┴── pulls ─►│  Worker    │
   │ (per room,   │               │  process   │
   │  server-side)│               │  (any      │
   └──────────────┘               │   number)  │
                                  └─────┬──────┘
                                        │ does the work,
                                        │ then ONE Submit
                                        │ via WS to write
                                        │ the result back
                                        ▼
                                ┌────────────────┐
                                │  kyoso_server  │
                                │  (graph handler│
                                │   accepts      │
                                │   service-     │
                                │   account op)  │
                                └────────────────┘
```

The worker has **two long-lived connections**: a Postgres connection
(for the queue) and a WS connection (for result submission). Neither
is per-room. A pool of 5 workers can serve 10,000 rooms.

## Worker lifecycle

```
1. start
2. authenticate as service account → JWT
3. open WS to kyoso_server, send Hello {tier: ReadWrite}
   - server records identity = Service { account_id, capabilities }
4. loop:
   a. dequeue (Postgres SELECT … FOR UPDATE SKIP LOCKED LIMIT 1
      WHERE kind IN [my capabilities] AND status = 'pending')
      with a 5min lock
   b. if no job: backoff (LISTEN/NOTIFY for wake-up if available)
   c. if job: spawn handler task to do the work
      - resolve inputs (may fetch from S3 / Postgres / RPC)
      - run stage.execute(inputs)
      - on success: submit SetNodeProperty op via WS, mark job complete
      - on failure: mark job failed (retryable or terminal)
      - extend lock periodically (heartbeat) for long jobs
5. on signal: drain in-flight jobs, close cleanly
```

The WS connection is **not** subscribing to broadcasts — the worker
is never a *consumer* of room state changes. It only ever writes,
and only the result of a job it just executed. The Submit travels via
the existing envelope; the graph handler's `allows_submit` recognizes
the service identity and accepts the write to `compute_*` paths.

**This means workers don't get the coalesced `ApplyBatch` traffic.**
Their writer task is essentially write-only. We could optimize this
(server detects workers and skips the broadcast subscribe step) but
v1 just lets it sit idle.

## Worker classes

Three classes mapped to the three workload classes from
[03_compute.md](03_compute.md):

| Class | Process | Capabilities | Where deployed |
|---|---|---|---|
| **In-process** | None — runs inside the orchestrator | All `Dispatch::InProcess` stages | The kyoso_server itself |
| **Pool worker** | `kyoso_worker` binary | Stage list configurable per deployment | Cluster of N worker pods, autoscaled by queue depth |
| **Inference worker** | `kyoso_inference_worker` binary | One stage per process (model is loaded at startup) | GPU-equipped nodes, manually sized |

Each class has its own scaling story:

- **In-process** scales by adding kyoso_server replicas. Each replica
  serves rooms independently. Limited to stages that fit in the
  server's CPU/memory budget.
- **Pool workers** scale independently by autoscaler watching queue
  depth (`kyoso_worker` exposes a Prometheus metric for each
  capability). Add more pods when queue p95 latency drifts up.
- **Inference workers** scale by capability — `sam3_segment` workers
  scale separately from `text_embed` workers. Manual sizing because
  GPU costs.

## Local-on-client workers

The killer feature. If the user's machine has the resources to run a
stage locally, route to it instead of a remote worker. Two parts:

**1. Worker discovery / registration.** Worker process registers with
the server on startup, advertising:
- Capabilities (which stages it can run)
- Locality hint (`local-only`, `prefer-local`, `pool`)
- Address (where to dispatch RPC, or "use the queue")

```sql
CREATE TABLE worker_registrations (
    id              uuid PRIMARY KEY,
    account_id      uuid REFERENCES service_accounts(id),
    user_id         uuid REFERENCES users(id),    -- NULL for pool workers
    locality        text NOT NULL,                 -- 'local'|'pool'|'inference'
    capabilities    text[] NOT NULL,
    rpc_endpoint    text,                          -- NULL for pure queue consumers
    last_heartbeat  timestamptz NOT NULL DEFAULT now()
);
```

A `local` worker is associated with a specific user. The orchestrator,
when dispatching a job for that user, prefers the user's local worker
if available; falls back to pool workers if not.

**2. Routing.** The orchestrator's dispatch decision becomes:

```
for stage = job.stage_kind:
    if stage in user's local worker capabilities:
        dispatch via direct RPC to local worker         (lowest latency)
    elif stage in pool worker capabilities:
        enqueue on Postgres queue                        (workhorse path)
    elif stage in inference worker capabilities:
        dispatch via inference RPC                        (long-lived ML)
```

**Why this matters:**
- A designer with an M3 Pro can run SAM3 locally — zero network
  latency, no GPU bill, results stay on their machine.
- A user on a phone falls back to pool workers automatically.
- The same `Stage` trait powers both, just with different dispatchers
  configured per-deployment.

This is conceptually the same as what game engines do for "client-side
prediction" but for compute, not for input. Local first, server-backed
fallback.

## Local server in-process — separate question

Different question from local workers, lumped here because both touch
"running stuff on the user's machine":

**Should kyoso_server itself be embeddable in the client process for
single-user / offline mode?**

Tentative answer: **yes, but later.** The architecture allows it
because:
- `kyoso_server::AppState::in_memory()` already exists (used by
  loadgen + tests).
- The handlers don't depend on a network — they consume `Op<K>`s.
- Bevy can embed an axum server in a tokio runtime alongside the ECS
  app.

Single-user offline mode would:
- Run kyoso_server in-process on the client.
- Persist op log to local SQLite (alternative `OpStore` impl).
- Sync to cloud server when online (Replicache-style — replay local
  log to remote, fetch remote ops, apply both).

This is **not v1 scope** but the trait abstractions in
[01_storage.md](01_storage.md) make it cheap to add later. We avoid
making decisions now that would close it off.

See [05_topology.md](05_topology.md) for the broader topology
discussion.

## Authentication for workers

Service-account JWTs (see [02_authorization.md](02_authorization.md)):
- Each worker process has a service account with declared
  capabilities.
- Token is signed by the auth service at deploy time (or fetched at
  startup from a secret store).
- Server's `allows_submit` check sees the service identity and
  permits writes to `compute_*` property paths only.

Local workers running on the user's machine use a different auth flow:
- The local worker is a child of the user's session — it inherits the
  user's identity for the purposes of "which rooms can I write to"
  but has the service-account *capability set* for "which ops can I
  submit".
- This requires a small extension to the JWT claim shape: a token
  that says "I am user U, AND I have service capabilities S".
- Defer this until the local-worker path actually ships.

## Recovery & fault tolerance

**Orphaned jobs.** Worker dies mid-job. The lock expires (5min
default), another worker picks the job up. Idempotency guaranteed
because compute output is LWW — a stale write loses to a fresher one.

**Result-write failure.** Worker computes but the WS submit fails
(network blip). Worker retries N times with backoff; if all fail,
marks the job `failed (retryable)`. Another worker picks it up,
recomputes, submits.

**Cache pollution from buggy stages.** Stage v1 has a bug, produces
wrong output, output gets cached. Fix: bump stage version (cache key
includes version → automatic miss).

**Worker process panic** in the stage handler. Caught by tokio's
spawn boundary; the job lock expires; another worker retries. Bug
gets fixed via stage version bump.

## Implementation plan

**Slice 4.A** — `kyoso_worker` binary skeleton. Postgres queue
consumer with `SELECT FOR UPDATE SKIP LOCKED`. Heartbeat to extend
lock. Single trivial stage (`thumbnail_image`).

**Slice 4.B** — WS submit path. Worker connects with service-account
JWT; submits result via `SetNodeProperty`. Service identity threaded
through `allows_submit` (depends on slice 2.C).

**Slice 4.C** — orchestrator wiring (depends on slices 3.B-D).
Orchestrator enqueues jobs targeted at this worker class; result
arrives back via the existing handler op-stream hook; node's
`compute_status` flips to `Clean`.

**Slice 4.D** — `kyoso_inference_worker` binary. Same skeleton as
4.A but holds a model in memory; one stage per process. Optional
RPC dispatch instead of queue (queue is fine for v1 even for ML).

**Slice 4.E** — local worker path. `worker_registrations` table +
discovery hook in the orchestrator. Local-worker RPC transport. UX
for "is the local worker running, do I need to install it?".

Slices A-C are ~1 week (gated on storage + authz). Slice D is ~3
days. Slice E is exploratory; could be days or weeks depending on
the install-and-discover UX we want.

## OPEN decisions

1. **Queue substrate: Postgres SKIP LOCKED, or something fancier?**
   - PG SKIP LOCKED handles ~10k jobs/sec with ease. Plenty for v1.
   - NATS JetStream / Redis Streams / Kafka are options if we ever
     hit the limit. Trait abstraction means swap is a 1-week job.
   - **Lean: PG for v1.**
2. **Worker discovery: static config or dynamic registration?**
   - Static config = `kyoso_server.toml` lists worker pools. Simple.
     Doesn't handle local workers well (they come and go).
   - Dynamic registration via Postgres table with heartbeat. Handles
     local + pool workers uniformly. More moving parts.
   - **Lean: dynamic registration. Static config is just a special
     case (workers register themselves at deploy time).**
3. **Worker submit path: regular WS Submit or a dedicated HTTP
   endpoint?**
   - WS Submit means workers use the same protocol as clients —
     simple. Adds a long-lived WS per worker.
   - HTTP `POST /rooms/:room/submit` would let workers be
     stateless-stateless. Slightly different code path.
   - **Lean: WS for protocol uniformity. Workers don't care about the
     WS connection cost; it's negligible.**
4. **Heartbeat / lock-extension granularity.** 30s default? 60s?
   Affects worst-case orphan recovery time. Tunable, not load-
   bearing.
5. **Per-tenant worker isolation.** If we run a SaaS with multiple
   orgs, a worker doing org A's compute shouldn't see org B's data.
   Trivial if jobs are pure functions of their inputs (worker pulls
   only what its job payload references) but worth being explicit
   about.
