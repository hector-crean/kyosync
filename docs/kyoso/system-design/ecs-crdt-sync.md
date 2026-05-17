# ECS ↔ CRDT sync — current architecture

> **Status:** current. Reflects what's wired in the workspace as of
> commit `2f7ec0b` (post the Backend/Topology unification + OpaqueRecord
> + GraphView + reconnect-clobber fixes). Supersedes the `0acc1e8`-era
> §3–§4 of [`crdt-overview.md`](crdt-overview.md) where they disagree.
> If you only have time to read one architecture doc, read this one and
> dip into `crdt-overview.md` for the substrate (`kyoso_crdt`),
> primitives, and worked composition examples.
>
> **Audience:** someone about to (a) add replicated state to a kyoso
> app, (b) debug a "why didn't this op show up?" bug, or (c) understand
> the order-of-operations and naming choices behind the four crates
> that meet at the ECS-CRDT boundary.

---

## 1 · The big picture in one paragraph

A peer holds **two parallel state stores** for every replicated entity:

1. A **CRDT engine** ([`ClientSyncEngine`](../../../crates/kyoso_graph_sync/src/engine.rs) on the client; [`GraphBackend<OpaqueRecord>`](../../../apps/kyoso_server/src/model.rs) on the server) — the canonical state. Stores topology (nodes, edges, tree shape) and, per peer-side typed schema, a separate [`SchemaDoc<S>`](../../../crates/kyoso_graph_sync/src/schema_sync.rs) resource holding the field-level CRDT state for one Bevy component type.

2. A **Bevy ECS world** — the *projection* of (1). The
   [`EntityCrdtIndex`](../../../crates/kyoso_graph_sync/src/index.rs)
   resource is the bidirectional `Entity ↔ CrdtId` map that lets the
   two sides talk.

Each Bevy frame runs a **four-phase pipeline** that keeps the two
stores in sync:

```
Inbound  →  Detection  →  Schema-sync chain (per typed component)  →  Outbound
   ↑                                                                       ↓
   └────────────────────────────── server WebSocket ──────────────────────┘
```

The CRDT is the source of truth. The ECS is a window onto it.
Locally-issued mutations don't take effect on screen until the server
has echoed them back — visibility waits one round-trip; the design
pays for it in latency to keep CRDT semantics simple (`apply` stays
idempotent, deterministic over `GlobalSeq` order).

---

## 2 · The crate cake

```
kyoso_crdt              ─────────────────────────────────────────────────
   Generic substrate: CrdtId, IdGen, Op, Backend<T, S>, Topology trait,
   primitive types (LwwRegister, OrSet, PnCounter, Sequence, CausalMap),
   the Schema*/Apply traits, OpaqueRecord, Path/PathSegment, Snapshot<T, S>.
   No Bevy. No transport. No domain knowledge.

kyoso_crdt_derive       ─────────────────────────────────────────────────
   #[derive(Crdt)]  — per-field codegen: Mutation/Delta enums, Lattice,
   Crdt, SchemaApply (apply_wire + install_state), IntoWireOp.

kyoso_graph_crdt        ─────────────────────────────────────────────────
   First Topology impl. GraphBackend<S> = thin wrapper over
   Backend<GraphTopology, S>. OpKind (AddNode/AddRefEdge/RemoveNode/
   RemoveRefEdge/Move/SetNodeProperty/SetRefEdgeProperty), EdgeCategory,
   GraphView trait + algorithms, structural invariants. No Bevy.

kyoso_graph             ─────────────────────────────────────────────────
   Pure Bevy ECS graph: components (EdgeFrom, EdgeTo, OutgoingEdges,
   IncomingEdges), tree (TreeParent, TreeEdge, OrderKey), queries
   (GraphQuery), commands (GraphCommand), event propagation
   (GraphMessage, GraphSystemSet, detect_*). Implements GraphView for
   its own EcsGraphView so the cycle-check algorithms come from one place.

kyoso_sync              ─────────────────────────────────────────────────
   Multi-model WebSocket transport. SyncTransportPlugin, WsBridge,
   WsInbound, SyncStatus, PeerIdGen, ModelRegistry, model envelopes.
   Doesn't know about graphs or schemas; carries opaque postcard bytes
   per model.

kyoso_graph_sync        ─────────────────────────────────────────────────
   The bridge. GraphSyncPlugin, ClientSyncEngine, EntityCrdtIndex,
   SchemaDoc, SchemaSync (derive + trait), SchemaTarget (NodeTarget /
   EdgeTarget), SchemaHydrators registry, RemoteOpApplied,
   project_snapshot/project_op/project_move, detect_added_*,
   detect_tree_moves, ensure_schema_slots / detect_typed_changes /
   route_typed_inbound / project_typed_to_bevy, outbound_system. All the
   per-frame plumbing lives here.

kyoso_sync_derive       ─────────────────────────────────────────────────
   #[derive(SchemaSync)] — generates the per-component Schema struct
   (e.g. ResistorSchema), the SchemaSync impl (changes_against /
   write_back), and routes per-field CRDT kinds (#[crdt(lww)],
   #[crdt(counter)], #[crdt(or_set)], …).
```

Three apps consume the stack:

- `apps/kyoso_server` — headless, runs `GraphBackend<OpaqueRecord>`
  per room. Stamps `GlobalSeq` under an append lock; takes periodic
  snapshots; compacts ops below `min_ack`; serves Welcome / Apply /
  ApplyBatch / Catchup / Ack frames.
- `apps/kyoso_client` (figma/weave 2D reference app), `apps/kyoso_circuit_client`
  (analogue-circuit 3D app) — Bevy apps that mount
  `GraphSyncPlugin<N, E>` and one `SchemaSyncedNodeComponentPlugin` per
  domain component type. `kyoso_loadgen` and `kyoso_scenarios` drive the
  same stack headlessly for chaos/replay/soak harnesses.

---

## 3 · The four phases of one frame

Per-frame system order inside Bevy's `Update` schedule, as configured
by [`GraphSyncPlugin::build`](../../../crates/kyoso_graph_sync/src/plugin.rs):

```
graph_inbound_system::<N, E>                                  ─ Phase 1
detect_added_nodes::<N, E>                                    ─ Phase 2
detect_added_edges::<N, E>
detect_tree_moves::<N, E>
detect_removed_nodes::<N, E>
detect_removed_edges::<N, E>
  ┌─────────────────────────────────────────┐
  │ Per SchemaSyncedNodeComponentPlugin<C>: │ ─ Phase 3
  │   ensure_schema_slots::<NodeTarget, C>  │
  │   route_typed_inbound::<NodeTarget, C>  │
  │   project_typed_to_bevy::<NodeTarget, C>│
  │   detect_typed_changes::<NodeTarget, C> │
  └─────────────────────────────────────────┘
outbound_system::<N, E>                                       ─ Phase 4
```

### Phase 1 — Inbound

[`graph_inbound_system`](../../../crates/kyoso_graph_sync/src/plugin.rs)
reads [`WsInbound`](../../../crates/kyoso_sync/src/lib.rs) events from
the transport plugin and dispatches by frame variant:

- **`Welcome { peer, models, … }`** — the room handshake. Decodes the
  server snapshot ([`ServerSnapshot = Snapshot<GraphTopology, OpaqueRecord>`](../../../crates/kyoso_graph_sync/src/engine.rs)),
  splits it: the structural half goes to `engine.restore(…)`; the
  typed-schema half (the `OpaqueRecord`-keyed `BTreeMap`) is
  handed to `commands.queue(hydrate_typed_schemas)`, which walks the
  `SchemaHydrators` registry and installs each opaque field into the
  matching `SchemaDoc<S>` resource. Then the **diff** (any ops above
  the snapshot's `at_seq`) is applied op-by-op via `apply_one`.
- **`ModelApply` / `ModelApplyBatch` / `ModelCatchup`** — individual
  stamped ops; each goes through `apply_one` (which calls
  `engine.apply_remote` then `project_op` / `project_move`).

For every op that lands successfully, the system writes a
`RemoteOpApplied(op)` Bevy message. That's the bridge that lets the
schema-sync plugins (running later in the same frame) react to property
ops without re-decoding the wire payload.

**Structural projection.** `project_op` does the ECS-side spawn /
despawn for structural ops:

- `AddNode` → `commands.spawn(N::default())`, bind in
  `EntityCrdtIndex`. The Bevy `Add` observer fires synchronously and
  any `#[require(...)]` components (Transform, Mesh3d, …) get inserted
  with `Default::default()` *at this point*. That auto-insert is the
  trigger for the reconnect-clobber bug fixed in
  [`schema_sync.rs::detect_typed_changes`](../../../crates/kyoso_graph_sync/src/schema_sync.rs) — see §6.
- `AddRefEdge { from, to, category }` → spawn an edge entity with
  `(EdgeFrom, EdgeTo, E::default())`, apply category marker via a
  queued command.
- `RemoveNode { target }` / `RemoveRefEdge { target }` → despawn.
- `Move { target, new_parent, position }` → `project_move` despawns
  any prior `TreeEdge` and inserts the new `TreeParent` / `OrderKey`,
  spawning a fresh `TreeEdge` under the new parent. This path also
  fires `GraphMessage::TreePositionChanged` indirectly via the
  `detect_tree_position_changes` system in `kyoso_graph` (Phase 2)
  picking up the resulting `Changed<TreeParent>`.

Property ops (`SetNodeProperty` / `SetRefEdgeProperty`) are *not*
applied here — they're handled in Phase 3 by `route_typed_inbound`.
But `apply_one` still writes the `RemoteOpApplied` event for them so
the schema chain can pick them up.

### Phase 2 — Structural detection (Bevy → CRDT)

Five systems watch for local ECS-side mutations and queue the
corresponding ops:

- `detect_added_nodes`: `Query<Entity, Added<N>>` → for any new entity
  with the structural marker that isn't already bound in the index,
  call `engine.add_node()` and bind the result. (Skips already-bound
  entities — that's how Phase 1's `commands.spawn(N::default())` for
  remote AddNode doesn't echo.)
- `detect_added_edges`: similar shape over `Added<E>`.
- `detect_tree_moves`: queries `Changed<TreeParent>` /
  `Changed<OrderKey>`, calls `engine.move_node` if the engine's view
  disagrees with the component (echo guard). This is how
  `GraphCommand::Reparent` flows out to the wire. Named to avoid
  collision with `kyoso_graph::detect_tree_position_changes`, which
  observes the same query but emits the ECS-side
  `GraphMessage::TreePositionChanged` propagation event instead.
- `detect_removed_nodes` / `detect_removed_edges`: drive
  `engine.remove_node` / `engine.remove_edge` off
  `RemovedComponents<…>`.

Echo prevention is structural, not stateful: each detection system
either checks the index (already-bound = remote, skip) or compares
component value to engine state (match = no work). There is no
"`just_projected` set" — that approach was tried and removed because
the structural checks subsume it.

### Phase 3 — Schema sync chain (per typed component)

Each `SchemaSyncedNodeComponentPlugin<N, E, C>` adds a four-system
chain ([`schema_sync.rs`](../../../crates/kyoso_graph_sync/src/schema_sync.rs)):

1. **`ensure_schema_slots`** — for every entity carrying `C` that has
   a bound `CrdtId`, ensure the per-id schema slot exists in
   `SchemaDoc<C::Schema>`. Idempotent.
2. **`route_typed_inbound`** — reads `RemoteOpApplied` events from
   Phase 1. For each op whose `path` head matches `C::SCHEMA_NAME`,
   strip the head and apply the rest to the corresponding entry in
   `SchemaDoc<C::Schema>` via `apply_property_op`. This is what makes
   `SetNodeProperty { path = ["Resistor", "resistance_ohms"], delta = … }`
   land in `SchemaDoc<ResistorSchema>` rather than getting silently
   dropped.
3. **`project_typed_to_bevy`** — if `doc.is_changed()` (i.e. step 2
   just applied something, *or* hydration in Phase 1 just installed
   state for new ids), walk every indexed entity and write the doc's
   per-id schema back into the Bevy component via `C::write_back`. For
   entities that don't have `C` yet (snapshot-spawned entities that
   are missing this component), queues an `InsertSchemaProjected`
   command that inserts `C::default()` then `write_back`s.
4. **`detect_typed_changes`** — queries `(Entity, Ref<C>)` with
   `Changed<C>`. For each candidate, compute
   `component.changes_against(doc.schema(id))` and emit one
   `SetNodeProperty` op per diff. Includes the
   **reconnect-clobber guard**: skip if `component.is_added()` AND
   `*current != C::Schema::default()` — that's the snapshot-hydration
   signature (just-inserted default placeholder, doc holds real
   values); we let `project_typed_to_bevy` overwrite the component
   rather than misreading it as a user mutation. See §6 for the
   failure mode.

Inbound runs before outbound in the same frame so a remote write
arriving on the same frame as a local edit doesn't double-emit. The
LWW conflict is decided by `GlobalSeq` on the server.

### Phase 4 — Outbound

[`outbound_system`](../../../crates/kyoso_graph_sync/src/plugin.rs)
drains `engine.drain_pending()`, postcard-encodes each op, and ships
via `WsBridge::submit`. When `engine.applied_seq()` has advanced past
the last sent value, it also sends an `Ack` so the server can compact.

---

## 4 · State stores at a glance

| Store | Lives in | What it holds | Synced via |
|---|---|---|---|
| `ClientSyncEngine` (= `GraphBackend<EmptySchema>` inside) | Resource on the client | Topology only — nodes, edges, tree parents, tombstones | `apply_remote` (Phase 1) + `add_node`/`add_edge`/`move_node`/etc. (Phase 2) |
| `SchemaDoc<C::Schema>` | Resource on the client, one per registered `C` | Per-id field state for one Bevy component type | `apply_property_op` via `route_typed_inbound` (Phase 3, step 2); `install_state` via hydrator on Welcome |
| `EntityCrdtIndex` | Resource on the client | `Entity ↔ CrdtId` bidirectional maps (separately for nodes and edges) | Mutated by `project_op` (inbound), `detect_added_*` (outbound), `RemoveNode`/`RemoveRefEdge` projections |
| `GraphBackend<OpaqueRecord>` (server-side `ServerModel`) | Per-room on the server | Topology + every peer's opaque per-id schema state | Server stamps each incoming op with `GlobalSeq`, applies, broadcasts |

The split between the client's structural `GraphBackend<EmptySchema>`
and the per-component `SchemaDoc<S>` is the **flat-store decision**:
typed schemas are sharded by component type at rest on the client,
even though the server stores them flat in a single `OpaqueRecord`.
The rationale: the client knows the static set of registered schema
types (one plugin per `C`), so per-`C` resources give safe typed
access for free; the server doesn't and shouldn't know about the
concrete `S`s, so it carries opaque per-primitive bytes.

`OpaqueRecord` ([`crates/kyoso_crdt/src/opaque.rs`](../../../crates/kyoso_crdt/src/opaque.rs))
is the bridge: a `BTreeMap<Path, OpaqueValue>` where each
`OpaqueValue` is `Lww(byte_payload) | OrSet(byte_payload) |
PnCounter(state) | Sequence(state)`. Snapshots can roundtrip without
knowing which `Resistor` / `Transform` / `Counted` schema struct
lives behind each path. On Welcome, the receiving peer's
`hydrate_typed_schemas` walks every `(target, OpaqueValue)` pair and
calls the registered `HydratorFn` for the `(TargetKind, schema_name)`
key, which knows the concrete `S` and decodes the bytes via
`SchemaApply::install_state`.

---

## 5 · The Welcome handshake — the trickiest path

Reconnect / late-join is where most subtle bugs live, because three
things need to land in the right order on the joining peer.

```
Server                                         Joining peer
  │                                                 │
  ├── Welcome { peer, models[graph: {                │
  │      snapshot_payload: ServerSnapshot,          │
  │      diff_payload: Diff<OpKind>                 │
  │   }] } ─────────────────────────────────────►   │
  │                                                 │
  │                                            ┌────┴────┐  ── inside graph_inbound_system
  │                                            │ engine.restore(structural) │
  │                                            │ project_snapshot(commands, index, topology) │
  │                                            │   ◦ commands.spawn(N::default()) per node   │
  │                                            │   ◦ commands.spawn((EdgeFrom, EdgeTo,…)) per edge │
  │                                            │   ◦ binds index synchronously     │
  │                                            │ commands.queue(hydrate_typed_schemas) │
  │                                            │ apply_one(diff op) × N             │
  │                                            └────┬────┘
  │                                                 │ command flush at sync point ↓
  │                                            ┌────┴────┐
  │                                            │ entities spawned     │
  │                                            │ Add observers fire   │
  │                                            │   (Mesh3d etc → auto-required Transform::default()) │
  │                                            │ hydrate_typed_schemas runs:                          │
  │                                            │   per (target, field), HydratorFn writes into SchemaDoc<S> │
  │                                            └────┬────┘
  │                                                 │ schema chain (Phase 3) runs ↓
  │                                            ┌────┴────┐
  │                                            │ ensure_schema_slots: ok        │
  │                                            │ route_typed_inbound: applies   │
  │                                            │   any property ops from diff   │
  │                                            │ project_typed_to_bevy:         │
  │                                            │   doc.is_changed → write_back  │
  │                                            │   or InsertSchemaProjected     │
  │                                            │ detect_typed_changes:          │
  │                                            │   ⚠ is_added + non-default-doc │
  │                                            │     guard skips spurious emits │
  │                                            └─────────┘
```

The thing that makes this delicate: between `project_snapshot`
spawning an entity and `project_typed_to_bevy` writing real values
into it, Bevy's `Add` observers and `#[require(...)]` chain insert
*default* values for any required components (Transform, Mesh3d,
Visibility, …). Without the `is_added && doc != default` guard in
`detect_typed_changes`, those defaults trip `Changed<C>` against a
freshly-hydrated `SchemaDoc<S>`, the diff says "user just reset
everything to default," and the joining peer broadcasts
`SetNodeProperty(<default>)` for every node it just received —
clobbering the live scene on every other peer. See
[`derived_schema::reconnect_clobber`](../../../crates/kyoso_graph_sync/tests/derived_schema.rs)
for the regression test, and the `auto_required_default_on_snapshot_join_does_not_clobber_existing_state`
case for the exact failure mode reproduced.

---

## 6 · Invariants — what to preserve when refactoring

These are the load-bearing rules. Each has a test that fails if you
break it.

| Invariant | Failure mode | Test |
|---|---|---|
| `detect_typed_changes` skips Added-this-frame components when `SchemaDoc<S>` already holds non-default state. | Reconnect clobbers every peer's scene with locally-defaulted values. | [`derived_schema::reconnect_clobber::auto_required_default_on_snapshot_join_does_not_clobber_existing_state`](../../../crates/kyoso_graph_sync/tests/derived_schema.rs) |
| `AddRefEdge` arriving for an endpoint that's already tombstoned must apply pre-tombstoned (cascade), never as a live edge. | "Orphan edge" — an edge whose live status says yes but whose endpoint is dead, surfaced by the chaos sim's invariant checker. | Chaos-sim `CascadeHeavy` workload + [`invariants::check_topology`](../../../crates/kyoso_graph_crdt/src/invariants.rs) |
| Snapshots are postcard-deterministic across peers: same canonical state → byte-identical bytes. | Convergence checks via byte equality fail; snapshot-based catchup ships different bytes to different peers. | `BTreeMap` (not `HashMap`) inside every CRDT primitive + `Path/PathSegment: Ord`; tested by [`proptest_snapshot`](../../../crates/kyoso_graph_crdt/tests/proptest_snapshot.rs). |
| `GraphTopology::would_create_cycle` and the ECS-side equivalent agree on every `(target, parent)` pair. | Server accepts a Move the client rejects (or vice versa), violating total-order semantics. | [`kyoso_graph/tests/cross_view.rs`](../../../crates/kyoso_graph/tests/cross_view.rs) (proptest, 64 random op streams + 6 hand-crafted). |
| Schema-sync chain order is `ensure → route → project → detect`. | Detect runs before the doc has been written → emits spurious ops (the reconnect-clobber bug surfaced from a related re-order). | Implicit; covered by `derived_schema` + `two_apps` tests. |
| Welcome diff ops + snapshot hydration are commutative on the receiver. | Diff op modifies a field; snapshot hydrator overwrites it back. | [`compaction_recovery::late_joiner_hydrates_typed_schema_after_compaction`](../../../crates/kyoso_graph_sync/tests/derived_schema.rs). |

---

## 7 · Glossary — names and their aliases

The codebase has accumulated several names for the same conceptual
thing. This is the authoritative list.

### State containers

- **`Backend<T, S>`** (`kyoso_crdt::Backend`) — the generic CRDT
  engine. `T: Topology`, `S: Crdt + SchemaApply`. Holds `applied_seq`,
  pending op queue, per-id schema slots, topology state. Headless,
  no Bevy.
- **`GraphBackend<S>`** — thin wrapper over `Backend<GraphTopology,
  S>` with the structural-op convenience methods (`add_node`,
  `add_edge`, `move_node`, `remove_node`, `remove_edge`). Still
  headless.
- **`ClientSyncEngine`** — Bevy `Resource` wrapper over
  `GraphBackend<EmptySchema>`. Adds nothing semantic anymore; exists
  so detection systems can take `ResMut<ClientSyncEngine>` rather than
  the raw `GraphBackend` (which doesn't `derive(Resource)`).
- **`SchemaDoc<S>`** — Bevy `Resource` wrapper over
  `Backend<GraphTopology, S>`. One per registered schema-synced
  component. Holds *only* the per-id typed schema state for that
  one component type; topology lives in `ClientSyncEngine`.
- **`ServerModel`** — server-side per-room state. Currently
  `GraphBackend<OpaqueRecord>`. Aliased that way in
  `apps/kyoso_server/src/model.rs`.

### Snapshots

- **`Snapshot<T, S>`** (`kyoso_crdt::Snapshot`) — generic snapshot.
  `at_seq + topology + schemas: BTreeMap<CrdtId, S>`.
- **`EngineSnapshot`** — `Snapshot<GraphTopology, EmptySchema>`. What
  the client's `ClientSyncEngine` snapshots/restores. No schema state.
- **`ServerSnapshot`** — `Snapshot<GraphTopology, OpaqueRecord>`.
  What the server emits in Welcome. Carries every peer's typed schema
  state opaquely.
- **`GraphTopologySnapshot`** — *domain-specific* topology
  serialization (`Vec<NodeSnap> + Vec<EdgeSnap>`) inside the generic
  snapshot's `topology` field. Different shape from the generic
  `Snapshot<T, S>` one level up. Note the unrelated
  `kyoso_graph::solver::GraphSnapshot` is a `petgraph::StableGraph`
  mirror for off-thread solving — same domain, different purpose,
  different shape.

### Identity

- **`CrdtId`** — `(peer: u32, seq: u64)`. Stable across renames /
  reparents / property changes. Born once, lives forever (or until
  tombstoned).
- **`Entity`** — Bevy's ECS handle. Local to one peer; differs across
  peers for the same logical node. Bridged to `CrdtId` via
  `EntityCrdtIndex`.
- **`PeerId`** — `u32`. Issued by the server on connect; scoped to a
  single session.
- **`GlobalSeq`** — `u64`. The server-stamped totally-ordered sequence
  number per (room, model). Different from the `CrdtId.seq` (which is
  peer-local; server-stamped `GlobalSeq` lives in `Op::seq` on the
  wire frame).

### Ops and deltas

- **`Op<OpKind>`** — one wire frame: `{ id: CrdtId, seq:
  Option<GlobalSeq>, kind: OpKind }`. `seq = None` means
  client-originated, awaiting server stamp; `seq = Some(_)` means
  server-confirmed.
- **`OpKind`** (graph-specific) — `AddNode | AddRefEdge | RemoveNode |
  RemoveRefEdge | Move | SetNodeProperty | SetRefEdgeProperty`.
- **`WireDelta`** — per-primitive opaque payload on the wire:
  `LwwReplace(bytes) | OrSetAdd/Remove(bytes) | PnInc/Dec(amount) |
  SequenceInsert/Delete(…)`. Schema-aware decoders on the receiving
  side turn it into typed `Crdt::Delta`s.
- **`Path` / `PathSegment`** — schema-relative addressing inside an
  `OpaqueRecord`. First segment is the schema name (e.g.
  `"Resistor"`); subsequent segments walk into the schema struct.

### Schema machinery

- **`SchemaApply`** (`kyoso_crdt::SchemaApply`) — trait every schema
  struct implements (via `derive(Crdt)`): `apply_wire(path, delta,
  ctx)` and `install_state(path, opaque_field)`. The two ways state
  enters the schema struct.
- **`SchemaSync`** (`kyoso_graph_sync::SchemaSync`) — trait that
  bridges a Bevy `Component` to its schema struct: `changes_against`
  (component → mutations) and `write_back` (schema → component). The
  associated `type Schema` must implement `SchemaApply`.
- **`SchemaField`** — escape hatch for fields whose Bevy type doesn't
  fit the standard schema bounds; used by `#[crdt(with = "Type")]`.
- **`SchemaTarget`** — abstracts "this schema attaches to nodes" vs
  "this schema attaches to ref edges." Two impls: `NodeTarget` /
  `EdgeTarget`. Used to make the schema-sync chain generic over both
  shapes without duplicating systems.
- **`SchemaHydrators`** — registry mapping `(TargetKind, schema_name)`
  → `HydratorFn` so the Welcome handler can route opaque per-id state
  into the right `SchemaDoc<S>` resource at runtime.

### Graph topology

- **`Topology`** (`kyoso_crdt::Topology`) — abstract trait the
  `Backend<T, S>` uses to apply structural ops. Has `apply_structural_op`,
  `snapshot_state`, etc.
- **`GraphTopology`** — graph-specific `Topology` impl. Headless,
  `HashMap`-backed. Hosts `would_create_cycle`, `tree_parent`, etc.
- **`GraphView`** (`kyoso_graph_crdt::GraphView`) — read-only trait
  over a graph topology. Implemented by `GraphTopology` (headless) and
  by `EcsGraphView` (over `EdgeFrom`/`EdgeTo`/`OutgoingEdges`/
  `IncomingEdges`/`TreeParent` in Bevy ECS). Free-function algorithms
  (`would_create_cycle`, `ancestors`, `descendants`,
  `connected_component_undirected`) live once and operate over
  `impl GraphView`.
- **`GraphQuery`** (`kyoso_graph::GraphQuery<N, E>`) — typed Bevy
  `SystemParam` for traversal with domain components in scope. Higher-
  level than `EcsGraphView`; uses it internally for the algorithms.

### Events and messages

- **`RemoteOpApplied(GraphOp)`** — Bevy `Message` emitted by Phase 1
  for every server-confirmed op the inbound system applied. Routed to
  schema-sync plugins as the bridge into Phase 3 step 2.
- **`GraphMessage`** (`kyoso_graph::GraphMessage`) — ECS-side
  propagation event. `NodeAdded | NodeRemoved | EdgeAdded | EdgeRemoved |
  NodeConnected | NodeDisconnected | NodeChanged | EdgeChanged |
  TreePositionChanged | PropagationTriggered`. Fires for *both* local
  and remote-applied changes — detected via `Added<…>`, `Changed<…>`,
  `RemovedComponents<…>` queries that fire regardless of who caused
  the change.
- **`GraphCommand`** — intent-based command messages
  (`Connect | Disconnect | RemoveNode | RemoveEdge | InsertChild |
  Reparent | MoveSibling`) for user-issued tree edits. Consumed by
  `consume_graph_commands` + `tree::apply_tree_commands`.
- **`WsInbound`** — transport-level event from `kyoso_sync`: `Welcome
  | ModelApply | ModelApplyBatch | ModelCatchup | Disconnected`.
- **`SyncStatus`** — connection state resource: `Disconnected |
  Connecting | Connected`.

### Index

- **`EntityCrdtIndex`** — bidirectional `Entity ↔ CrdtId` for nodes
  (`node_of_entity` / `entity_of_node`) and edges (`edge_of_entity` /
  `entity_of_edge`). The single source of "which Bevy entity is this
  CRDT id?"

---

## 8 · Composition examples

### Adding a per-field-synced component

Three steps end-to-end:

1. **Define the component**.

   ```rust
   #[derive(Component, Default, Clone, PartialEq, Reflect, SchemaSync)]
   #[reflect(Component, Default)]
   #[schema(name = "Resistor")]
   pub struct Resistor {
       pub resistance_ohms: f32,
   }
   ```

2. **Register the plugin**.

   ```rust
   app.add_plugins(SchemaSyncedNodeComponentPlugin::<CircuitNode, CircuitEdge, Resistor>::default());
   ```

3. **Mutate**. Spawn or modify the component on any entity that also
   carries the `N` marker (`CircuitNode`). The chain in §3 handles the
   rest.

The `derive(SchemaSync)` macro generates `ResistorSchema` (a
`#[derive(Crdt)]` struct with one `LwwRegister<f32>` field) plus the
`SchemaSync` impl. The plugin (a) inits the `SchemaDoc<ResistorSchema>`
resource, (b) registers a hydrator in `SchemaHydrators` so Welcome can
install opaque state, (c) schedules the four-system chain.

### Adding a new edge category

Edges carry an `EdgeCategory::Custom("wire" | "same_net" | …)` string
on the wire (`kyoso_graph_crdt::OpKind::AddRefEdge { … category }`).
On the ECS side, add a per-category marker component and register a
`SyncedEdgeCategoryPlugin` that maps the wire string to the marker.
See `kyoso_circuit::edge::{CircuitEdgeKind, WireMarker, SameNetMarker, …}`
for the full pattern.

### Adding a new top-level model (e.g. presence cursors)

Already done for `kyoso_comments_crdt` + `kyoso_comments_sync`. The
template:

1. New crate with a `Topology` impl + `OpKind` enum + `Backend<T, S>`
   wrapper (see `kyoso_comments_crdt::CommentsBackend`).
2. Sibling sync crate that adds a `CommentsSyncPlugin` mirroring
   `GraphSyncPlugin` (inbound system, detection systems, outbound
   system, register a `ModelId` slug with `ModelRegistry`).
3. The transport (`kyoso_sync::SyncTransportPlugin`) already
   multiplexes per-model traffic on a single socket; no protocol-level
   changes needed.

See `crdt-overview.md §7.3` for the worked example.

---

## 9 · Cleanup punch-list

Items in this session done inline:

- ✅ **`ClientSyncEngine::just_projected`** — dead echo-prevention
  field + 4 methods + 1 test removed. Echo prevention is structural
  (index lookup, value comparison) and doesn't need a tracked set.
- ✅ **Stale `CrdtSyncPlugin` docstring references** — updated to
  `GraphSyncPlugin` in `kyoso_figma/src/lib.rs` (×3) and
  `apps/kyoso_client/src/scene.rs` (×1).
- ✅ **Stale `Document<S>` reference** in
  `kyoso_comments_crdt/src/backend.rs:165`.
- ✅ **Misleading "legacy convenience"** comment in
  `kyoso_graph_sync/src/plugin.rs` (single-model `GraphSyncPlugin::new`
  isn't legacy — re-worded to "transport bundled in").

A second cleanup pass after this doc landed swept the medium-effort items too:

- ✅ **`GraphSnapshot` → `GraphTopologySnapshot`.** Renamed the
  domain CRDT topology snapshot in `kyoso_graph_crdt` so it no longer
  shares the same identifier with the unrelated
  `kyoso_graph::solver::GraphSnapshot`. The two could collide in any
  scope that imports both crates — now they read differently at a
  glance.
- ✅ **`mint_id` / `enqueue` → `pub(crate)`.** The only caller is the
  typed-schema chain inside `kyoso_graph_sync`; external callers should
  go through the structural API (`add_node` / `add_edge` /
  `move_node`). Re-scoped with a comment explaining why.
- ✅ **`detect_tree_position_changes` collision resolved.** The
  `kyoso_graph_sync` one renamed to `detect_tree_moves` (matches the
  `detect_added_nodes` / `detect_removed_nodes` naming pattern in
  that crate). The `kyoso_graph` one keeps the longer name because
  it's part of the propagation-event language, not the
  detect-for-outbound language. Both still observe the same
  `Changed<TreeParent>` query; merging into one system would
  cross the crate seam and that costs more than it saves.
- ✅ **`GraphSystemSet::EventGeneration` and `Consumption` dropped.**
  Neither had any callers (verified workspace-wide). The chain is now
  the six sets that actually carry systems.
- ❌ **`kyoso_graph_sync::SchemaField`** — kept. It's the bridge for
  `#[crdt(with = "Type")]` in the derive macro, has a generic built-in
  impl over `LwwRegister<T>`, and is exercised end-to-end by
  [`custom_with::custom_schema_field_replicates_end_to_end`](../../../crates/kyoso_graph_sync/tests/derived_schema.rs).
  Tested infrastructure waiting for a real consumer (Fugue text,
  `Handle<Image>`, `Entity` reference fields).
- ✅ **`OpaqueField` → `OpaqueValue`, `OpaqueSchemaState` → `OpaqueRecord`.**
  "Field" reads as a struct field; the type is actually one
  primitive's bytes — `Value` fits. And `OpaqueSchemaState` (the map)
  is now `OpaqueRecord` — one record per entity. ~114 call sites
  swept via `perl -pi -e 's/\b…\b/…/g'`.

---

## 10 · Pointer table

Where to look when:

| Question | File |
|---|---|
| What ops can the graph model produce? | [`crates/kyoso_graph_crdt/src/op.rs`](../../../crates/kyoso_graph_crdt/src/op.rs) |
| How is a wire op decoded and applied? | [`crates/kyoso_graph_sync/src/plugin.rs::graph_inbound_system`](../../../crates/kyoso_graph_sync/src/plugin.rs) |
| How is a Bevy component mutation detected? | [`crates/kyoso_graph_sync/src/schema_sync.rs::detect_typed_changes`](../../../crates/kyoso_graph_sync/src/schema_sync.rs) |
| What happens on Welcome? | `plugin.rs::graph_inbound_system` Welcome arm + `hydrate_typed_schemas` |
| Cycle detection on a Move op? | [`crates/kyoso_graph_crdt/src/view.rs::would_create_cycle`](../../../crates/kyoso_graph_crdt/src/view.rs) |
| Snapshot encoding determinism? | `kyoso_crdt::Snapshot` (`BTreeMap` schemas) + every primitive's internal `BTreeMap` |
| Topology invariants? | [`crates/kyoso_graph_crdt/src/invariants.rs`](../../../crates/kyoso_graph_crdt/src/invariants.rs) |
| End-to-end convergence tests? | [`crates/kyoso_graph_sync/tests/two_apps.rs`](../../../crates/kyoso_graph_sync/tests/two_apps.rs), [`crates/kyoso_graph_sync/tests/derived_schema.rs`](../../../crates/kyoso_graph_sync/tests/derived_schema.rs) |
| Reproducible scenario harness? | [`crates/kyoso_scenarios/src/scenarios.rs`](../../../crates/kyoso_scenarios/src/scenarios.rs) |
| Chaos sim invariant check? | [`crates/kyoso_loadgen/src/sim.rs`](../../../crates/kyoso_loadgen/src/sim.rs) + `kyoso_loadgen/src/bin/kyoso_chaos.rs` |

Related docs:

- [`crdt-overview.md`](crdt-overview.md) — substrate, primitives,
  worked composition examples, presence vs storage split.
- [`crdt.md`](system-design/crdt.md) — research landscape (Fugue, Eg-walker, Loro,
  Yjs), deferred decisions, the original phased plan.
- [`event_bus.md`](event_bus.md) — how the client app's external
  message bus sits *alongside* the CRDT sync layer.
- [`architecture-evolution.md`](architecture-evolution.md) — what the
  refactor turned over and what "legacy" means in the codebase.
- [`backend-vs-document.md`](backend-vs-document.md) — historical
  context for the `Backend<T, S>` unification.
- [`creating-new-crdt-structures.md`](creating-new-crdt-structures.md) —
  the recipe for adding a new domain model crate.
