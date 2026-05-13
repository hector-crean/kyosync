# Compute layer

_Status: draft. Lifts heavily from rabona's
[`flow` crate design](../../../workspaces/rabona/docs/architecture/04_pipeline_dag.md);
read that first._

## Workload classes

Three distinct shapes, with three latency budgets and three dispatch
modes:

| Class | Latency | Frequency | Where it runs | Examples |
|---|---|---|---|---|
| **Real-time** | sub-1ms p99 | every CRDT op that touches inputs | in-process on server | bounding box of subtree, simple aggregation, layout |
| **Heavy CPU** | seconds–minutes | bursty, debounced | Postgres queue → worker process | video stitching, image resize, Parquet export |
| **ML inference** | 10ms–1s | bursty per-user | RPC to long-lived inference worker | image segmentation (SAM3), text→image, embedding |

These map directly onto rabona's three dispatch modes
(`InProcessDispatch`, `QueueDispatch`, `InferenceRpcDispatch`) — same
`flow::Dispatch` trait, different impls.

## Where the boundary is

**Reuse the rabona `flow` kernel.** Lift it into kyoso as
`kyoso_compute` (or import directly when both repos share a workspace
boundary later). The kernel is already domain-agnostic — it knows
about stages, ports, dispatch, registries, but nothing about
matches, vision, graphs, or comments.

**kyoso-specific glue lives in a new orchestrator** (per-room tokio
task) that:

1. Subscribes to the graph handler's stamped-op stream.
2. Walks the user's compute sub-DAG to find what's dirty.
3. Debounces per-stage settle delay.
4. Resolves cache hits.
5. Dispatches misses through `flow::Dispatch`.
6. Receives results, publishes them as `SetNodeProperty` ops.

The orchestrator is the new code. Everything inside the dispatch is
already designed in rabona.

## Compute nodes in the CRDT — user-visible, Weave-style

A **compute node** is a regular graph node with a special
`compute_kind` property. Edges from "input" nodes to the compute
node are regular CRDT edges with `category: ComputeInput`. The user
authors the compute graph the same way they author any other graph
— drag-onto-canvas, wire ports together — and the orchestrator
infers everything from the CRDT state.

Compute-node properties (server-managed, server-only writable):
- `compute_status: enum { Clean, Dirty, Computing { job_id }, Failed { reason } }`
- `compute_output: any` (the result, kind depends on stage)
- `compute_input_hash: bytes` (last-known-good input fingerprint)
- `compute_output_kind: string` (mime-ish, mirrors cache schema)

User-managed properties:
- `compute_kind: string` (`"image_segment"`, `"thumbnail"`, ...)
- `compute_config: json` (stage-specific config)
- Edges from input nodes (`category: ComputeInput`)

Authz (see [02_authorization.md](02_authorization.md)): users can
write the user-managed props and edges; service-account workers can
write the server-managed props. Enforced by the graph handler's
`allows_submit` check on path prefixes.

**Why user-visible?** Same reason rabona has visible pipeline-as-data
and Weave shows visible compute nodes: the compute graph is the
user's mental model. Hiding it would mean the user can't see why
something is recomputing or what the dependency chain is.

## Dirty propagation algorithm

```
on stamped_op(op):                          # called by graph handler hook
    for each affected_node in op.touches():
        for each downstream in compute_dependents(affected_node):
            mark downstream dirty (in orchestrator state)
            schedule_recompute(downstream)

schedule_recompute(node):
    cancel any pending timer for node
    spawn timer:
        wait stage(node).debounce_ms        # 50ms / 250ms / 500ms
        if node still dirty:
            input_hash = hash_inputs(node)
            cache_key = stage.cache_key(input_hash, node.compute_config)
            if cache.has(cache_key):
                publish_output(node, cache.get(cache_key))    # instant
            else:
                dispatch(node, stage, inputs)                  # async
                # on completion: cache.put + publish_output

publish_output(node, output):
    op = Op::new(SetNodeProperty {
        target: node.id,
        path: ["compute_output"],
        delta: LwwReplace { value: output },
    });
    submit_via_service_account(op);          # graph handler's normal Submit path
```

**Key properties:**

- **Idempotent**. Re-running on the same dirty set produces the same
  result. Safe under crashes, retries, restarts.
- **Debounced**. Drag a slider 60Hz → cancel-cancel-cancel-execute.
  No queue thrash, no wasted work.
- **Cache-first**. Content-addressed key includes stage version + config
  + input hash. Pure functions get instant replays for free.
- **Server-internal state**. Dirty bits don't replicate; they're
  derived from the current graph snapshot at restart.
- **Conflict-free**. Output writes are LWW; if two replicas of the
  orchestrator (we don't have those today, but planning ahead) both
  publish, the later one wins.

## Cancellation & race conditions

**Stale outputs** — node gets dirtied while a job is in flight, the
job finishes, writes a stale result. Then the orchestrator notices the
node is still dirty (input changed during compute), schedules another
job. The second job's output supersedes the first by LWW seq. Some
worker waste, no correctness issue. **OK for v1.**

**Cooperative cancellation (v2)** — for expensive ML jobs, the
orchestrator marks "this job is no longer wanted" in the `jobs`
table. The worker periodically checks and aborts. Saves GPU time at
the cost of slightly more queue traffic.

**Coordinator restart mid-flight** — a job is in `running` state with
no orchestrator listening. The worker still finishes and submits the
result. New orchestrator on restart walks the graph, finds the
`compute_status: Computing { job_id }`, looks the job up in the queue,
and either adopts it or waits for the worker to finish.

## Real-time sub-millisecond compute

The first workload class is interesting because it doesn't go
through the queue at all. The orchestrator runs the stage inline:

```rust
async fn schedule_recompute(&mut self, node_id: CrdtId) {
    let stage = self.registry.get(node.compute_kind).unwrap();
    if stage.requirements().dispatch == Dispatch::InProcess {
        // Run synchronously inside the orchestrator's tokio task.
        let inputs = self.gather_inputs(node_id).await;
        let output = stage.execute(inputs, &self.ctx).await?;
        self.publish_output(node_id, output).await;
    } else {
        // Queue / RPC path — debounce, hash, cache check, dispatch.
        self.queue_recompute(node_id).await;
    }
}
```

For sub-millisecond stages there's no point debouncing; the work is
cheap. We trade away some "merge multiple updates into one" benefit
to keep the latency floor at the actual compute cost. Per-stage
config can opt back into debouncing if it wants.

## Caching

Content-addressed key:

```rust
fn cache_key(stage: &dyn Stage, config: &serde_json::Value, inputs: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(stage.id().as_bytes());
    h.update(stage.version().to_string().as_bytes());
    h.update(&serde_canonical::to_vec(config).unwrap());
    h.update(inputs);
    h.finalize().into()
}
```

Schema in [01_storage.md](01_storage.md). Key things:
- **Inline payload** in Postgres for ≤MB outputs (most graph
  computations).
- **Object-store pointer** for >MB outputs (images, video frames).
- **LRU eviction** based on `accessed_at`. GC is a periodic background
  task; cache is bounded by configured size.
- **Per-stage cache control**: a non-deterministic stage (ML with
  random sampling) sets `Stage::deterministic() -> false` and bypasses
  the cache.

## Compute as its own CRDT model? No.

Considered: a separate `compute` model where outputs land, parallel
to the `graph` model. Rejected because:
- Outputs are tightly coupled to the user's graph (they're properties
  of compute nodes the user sees and selects). Splitting them across
  models means the client has to do cross-model joins for rendering.
- The graph model already supports per-property updates via
  `SetNodeProperty + WireDelta::LwwReplace`. No new wire shape needed.
- Authz already gates per-property writes — service accounts can
  write `compute_*` paths but not user-editable ones. Clean.

So compute outputs ride the existing graph model. Workers submit
ordinary `Op<OpKind>::SetNodeProperty` ops; clients see them via the
existing tiered broadcast.

## Implementation plan

**Slice 3.A** — adapt the rabona `flow` kernel into `kyoso_compute`.
Lift `Stage` trait, registry, `Port` (Arrow schemas), `Dispatch` trait,
`InProcessDispatch`. Cross-domain test that registers a dummy stage.

**Slice 3.B** — orchestrator skeleton. Per-room tokio task
subscribing to the graph handler's stamped-op stream; in-memory dirty
set; no debouncing yet. One trivial in-process stage
(`bounding_box_of_subtree`) that runs every time inputs change and
writes back via SetNodeProperty.

**Slice 3.C** — content-addressed cache integration
(`PostgresComputeCache`). Hash key derivation, hit-path early-return.
Verify cache hits show up in tracing.

**Slice 3.D** — debouncing. Per-stage `debounce_ms` config; pending-timer
cancellation. Loadgen test that drags a node 60Hz and confirms we see
~4 actual compute runs/sec at 250ms debounce.

**Slice 3.E** — queue dispatch end-to-end (depends on
[04_workers.md](04_workers.md) slices). One heavy stage
(`thumbnail_image`) that runs in a worker process. Validates the full
loop: orchestrator enqueues → worker dequeues → worker computes →
worker submits `SetNodeProperty` → orchestrator marks node clean.

Slices A-D are ~2 weeks. Slice E is gated on workers being ready.

## OPEN decisions

1. **Compute-node user visibility — confirm Weave-style.** Above I
   recommend visible compute nodes. Alternative: hidden compute
   triggered by special node kinds. Visible is more flexible
   (user-authored pipelines like Figma Weave) but adds UI surface.
   **Lean: visible. Confirm.**
2. **Per-stage cache TTL vs LRU vs both.** LRU is simplest;
   TTL is useful for non-deterministic outputs we still want to
   cache briefly. Probably both, configurable per-stage.
3. **Cross-room cache sharing.** Pure content-addressed cache is
   inherently shareable across rooms (and even orgs, if outputs are
   public). Per-org sub-cache for "private" outputs. See
   [01_storage.md](01_storage.md) open Q.
4. **Cancellation v1 or v2.** Above says v1 lets stale outputs land
   (idempotent, LWW). Probably right. Revisit if ML waste is real.
5. **Reactive vs polled dirty propagation.** Push (handler hook) is
   simpler and lower-latency than poll. Above assumes push. No
   alternative considered seriously.
