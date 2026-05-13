# CRDT stack — implementation overview

> **Status:** resolved. This document describes what is actually wired in
> the workspace today. For the research/landscape (Fugue, Eg-walker,
> Loro, Yjs, P2P vs. server-mediated) and the original phased plan,
> see [`crdt.md`](crdt.md). For how the client app's external message bus
> sits *alongside* the CRDT sync layer, see [`event_bus.md`](event_bus.md).

## 1 · How to read this doc

This is the onboarding doc for someone who is about to add replicated
state to a kyoso app, write a new domain model, or understand why a sync
bug looks the way it does. It covers, in order:

- **§2** the substrate (`kyoso_crdt`): identity, ops, lattices, primitives, derives.
- **§3** the graph model (`kyoso_graph_crdt`): the first `CrdtModel` impl in the workspace.
- **§4** the ECS bridge (`kyoso_graph_sync`): how a Bevy component flows to and from the op log each frame.
- **§5** the transport (`kyoso_sync`): the multi-model WebSocket envelope.
- **§6** the server (`apps/kyoso_server`): rooms, append locks, snapshots, presence.
- **§7** three worked composition examples — adding a property, adding an edge category, adding a new model.
- **§8** presence vs. storage — what the split looks like as wired today.
- **§9** apps that consume the stack: `kyoso_client`, `kyoso_circuit_client`, `kyoso_loadgen`.
- **§10** known gaps and deferred decisions.
- **§11** pointer table.

### One-paragraph mental model

Each *room* has one totally-ordered op log per *model* (graph, comments,
…). Clients open one WebSocket per room; an [envelope][envelope]
multiplexes per-model traffic over that one socket. Locally-generated
ops accumulate in a per-model `pending` queue; an outbound system per
frame ships them to the server; the server stamps each op with a
monotonic [`GlobalSeq`][globalseq], appends to the log, and broadcasts
to every peer (including the originator). On every peer — including
the originator — the inbound system applies confirmed ops in `GlobalSeq`
order, then *projects* the resulting state back into the Bevy ECS. The
originator's local state is therefore not pre-applied; **visibility
waits one server round-trip**, which is the price the design pays to
keep CRDT semantics simple and `apply` idempotent.

### Crate map

| Crate | Role |
|---|---|
| [`kyoso_crdt`](../../../crates/kyoso_crdt/) | Model-agnostic primitives: identity, ops, lattice, wire format, envelope, protocol. |
| [`kyoso_crdt_derive`](../../../crates/kyoso_crdt_derive/) | `#[derive(Crdt)]` for schema structs (the *content* of a node). |
| [`kyoso_sync_derive`](../../../crates/kyoso_sync_derive/) | `#[derive(SchemaSync)]` for Bevy components (the *bridge* to the schema). |
| [`kyoso_graph`](../../../crates/kyoso_graph/) | In-memory graph/tree data structures, no networking. |
| [`kyoso_graph_crdt`](../../../crates/kyoso_graph_crdt/) | `CrdtModel` impl for the graph: ops, backend, snapshot, edge categories. |
| [`kyoso_graph_sync`](../../../crates/kyoso_graph_sync/) | The Bevy ECS ↔ graph CRDT bridge. |
| [`kyoso_sync`](../../../crates/kyoso_sync/) | The model-agnostic WebSocket transport plugin. |
| [`kyoso_comments_crdt`](../../../crates/kyoso_comments_crdt/) | `CrdtModel` impl for comments. |
| [`kyoso_comments_sync`](../../../crates/kyoso_comments_sync/) | Bevy plugin for the comments model. |
| [`apps/kyoso_server`](../../../apps/kyoso_server/) | Axum WebSocket coordinator. Owns the canonical op log per model per room. |
| [`apps/kyoso_client`](../../../apps/kyoso_client/) | Figma-shaped 2D editor binary. |
| [`apps/kyoso_circuit_client`](../../../apps/kyoso_circuit_client/) | 3D analogue circuit designer binary. |
| [`kyoso_loadgen`](../../../crates/kyoso_loadgen/) | Load-generation and chaos-testing binaries. |

---

## 2 · The substrate (`kyoso_crdt`)

### 2.1 Identity

Three id types do all the work, defined in [crates/kyoso_crdt/src/id.rs](../../../crates/kyoso_crdt/src/id.rs):

- `PeerId = u32` — assigned by the server on session start.
- `LocalSeq = u64` — per-peer monotonic counter.
- `GlobalSeq = u64` — server-assigned position in the totally-ordered op log.

A `CrdtId = (peer, seq)` ([id.rs:40](../../../crates/kyoso_crdt/src/id.rs#L40)) is collision-free without
coordination because the originator mints it locally under its own
`PeerId`. The same value identifies both an *op* and (for add-style
ops) the *element it creates* — `AddNode` doesn't carry a separate
"new node id" field because the op's own `CrdtId` is the node id.

`IdGen` ([id.rs:108](../../../crates/kyoso_crdt/src/id.rs#L108)) is the cloneable handle around an
`Arc<Mutex<IdGenerator>>`. **All CRDT models on the same peer share
one `IdGen`**, which is what makes cross-model references (e.g. a
comment's `anchor: CrdtId` pointing at a graph node) safe: every model
mints from one `LocalSeq` counter, so collisions are impossible.

Why this matters: there is no model tag on a `CrdtId`. A comment's
anchor is just a `CrdtId`; it could refer to a graph node, an edge, or
even another comment, and the typing comes entirely from context. The
shared `IdGen` is what lets the design get away with that.

### 2.2 Ops, log, and protocol

`Op<K> { id, seq, kind }` in [op.rs:21](../../../crates/kyoso_crdt/src/op.rs#L21) is the wire envelope: the
generic `K` is the model-specific op enum (e.g.
`kyoso_graph_crdt::OpKind`). `seq` is `None` while the op is pending
ack and `Some(GlobalSeq)` once the server has stamped it. `Diff<K>`
([op.rs:55](../../../crates/kyoso_crdt/src/op.rs#L55)) is a contiguous slice of the log used to ship history
on catch-up.

`InMemoryOpLog<K>` ([log.rs:48](../../../crates/kyoso_crdt/src/log.rs#L48)) is the default log implementation;
`append()` assigns `seq = head() + 1`. Sequences are 1-indexed so `0`
can mean "before the log".

The single-model protocol ([protocol.rs](../../../crates/kyoso_crdt/src/protocol.rs)) defines:

- `ClientMsg::Hello { room, since }` — joining handshake.
- `ClientMsg::Submit(Op<K>)` — outbound local op.
- `ClientMsg::Catchup { since }`, `Ping { applied_seq }`, `Presence(Vec<u8>)`, `LeavePresence`.
- `ServerMsg::Welcome { peer, snapshot, diff, presence }` — handshake reply.
- `ServerMsg::Apply(Op<K>)` — broadcast a stamped op.
- `ServerMsg::Catchup(Diff<K>)`, `Pong`, `Error`, `PresenceUpdate`, `PresenceLeft`.

Production paths use the **multi-model envelope** instead
([envelope.rs](../../../crates/kyoso_crdt/src/envelope.rs)): `EnvelopeClientMsg::Hello` carries a list
of `(ModelId, since)` so one socket joins multiple models in one
handshake; `Submit / Catchup / Ping / Apply / ApplyBatch / Catchup`
all tag their per-model payload with a `ModelId` so the receiver routes
to the matching handler. The `Tier` field
([envelope.rs:71](../../../crates/kyoso_crdt/src/envelope.rs#L71)) lets the server distinguish full-fidelity
`ReadWrite` peers (live per-op fanout, ~50ms p99) from observer-tier
`Read` peers (coalesced `ApplyBatch` fanout at ~250ms, 10× room
capacity).

### 2.3 The `Lattice` / `Crdt` trait pair

The algebraic backbone is two layered traits, in
[lattice.rs](../../../crates/kyoso_crdt/src/lattice.rs):

- `Lattice` ([lattice.rs:36](../../../crates/kyoso_crdt/src/lattice.rs#L36)) — a join-semilattice with bottom. `join`
  is associative, commutative, idempotent. These three axioms are the
  reason convergence works under arbitrary message reordering and
  duplication.
- `Crdt: Lattice` ([lattice.rs:76](../../../crates/kyoso_crdt/src/lattice.rs#L76)) — adds a typed `Mutation`
  (intent — "set name to X") and `Delta` (wire-shippable, idempotent
  record of one change). The contract on `apply` is **idempotent**:
  applying the same delta twice is a no-op.

`DeltaError` ([lattice.rs:99](../../../crates/kyoso_crdt/src/lattice.rs#L99)) names the four ways an apply can
fail (`TypeMismatch`, `UnknownPath`, `Invalid`, `MissingPredecessor`)
— distinct from `ApplyError` ([model.rs:31](../../../crates/kyoso_crdt/src/model.rs#L31)) which models
*ordering* failures at the op-log level (`Unconfirmed`, `OutOfOrder`).

### 2.4 Causal context

A subtle but load-bearing piece. CRDTs need *fresh, unique* tags for
some operations (an OR-Set add needs a unique add-dot; a PN-Counter
needs a replica id). Rather than mint a new id from scratch and ship
it on the wire, every embedded CRDT derives its identity from the
*outer* op's `CrdtId + GlobalSeq` at apply time.

`Dot = CrdtId` ([context.rs:29](../../../crates/kyoso_crdt/src/context.rs#L29)). `SubDot { op, sub: u32 }`
([context.rs:35](../../../crates/kyoso_crdt/src/context.rs#L35)) is `(parent_op_id, counter)`, globally unique
because `parent_op_id` already is.

`CausalContext<'a>` ([context.rs:85](../../../crates/kyoso_crdt/src/context.rs#L85)) is the borrowed
view passed into `apply` (read-only) and `mutate` (read-write — embedded
CRDTs allocate sub-dots via `fresh_sub_dot()`). The persistent state
behind it is `CausalState` ([context.rs:60](../../../crates/kyoso_crdt/src/context.rs#L60)), a `HashMap<Dot, u32>`
of per-op sub-counters living on the backend.

**Consequence**: `WireDelta` carries no timestamps and no per-element
tags. The LWW stamp, the OR-Set add-tag, the PN-Counter replica are
*all* derived at the receiving end from the outer op's identity. See
the comment in [delta.rs:96–104](../../../crates/kyoso_crdt/src/delta.rs#L96-L104).

### 2.5 The wire delta and path addressing

One uniform on-the-wire shape — `WireDelta` ([delta.rs:106](../../../crates/kyoso_crdt/src/delta.rs#L106)) — with
variants for each base CRDT:

```
LwwReplace { value: Vec<u8> }
OrSetAdd   { value: Vec<u8> }
OrSetRemove { observed: Vec<SubDot> }
PnCounterDelta { by: i64 }
SequenceInsert { predecessor: Option<SubDot>, value: Vec<u8> }
SequenceDelete { targets: Vec<SubDot> }
MapPut    { key: PathSegment, inner: Box<WireDelta> }
MapRemove { key: PathSegment, observed: Vec<SubDot> }
```

A `Path` ([delta.rs:33](../../../crates/kyoso_crdt/src/delta.rs#L33)) is a list of `PathSegment::Field(String)` or
`PathSegment::Key(String)` and addresses *where* in a nested schema the
delta lands (e.g. `["style", "fill"]`).

Dispatch is the `SchemaApply` trait ([schema.rs:39](../../../crates/kyoso_crdt/src/schema.rs#L39)): an
implementation walks `path` through the struct's named fields and
converts the wire variant into the leaf CRDT's typed delta. The
`#[derive(Crdt)]` macro generates this impl.

The cycle for a property mutation looks like:

```
typed mutation  ─►  S::Mutation  ─► S::Delta  ─►  (Path, WireDelta)
                                                       │
                                  wire transit          ▼
                                                  OpKind::SetNodeProperty {
                                                    target: node_id, path, delta
                                                  }
                                                       │
                                                       ▼
                                          S::apply_wire(path, delta, ctx)
```

### 2.6 Base CRDT primitives

In [crates/kyoso_crdt/src/types/](../../../crates/kyoso_crdt/src/types/):

| Primitive | Convergence | Use for |
|---|---|---|
| [`LwwRegister<T>`](../../../crates/kyoso_crdt/src/types/lww.rs) | `(GlobalSeq, PeerId)` LWW; stamp derived at apply | Transforms, names, scalar enums — anywhere "later wins" is fine. |
| [`OrSet<T>`](../../../crates/kyoso_crdt/src/types/or_set.rs) | Add-wins; remove only targets *observed* dots | Tags, labels, any add-wins set. |
| [`PnCounter`](../../../crates/kyoso_crdt/src/types/pn_counter.rs) | Per-peer `pos`/`neg` `HashMap<PeerId, u64>`, pointwise max | Counters where concurrent ±1s should sum. |
| [`CausalMap<V: Crdt>`](../../../crates/kyoso_crdt/src/types/causal_map.rs) | Pointwise join over key | `HashMap<String, V>` where `V` itself is a CRDT. The central composition combinator. |
| [`Sequence<T>`](../../../crates/kyoso_crdt/src/types/sequence.rs) | **Naive `Vec`-backed stub** — single-writer safe only | Placeholder for `#[crdt(sequence)]` fields. Real Fugue/Eg-walker impl is future work; see §10. |

Each impl provides `Crdt` + `Lattice` + `SchemaApply` + `From<TypedDelta> for WireDelta` + `TryFrom<WireDelta> for TypedDelta`, all property-tested for the lattice axioms.

### 2.7 The `CrdtModel` trait

[model.rs:43](../../../crates/kyoso_crdt/src/model.rs#L43). The single abstraction every replicated data
structure implements. Two associated types — `OpKind` (the per-model op
enum) and `State` (the snapshot type) — plus seven methods covering the
lifecycle:

- `set_peer(peer)` — set on Welcome.
- `applied_seq()` — for liveness / GC.
- `apply_remote(op)` — idempotent apply, returns `ApplyError::OutOfOrder` if the op's seq doesn't match the expected high-water mark.
- `snapshot() / restore(snap)` — compaction + recovery.
- `drain_pending() -> Vec<Op<OpKind>>` — outbound side; the transport calls this each tick.
- `op_kind_label(op) -> &'static str` — for telemetry.

The graph and comments models both implement this trait; **a third model is a few hundred lines of code** thanks to the shared substrate.

### 2.8 Derive macros

Two procedural macros do the bulk of the per-domain glue work.

**`#[derive(Crdt)]`** ([kyoso_crdt_derive/src/lib.rs](../../../crates/kyoso_crdt_derive/src/lib.rs)) on a struct of CRDT-typed fields:

```rust
#[derive(Crdt)]
pub struct FrameSchema {
    pub name: LwwRegister<String>,
    pub tags: OrSet<String>,
    pub edit_count: PnCounter,
}
```

generates:

- `FrameSchemaMut` — a sum type with one variant per field carrying that field's `Crdt::Mutation`.
- `FrameSchemaDelta` — the same shape over `Crdt::Delta`.
- `impl Lattice` — pointwise join over fields.
- `impl Crdt` — typed `apply` / `mutate` that dispatches by variant.
- `impl SchemaApply` — wire-driven dispatch by the head `Path` field name.

**`#[derive(SchemaSync)]`** ([kyoso_sync_derive/src/lib.rs](../../../crates/kyoso_sync_derive/src/lib.rs)) on a Bevy component:

```rust
#[derive(Component, Default, Clone, PartialEq, SchemaSync)]
#[schema(name = "Frame")]
pub struct Frame {
    pub name: String,
    pub visible: bool,
    #[crdt(or_set)]    pub tags: Vec<String>,
    #[crdt(counter)]   pub edit_count: i64,
    #[crdt(skip)]      pub local_hover: HoverState,
}
```

generates `FrameSchema` (with each field wrapped in the appropriate CRDT
primitive — `LwwRegister<String>` for `name`, `OrSet<String>` for `tags`,
`PnCounter` for `edit_count`), an impl of `SchemaSync` that knows how to
`changes_against` an existing schema state (producing a list of
mutations) and how to `write_back` a schema state to the Bevy component.
Per-field attributes:

| Attribute | Schema type | Behaviour |
|---|---|---|
| (default) / `#[crdt(lww)]` | `LwwRegister<T>` | Echo-guards against `Self::default()`. |
| `#[crdt(or_set)]` | `OrSet<T>` (from `Vec<T>` / `HashSet<T>`) | Set-diff: add new, remove missing. |
| `#[crdt(counter)]` | `PnCounter` | Inc / Dec by signed diff. |
| `#[crdt(map)]` | `CausalMap<LwwRegister<V>>` | Per-key apply / remove. |
| `#[crdt(nested)]` | `<T as SchemaSync>::Schema` | Delegate to inner type's `SchemaSync`. |
| `#[crdt(with = "Type")]` | the named `Type` | Escape hatch: `Type: SchemaField`. |
| `#[crdt(sequence)]` | `Sequence<char>` or `Sequence<T>` | Prefix-suffix diff. **Stub** — see §10. |
| `#[crdt(skip)]` | — | Field not replicated. |
| `#[crdt(rename = "x")]` | — | Override the wire field name. |

The pair is the surface most contributors will touch.

---

## 3 · The graph model (`kyoso_graph_crdt`)

The first concrete `CrdtModel` impl. Its job: replicate a node + edge
topology with a tree-overlay structure, plus typed per-node properties.

### 3.1 The op kinds

[op.rs](../../../crates/kyoso_graph_crdt/src/op.rs):

```rust
pub enum OpKind {
    AddNode,                                                    // id = enclosing op's CrdtId
    RemoveNode { target: CrdtId },
    Move { target: CrdtId,
           new_parent: Option<CrdtId>,
           position: String },                                  // OrderKey, fractional index
    AddRefEdge { category: EdgeCategory, from: CrdtId, to: CrdtId },
    RemoveRefEdge { target: CrdtId },
    SetNodeProperty { target: CrdtId, path: Path, delta: WireDelta },
    SetRefEdgeProperty { target: CrdtId, path: Path, delta: WireDelta },
}
```

`AddNode` / `AddRefEdge` reuse the enclosing op's `CrdtId` as the new
element's id — there is no separate "new id" field. Removes are
tombstones (with cascade for incident reference edges), not deletions,
so late-arriving `AddRefEdge` ops referencing a removed node can be
detected and skipped deterministically.

### 3.2 Two distinct edge kinds

**Tree edges** are not a separate `OpKind`. The parent-child scaffold
lives as an annotation on nodes (`TreeParent`, `OrderKey`) and is
replicated through the atomic [`Move`][move-op] op:

- Reparent and reorder in one op.
- Cycle detection at apply time — the op is no-op'd rather than
  rejected (deterministic under server total order — every replica
  reaches the same accept/reject decision).
- `position` is a fractional-index string (`OrderKey`) — concurrent
  inserts at the same sibling position interleave by string compare.

This is the Kleppmann atomic-tree-move algorithm, simplified by the
fact that the server total-orders everything: peers don't have to
undo-and-redo speculative moves on out-of-order delivery because there
*is* no out-of-order delivery.

**Reference edges** are first-class entities. Each has a category
([edge_category.rs:29](../../../crates/kyoso_graph_crdt/src/edge_category.rs#L29)):

```
Reference            // default for untyped edges
InstanceOf           // Figma: component instance → main
PrototypeLink        // Figma: prototype transition
ConstraintPin
StyleRef
CommentAnchor
Mention
MaskOf
Custom(String)       // app-defined
```

Phase E treats the category as metadata. The `RefEdgeCrdt` trait
([edge_category.rs:123](../../../crates/kyoso_graph_crdt/src/edge_category.rs#L123)) is the hook for future per-category divergence
(different `RefEdgePolicy` — `OrSet` / `TwoPSet` / `RemoveWins` /
`LwwByEndpoints` — and `DanglePolicy` — `Cascade` / `Tolerate` /
`ReanchorOnUndo`). At the moment every category uses the same
remove-by-tombstone shape with `DanglePolicy::Cascade`.

### 3.3 The backend

[`CrdtBackend<N, E>`](../../../crates/kyoso_graph_crdt/src/backend.rs) is the storage type. Internally:

- `nodes: HashMap<CrdtId, NodeRecord>` — `(tombstoned, order_key, tree_parent, properties: HashMap<String, Vec<u8>>)`.
- `edges: HashMap<CrdtId, EdgeRecord>` — `(from, to, category, tombstoned, properties)`.
- `pending: Vec<Op<OpKind>>` — locally generated, not yet ack'd.
- `pending_moves: HashMap<CrdtId, CrdtId>` — `op_id → target_node_id` for in-flight moves so the detection systems can answer "is this entity awaiting a Move echo?" without reading a pre-applied `tree_parent`.
- `applied_seq: GlobalSeq` — high-water mark.
- `ids: IdGen` — *cloneable handle*, typically shared with every other CRDT model on the peer.

Mutating methods (`add_node`, `remove_node`, `add_edge`, `add_ref_edge_with_category`, `move_node`, etc.) **mint a `CrdtId` from `ids` and queue an `Op<OpKind>` in `pending`**. They do *not* update the backend's authoritative `nodes` / `edges` state. That update only happens when the server echo comes back through `apply_remote` ([backend.rs:469](../../../crates/kyoso_graph_crdt/src/backend.rs#L469)).

Exception: `AddNode` *can* be pre-applied because `or_insert` makes the
echo idempotent ([document.rs:148](../../../crates/kyoso_graph_crdt/src/document.rs#L148)). But mutations on existing state
are not pre-applied, because:

- `PnCounter` would double-count (the local apply and the echo both add).
- `LWW` stamps with `seq = None` compare "always loses to a confirmed
  op" — bad behaviour under interleaving with a concurrent remote op.
- `Sequence` would insert twice.

The decision is documented in detail at [document.rs:70–90](../../../crates/kyoso_graph_crdt/src/document.rs#L70).

The trade-off: one server round-trip of staleness per mutation. Apps
that want optimistic UI keep their own local shadow state — Bevy ECS
components are a natural fit — and re-sync when the echo arrives.

### 3.4 Schema-aware document

[`Document<S>`](../../../crates/kyoso_graph_crdt/src/document.rs) is the schema-typed sibling to `CrdtBackend`.
Where `CrdtBackend` stores per-property bytes (`HashMap<String, Vec<u8>>`),
`Document<S>` stores a typed `S` (a `#[derive(Crdt)]` schema struct) per node and routes inbound `SetNodeProperty` ops through `S::apply_wire`. The two layers coexist; the typed-component plugin layer in `kyoso_graph_sync` always uses `Document<S>`.

### 3.5 Snapshots

[`Snapshot`](../../../crates/kyoso_graph_crdt/src/snapshot.rs) is the materialised converged state at a `GlobalSeq`:

```
Snapshot { at_seq,
           nodes: Vec<NodeSnap { id, order_key, tree_parent, properties }>,
           edges: Vec<EdgeSnap { id, from, to, category, properties }> }
```

Tombstones are excluded — the snapshot only contains live state. That
is what makes compaction safe: once a snapshot at seq `N` exists *and*
every connected peer's `applied_seq >= N`, the server can drop log ops
below `N`. Late joiners get the snapshot in their `Welcome`, then the
diff since.

`apply_remote` restores `IdGen::next_seq` past the highest local seq in
the snapshot ([backend.rs:341](../../../crates/kyoso_graph_crdt/src/backend.rs#L341)), so newly-minted IDs after a restore can't
collide.

---

## 4 · The ECS ↔ CRDT bridge (`kyoso_graph_sync`)

The bridge is where the graph model meets Bevy. It's where most
new-contributor confusion lives, so this section is long.

### 4.1 Plugin layout

`GraphSyncPlugin<N, E>` ([plugin.rs:88](../../../crates/kyoso_graph_sync/src/plugin.rs#L88)) is the top-level Bevy plugin. It is generic over two marker components — `N` for nodes, `E` for edges — that consuming apps choose (e.g. `FigmaNode` / `FigmaEdge` or `CircuitNode` / `CircuitEdge`).

`build()` ([plugin.rs:124](../../../crates/kyoso_graph_sync/src/plugin.rs#L124)) installs:

- `ModelRegistry` (resource) + the graph model id pushed into it.
- `PeerIdGen` (resource) — the shared `IdGen` handle.
- `ClientSyncEngine` (resource) — a Bevy-side wrapper around `CrdtBackend<(), ()>`.
- `EntityCrdtIndex` (resource) — the bidirectional `Entity ↔ CrdtId` map.
- `RemoteOpApplied` (event) — emitted per confirmed op, consumed by typed schema and edge category plugins.
- `GraphLastAck` (resource) — last applied seq we sent as a Ping.

Then chains **seven systems** in `Update` ([plugin.rs:153](../../../crates/kyoso_graph_sync/src/plugin.rs#L153)):

```
graph_inbound_system
  → detect_added_nodes
  → detect_added_edges
  → detect_tree_position_changes
  → detect_removed_nodes
  → detect_removed_edges
  → outbound_system
```

Each frame, inbound runs first (network → ECS), then detection (ECS →
op queue), then outbound (op queue → network + ack).

### 4.2 The Entity ↔ CrdtId index

[`EntityCrdtIndex`](../../../crates/kyoso_graph_sync/src/index.rs) maps `Entity ↔ CrdtId` in both directions for both nodes and edges. Detection systems consult `node_id(entity)` to find the CRDT id for the entity they're emitting an op against; the inbound projector consults `entity_for_node(id)` to find (or create) the entity for an incoming op.

### 4.3 The `ClientSyncEngine` and echo suppression

[`ClientSyncEngine`](../../../crates/kyoso_graph_sync/src/engine.rs) wraps the `CrdtBackend` for Bevy. It is the same backend logic with two additions:

- A `just_projected: HashSet<CrdtId>` set ([engine.rs:45](../../../crates/kyoso_graph_sync/src/engine.rs#L45)) — op IDs the inbound projector applied this frame. Detection systems skip entities whose `CrdtId` is in this set, which is how the originator doesn't re-emit the op they just got an echo for.
- A `Bevy Resource` impl — so the engine can be inserted as one.

The set is cleared each frame; the discipline is "if you just spawned
an entity from an inbound op, mark its id so the next system in the
chain doesn't observe it as a `Added<C>` and emit an outbound op for
it."

### 4.4 The detection systems

These are local-to-network. Each watches a specific Bevy query and
emits the corresponding op into the engine's pending queue:

- `detect_added_nodes::<N, E>` — `Added<N>` → `OpKind::AddNode`.
- `detect_added_edges::<N, E>` — `Added<EdgeFrom>` + `Added<EdgeTo>` (with `E` marker, without `TreeEdge` marker) → `OpKind::AddRefEdge { category: Reference, ... }`. Typed-category plugins (§4.6) run *before* this and bind their own categories first.
- `detect_tree_position_changes::<N, E>` — `Changed<TreeParent>` or `Changed<OrderKey>` → `OpKind::Move`. Skips entities already in `pending_moves`.
- `detect_removed_nodes::<N, E>` and `detect_removed_edges::<N, E>` — Bevy `RemovedComponents` → `OpKind::RemoveNode` / `RemoveRefEdge`.

None of them write to ECS state. They write to the engine's `pending`
queue and the index.

### 4.5 The inbound projector

`graph_inbound_system` ([plugin.rs:178](../../../crates/kyoso_graph_sync/src/plugin.rs#L178)) reads `WsInbound` Bevy events (emitted by the transport plugin in `PreUpdate`), filters for graph traffic, decodes each payload, calls `engine.apply_remote(&op)`, then projects the op into ECS:

- `AddNode` → spawn entity with `N::default()`, bind in `EntityCrdtIndex`.
- `AddRefEdge` → spawn entity with `EdgeFrom`/`EdgeTo`/`E::default()`, then `commands.queue(ApplyEdgeCategory { entity, category })` to insert the matching marker if any plugin registered one.
- `Move` → update `TreeParent` and `OrderKey` on the target entity.
- `RemoveNode` / `RemoveRefEdge` → despawn entity.
- `SetNodeProperty` / `SetRefEdgeProperty` → no direct ECS write; emit `RemoteOpApplied` so the typed-schema layer (§4.7) routes it to a `Document<S>` and writes back to the Bevy component.

Each projected op's id is added to `just_projected` so the detection
systems running afterwards skip the new entity.

`Welcome { snapshot, diff }` is handled here too: snapshot is restored
into the engine (which bumps `IdGen`), then every diff op is applied
and projected as if it had arrived as `Apply`.

### 4.6 Typed edge categories

`SyncedEdgeCategoryPlugin<N, E, M>` ([category.rs:82](../../../crates/kyoso_graph_sync/src/category.rs#L82)) is what wires a per-category marker component:

```rust
#[derive(Component, Default, Debug, Clone)]
struct InstanceOfEdge;
impl EdgeCategoryMarker for InstanceOfEdge {
    fn category() -> EdgeCategory { EdgeCategory::InstanceOf }
}

app.add_plugins(SyncedEdgeCategoryPlugin::<MyNode, MyEdge, InstanceOfEdge>::default());
```

The plugin:

- Registers `M` in `EdgeCategoryProjectors` ([category.rs:62](../../../crates/kyoso_graph_sync/src/category.rs#L62)) — a `HashMap<String, fn(&mut World, Entity)>` keyed by the debug string of the category.
- Adds `detect_added_categorized_edges::<E, M>` ([category.rs:139](../../../crates/kyoso_graph_sync/src/category.rs#L139)) *before* the generic `detect_added_edges` — so a spawn with `(EdgeFrom(a), EdgeTo(b), MyEdge, InstanceOfEdge)` produces `AddRefEdge { category: InstanceOf, ... }` rather than the default `Reference` category.

Inbound `AddRefEdge` ops with that category get the marker re-attached
via the projector.

### 4.7 Typed schema sync

`SchemaSyncedNodeComponentPlugin<N, E, C>` ([schema_sync.rs:137](../../../crates/kyoso_graph_sync/src/schema_sync.rs#L137)) is the per-component sync wiring. For a Bevy component `C: SchemaSync`:

- Inserts a `SchemaDoc<C::Schema>` resource — a `Document<C::Schema>`.
- Adds three systems:
  - **Outbound**: `detect_typed_changes::<C>` on `Changed<C>` — compares against the schema state, emits one wire op per changed field through `ClientSyncEngine`.
  - **Inbound routing**: `route_typed_inbound::<C>` reads `RemoteOpApplied`, filters by `Path` head == `C::SCHEMA_NAME`, strips the prefix, calls `Document::apply_property_op`.
  - **Projection**: `project_typed_to_bevy::<C>` watches the document for changes, calls `SchemaSync::write_back` on the matching Bevy component.

Path namespacing keeps each schema's fields independent:

```
Set Frame.name = "X"  →  path = ["Frame", "name"]
Set Rectangle.w = 50  →  path = ["Rectangle", "w"]
```

The inbound dispatch matches the head; the schema's `SchemaApply` impl
(generated by `derive(Crdt)`) consumes the rest.

### 4.8 The outbound system

[plugin.rs:587](../../../crates/kyoso_graph_sync/src/plugin.rs#L587). Each frame:

- Skip if not `SyncStatus::Connected`.
- Drain `engine.drain_pending()`.
- Postcard-encode each `Op<OpKind>` and call `bridge.submit(graph_model(), payload)`.
- If `submit` returns `false` (transport dead), break the loop without acking.
- If `engine.applied_seq() > last_ack`, send a `Ping { applied_seq }` via `bridge.ack(...)` and update `last_ack`.

That `Ping` is what the server uses to compute the safe-to-compact
threshold across all peers.

---

## 5 · Transport (`kyoso_sync`)

`kyoso_sync` is **model-agnostic**. It owns the WebSocket; it knows
nothing about graphs or comments. Per-model plugins layer on top.

### 5.1 The `WsClient`

[client.rs:85](../../../crates/kyoso_sync/src/client.rs#L85). Holds its own multi-threaded tokio runtime (so the Bevy thread doesn't have to be async-aware), plus `(outbound_tx: mpsc::UnboundedSender<EnvelopeClientMsg>, inbound_rx: crossbeam_channel::Receiver<Inbound>)`. The io loop is a tokio task; dropping the runtime aborts it.

`connect(url, room, tier, models)` ([client.rs:101](../../../crates/kyoso_sync/src/client.rs#L101)) opens the WS, sends `EnvelopeClientMsg::Hello` with the model+since list, and spawns the io loop. `submit(model, payload)`, `catchup`, `ack`, `send_presence`, `leave_presence` are thin wrappers that wrap the payload in the right envelope variant and queue on `outbound_tx`. Each returns `bool` — `false` means the transport is dead and the caller should treat themselves as disconnected.

### 5.2 The Bevy plugin

`SyncTransportPlugin` ([transport.rs](../../../crates/kyoso_sync/src/transport.rs)) is what apps add to their Bevy app:

```rust
App::new()
    .add_plugins(SyncTransportPlugin::new("ws://...", "demo"))
    .add_plugins(GraphSyncPlugin::<MyNode, MyEdge>::default())
    .add_plugins(CommentsSyncPlugin::default())
    .run();
```

It owns:

- `WsBridge` (resource, [transport.rs:101](../../../crates/kyoso_sync/src/transport.rs#L101)) — the open `WsClient`.
- `ModelRegistry` (resource, [transport.rs:140](../../../crates/kyoso_sync/src/transport.rs#L140)) — list of `ModelId` per-model plugins register into.
- `PeerIdGen` (resource, [transport.rs:167](../../../crates/kyoso_sync/src/transport.rs#L167)) — the shared `IdGen` handle.
- `SyncStatus` (resource) — `AwaitingConnect / AwaitingWelcome / Connected { peer } / Disconnected`.
- A `WsInbound` event ([transport.rs:48](../../../crates/kyoso_sync/src/transport.rs#L48)) that mirrors `Inbound` one-to-one.

A `PreUpdate` system drains `WsClient::try_recv()` and re-emits each event as `WsInbound` so multiple per-model plugins can each filter for their own model.

The connect happens in `PreStartup` so every model plugin's `build()` has already registered its `ModelId` in the registry by the time `Hello` goes out.

### 5.3 Sequence diff

[`sequence_diff.rs`](../../../crates/kyoso_sync/src/sequence_diff.rs) — naive prefix-suffix diff used by `#[crdt(sequence)]` field codegen.
Single-writer safe; concurrent edits will lose data. See §10.

### 5.4 What's not there

- **No auto-reconnect.** When the transport reports `Disconnected`, the
  app sees `WsInbound::Disconnected` once and the resource flips to
  `Disconnected`. Re-establishing the connection requires recreating
  the plugin.
- **No offline buffer.** Pending ops sit in
  `CrdtBackend::pending` and aren't flushed to disk. A process restart
  loses them.
- **No backpressure beyond `bool`.** `submit` returns false when the
  outbound channel is closed; the outbound system simply stops trying
  this frame. There's no flow control on the queue depth.

These are deliberate v1 gaps — see §10.

---

## 6 · Server (`apps/kyoso_server`)

### 6.1 Topology

Axum HTTP server with a binary `/ws` endpoint. One process. Per-room
state lives in a `RoomManager` ([room.rs:239](../../../apps/kyoso_server/src/services/room.rs#L239)) backed by a `DashMap<RoomId, Arc<Room>>`. Rooms are lazy-created on first access; concurrent calls converge on the same `Arc`.

### 6.2 The `Room`

[room.rs:33](../../../apps/kyoso_server/src/services/room.rs#L33). A thin router:

- `handlers: HashMap<ModelId, Arc<dyn RoomModelHandler>>` — one per registered model, built by walking `AppState`'s `HandlerFactory` list.
- `broadcast: broadcast::Sender<EnvelopeServerMsg>` (capacity 256) — multi-model fan-out.
- `next_peer: AtomicU32` — room-wide peer-id assignment.
- `presence: Mutex<HashMap<PeerId, Vec<u8>>>` — opaque per-peer awareness bytes.

`submit(model, tier, payload)` looks up the handler, checks
`allows_submit(tier, &payload)` (default `true` for `ReadWrite`, model
chooses for `Read`), forwards to `handler.submit()`, and broadcasts the
returned `Apply` payload to everyone subscribed.

`welcome_for(models)` iterates the requested models and builds a
per-model greeting list (snapshot + diff for each).

### 6.3 Per-model handlers

The `RoomModelHandler` trait lives at [handler.rs](../../../apps/kyoso_server/src/services/handler.rs); two impls ship:

**`GraphRoomHandler`** ([handlers/graph.rs](../../../apps/kyoso_server/src/services/handlers/graph.rs)):

- Owns an `OpStore` (Postgres or in-memory) for the canonical log.
- Owns a `Mutex<CrdtBackend<(), ()>>` — the server-side mirror.
- Owns an `append_lock: Mutex<()>` — serialises `Submit` to keep `GlobalSeq` monotonic.
- `submit(payload)`: decode → `append_lock` → `store.append(op)` (assigns next seq) → `mirror.apply_remote(stamped)` → re-encode and return.
- `welcome_for(since)`: if the client is behind the latest snapshot, ship the snapshot + ops since `snapshot.at_seq`; otherwise just the diff since `since`.
- `take_snapshot()` and `run_gc()` hook into the schedulers.

**`CommentsRoomHandler`** ([handlers/comments.rs](../../../apps/kyoso_server/src/services/handlers/comments.rs)):

- In-memory `InMemoryOpLog<CommentOpKind>` only. No persistent storage (v1).
- No snapshot/GC.
- Permissive `allows_submit`: even `Tier::Read` peers can post comments (the read-only restriction applies to the graph, not annotations).

### 6.4 Op store

[store.rs](../../../apps/kyoso_server/src/services/store.rs). Two backends:

- `OpStore::in_memory()` — `BTreeMap<GlobalSeq, OpRow>` per room. Used by all workspace tests.
- `OpStore::postgres(url)` — `sqlx` against `rooms / ops / snapshots / peer_acks` tables; migrations run on connect.

All `Op` and `Snapshot` blobs are postcard-encoded so wire / on-disk /
in-memory formats are identical.

### 6.5 Schedulers

Two background tokio tasks ([lib.rs:22](../../../apps/kyoso_server/src/lib.rs#L22)):

- **Snapshot scheduler** — periodically calls `Room::take_snapshot_all()` on every live room. Handlers that don't snapshot are no-ops.
- **GC scheduler** — calls `Room::run_gc_all()`. The graph handler drops ops below `min(peer_acks, snapshot.at_seq)`.

Cadence is configurable via `SchedulerConfig`.

### 6.6 Presence

Model-agnostic, room-level. `Mutex<HashMap<PeerId, Vec<u8>>>` ([room.rs:41](../../../apps/kyoso_server/src/services/room.rs#L41)). `update_presence(peer, state)` overwrites the entry and broadcasts `PresenceUpdate`; `clear_presence(peer)` removes and broadcasts `PresenceLeft`. The map is snapshotted into `Welcome` so joiners hydrate without a round-trip.

No `GlobalSeq`, never persisted, dropped on disconnect — see §8.

### 6.7 Where the wire envelope is parsed

[handlers/room_ws.rs](../../../apps/kyoso_server/src/handlers/room_ws.rs). Per-connection axum handler: reads `EnvelopeClientMsg::Hello`, assigns a peer, builds the welcome, then loops on each subsequent frame and routes to the matching `Room::submit / catchup / record_ack / update_presence / clear_presence`. Subscribes to the room's `broadcast::Receiver` and forwards every server message to the WebSocket sink.

---

## 7 · Composition in practice — three worked examples

This is the section to read if you're adding new replicated state. Each
example walks the actual code top to bottom.

### 7.1 Add an LWW property to an existing component

Easiest case. Suppose `Frame` is already wired with `SchemaSync`, and you
want to add a new `corner_radius: f32` field that syncs LWW.

```rust
#[derive(Component, Default, Clone, PartialEq, SchemaSync)]
#[schema(name = "Frame")]
pub struct Frame {
    pub name: String,
    pub visible: bool,
    pub corner_radius: f32,          // ← new field
}
```

That's it. `#[derive(SchemaSync)]` regenerates `FrameSchema` with a new
`LwwRegister<f32>` field; `changes_against` compares the Bevy field
against the schema's value and emits `FrameSchemaMut::CornerRadius(LwwMut::Set(...))`
when they differ. The outbound detection system picks up the mutation
the next time `Changed<Frame>` fires, packages it as
`OpKind::SetNodeProperty { path: ["Frame", "corner_radius"], delta: LwwReplace(...) }`,
and ships it. Inbound routes by `Path` head `"Frame"` to
`Document<FrameSchema>`, which routes by tail `"corner_radius"` to the
`LwwRegister<f32>`. `write_back` updates the Bevy field. Done.

The same shape works for any of the per-field attributes in §2.8.

### 7.2 Add a typed edge category

Suppose your app wants a `Wire` edge category for the circuit designer.
The canonical case is in [kyoso_circuit](../../../crates/kyoso_circuit/edge.rs) — it already wires `WireMarker`, `SameNetMarker`, `DifferentialPairMarker`. Pattern:

1. **Pick a variant** in `EdgeCategory` ([edge_category.rs:29](../../../crates/kyoso_graph_crdt/src/edge_category.rs#L29)). If your category is first-class, add a variant to the enum. Otherwise use `EdgeCategory::Custom("Wire".into())`.

2. **Define a marker component**:

   ```rust
   #[derive(Component, Default, Debug, Clone)]
   pub struct WireMarker;
   impl EdgeCategoryMarker for WireMarker {
       fn category() -> EdgeCategory { EdgeCategory::Custom("circuit-wire".into()) }
   }
   ```

3. **Register the plugin**:

   ```rust
   app.add_plugins(
       SyncedEdgeCategoryPlugin::<CircuitNode, CircuitEdge, WireMarker>::default()
   );
   ```

Now spawning `(EdgeFrom(a), EdgeTo(b), CircuitEdge, WireMarker)` produces
`OpKind::AddRefEdge { category: Custom("circuit-wire"), .. }`; remote
`AddRefEdge` ops with that category arrive with `WireMarker` pre-attached
to the edge entity.

To add per-edge *properties*, give the marker a sibling component
deriving `SchemaSync`, then add a `SchemaSyncedEdgeComponentPlugin` for
it (analogous to the node-component plugin). The two layers compose.

### 7.3 Add a brand-new model

Use [`kyoso_comments_crdt`](../../../crates/kyoso_comments_crdt/) + [`kyoso_comments_sync`](../../../crates/kyoso_comments_sync/) as the reference. Five pieces:

1. **`OpKind` enum** — `kyoso_comments_crdt::CommentOpKind`. Define what operations exist (`AddComment { anchor, parent, body }`, `EditCommentBody { target, body }`, `DeleteComment { target }`, …).

2. **Backend type** implementing `CrdtModel`. `kyoso_comments_crdt::CommentsBackend` owns the in-memory state, `pending` queue, `applied_seq`, and a shared `IdGen` cloned from `PeerIdGen`. Implements `apply_remote`, `snapshot`, `restore`, `drain_pending`.

3. **Cross-model anchors are free.** A `Comment { anchor: CrdtId, ... }` field whose anchor is a graph node's id is safe because both models share the peer's `IdGen` and therefore the same `LocalSeq` namespace. No model tag.

4. **Server handler** implementing `RoomModelHandler` — for comments this is `CommentsRoomHandler` ([handlers/comments.rs](../../../apps/kyoso_server/src/services/handlers/comments.rs)). Owns an `InMemoryOpLog<CommentOpKind>`, an append-lock, and the mirror. Registered as a `HandlerFactory` in `AppState`.

5. **Bevy plugin** — `CommentsSyncPlugin` ([comments_sync/src/plugin.rs](../../../crates/kyoso_comments_sync/src/plugin.rs)). Mirrors `GraphSyncPlugin`'s structure: registers the model with `ModelRegistry`, owns a `CommentsClient` resource sharing `IdGen` with `PeerIdGen`, drains `WsInbound` for comments traffic on the inbound side, and drains `CommentsClient::drain_pending` on the outbound side.

The new model multiplexes onto the same WebSocket as the graph — no
new transport, no new server endpoint, just the new `ModelId` slug.

---

## 8 · Presence vs. storage

The conceptual split mirrors Yjs Awareness:

| | **Storage** | **Presence** |
|---|---|---|
| Lifetime | durable | until disconnect |
| Ordering | totally ordered, replayable | latest-wins per peer |
| Schema | structured | opaque `Vec<u8>` |
| Examples | nodes, edges, properties | cursor, selection, viewport, "is typing" |
| Replay on join | yes (snapshot + ops) | snapshot in Welcome, no replay |
| Persistence | Postgres / in-memory log | none |
| Bandwidth profile | bursty | steady, high frequency |

What's actually wired:

- Storage flows through per-model `Submit / Apply / Catchup / Ping` envelopes.
- Presence flows through three model-agnostic envelope variants: `ClientMsg::Presence(Vec<u8>)`, `ServerMsg::PresenceUpdate { peer, state }`, `ServerMsg::PresenceLeft { peer }`. The bytes are opaque — every consumer postcard-encodes their own struct (cursor + selection + display name + colour, etc.).
- Server-side, the presence map lives at the `Room` level
  ([room.rs:41](../../../apps/kyoso_server/src/services/room.rs#L41)), bypasses every handler, and is cleared on disconnect.

What's deliberately *not* wired (see §10):

- No heartbeat / timeout-based offline detection. The server learns
  about presence loss from a clean WS close.
- No per-peer presence clock. A `Presence` frame is a state replace.
- No WebRTC mesh for low-latency cursor updates. Every cursor move
  round-trips the server.

---

## 9 · Apps that consume the stack

### 9.1 `kyoso_client` — Figma-shaped 2D editor

`apps/kyoso_client` (binary at `src/bin/kyoso_client.rs`). The reference visual app: 2D scene graph with figma-style frames, rectangles, and text.

Wires:

- `KyosoFigmaPlugin` (in `kyoso_figma`) bundles `SyncTransportPlugin` + `GraphSyncPlugin<FigmaNode, FigmaEdge>` + per-component schema plugins for `Frame`, `Rectangle`, `Text`, `Size`, `TypeStyle`, `Transform`.
- `SyncedEdgeCategoryPlugin<FigmaNode, FigmaEdge, M>` for `ReferenceMarker`, `DependencyMarker`, `CommentMarker`, `AnnotationMarker`.
- A `PresencePlugin` (in `kyoso_client::presence`) that postcard-encodes cursor + selection.
- A `Tool` state machine and `AppCommand` / `AppEvent` external bus — covered in [`event_bus.md`](event_bus.md).

### 9.2 `kyoso_circuit_client` — 3D analogue circuit designer

`apps/kyoso_circuit_client`. The same architecture, different domain:

- `KyosoCircuitPlugin` (in `kyoso_circuit`) bundles transport + `GraphSyncPlugin<CircuitNode, CircuitEdge>` + schema plugins for `Resistor`, `Capacitor`, `Inductor`, `VoltageSource`, `Ground`, `Transform`, `OnLayer`.
- The consuming app adds `SyncedEdgeCategoryPlugin` per category (`WireMarker`, `SameNetMarker`, `DifferentialPairMarker`) — kept outside `KyosoCircuitPlugin` so the domain crate doesn't have an opinion on which subset of edge kinds an app wants.

The same `kyoso_server` instance hosts both apps. The only differences
on the wire are the `OpKind` payloads (which the server treats opaquely
within the graph model) and which `EdgeCategory` variants show up.

### 9.3 `kyoso_loadgen`

Crate at `crates/kyoso_loadgen`. Ships five binaries:

- `kyoso_loadgen` — concurrent WS clients driving graph / comments / mixed loads; measures submit→echo latency.
- `kyoso_chaos` — adds packet-drop, latency-injection, disconnect.
- `kyoso_harness` — orchestrator.
- `kyoso_wire_probe` — single-connection observer for debugging.
- `kyoso_peer_sweep` — peers-per-room scaling sweep.

Used by the `Justfile bench` harness to keep an eye on regressions.

---

## 10 · Known gaps & deferred decisions

Short, factual, no re-litigation. Items here are *acknowledged*; the
research/discussion for each lives in [`crdt.md`](crdt.md) at the linked
section.

- **No auto-reconnect, no offline op buffer.** A network drop today
  means the app sees `Disconnected` once and pending ops stay in
  `CrdtBackend::pending` until process restart (which loses them).
  Reconnection requires app-level intervention. See `crdt.md §3.3`.

- **No presence heartbeat or timeout-based eviction.** Presence loss is
  inferred only from a clean WS close. A peer with a broken connection
  appears live to others until the server's WS reader times out. See
  `crdt.md §5.1` for the Yjs-Awareness heartbeat design that's the
  intended path.

- **Outbound backpressure is `bool` from `submit`.** No queue-depth
  feedback, no rate limiting. Adequate for current workloads; needs
  attention if a peer ever has 10⁴ pending ops.

- **`Sequence<T>` is a naive `Vec`-backed stub.** Single-writer safe;
  concurrent edits lose data. The path to a real impl is
  Fugue / Eg-walker (`crdt.md §2.1`). For now, `#[crdt(sequence)]`
  fields work fine for single-author text but **must not be used for
  collaboratively-edited text**.

- **Branching deferred.** The `CrdtId`-as-stable-id design is the
  hook that keeps it possible — every element has a globally unique
  id that survives branch creation. The work needed to add branches is
  bounded (op `parents: Vec<CrdtId>`, branch-scoped `GlobalSeq`,
  causal-DAG replay instead of total-order) but not started. See
  `crdt.md §2.5`.

- **Comments storage is in-memory only.** `CommentsRoomHandler`'s log
  doesn't survive server restart. The schema is there for a persistent
  backend; the wiring isn't.

- **No WebRTC presence mesh.** Every cursor move round-trips the
  server. See `crdt.md §5.2` for the design.

- **Per-edge-category `RefEdgePolicy` is metadata only.** Every reference
  edge currently behaves as `OrSet` + `Cascade` regardless of
  `EdgeCategory`. The `RefEdgeCrdt` trait
  ([edge_category.rs:123](../../../crates/kyoso_graph_crdt/src/edge_category.rs#L123)) is the hook for per-category divergence;
  no impl has been wired beyond `Reference`.

---

## 11 · Pointer table

| Topic | Code | Deep dive |
|---|---|---|
| `CrdtId` / `IdGen` | [kyoso_crdt::id](../../../crates/kyoso_crdt/src/id.rs) | `crdt.md §1`, Part II.2.3 |
| `Op<K>` / `Diff<K>` | [kyoso_crdt::op](../../../crates/kyoso_crdt/src/op.rs) | `crdt.md §1` |
| `Lattice` / `Crdt` | [kyoso_crdt::lattice](../../../crates/kyoso_crdt/src/lattice.rs) | `crdt.md` Part II.2 |
| `CausalContext` / `SubDot` | [kyoso_crdt::context](../../../crates/kyoso_crdt/src/context.rs) | `crdt.md` Part II.2.3 |
| `WireDelta` / `Path` | [kyoso_crdt::delta](../../../crates/kyoso_crdt/src/delta.rs) | `crdt.md` Part III, Phase D |
| Base primitives | [kyoso_crdt::types](../../../crates/kyoso_crdt/src/types/) | `crdt.md` Part II.2.4 |
| `CrdtModel` | [kyoso_crdt::model](../../../crates/kyoso_crdt/src/model.rs) | `crdt.md §1` |
| Wire protocol | [kyoso_crdt::protocol](../../../crates/kyoso_crdt/src/protocol.rs), [kyoso_crdt::envelope](../../../crates/kyoso_crdt/src/envelope.rs) | `crdt.md §3` |
| `#[derive(Crdt)]` | [kyoso_crdt_derive](../../../crates/kyoso_crdt_derive/src/lib.rs) | `crdt.md` Phase G |
| `#[derive(SchemaSync)]` | [kyoso_sync_derive](../../../crates/kyoso_sync_derive/src/lib.rs) | `crdt.md` Phase G |
| Graph op kinds | [kyoso_graph_crdt::op](../../../crates/kyoso_graph_crdt/src/op.rs) | `crdt.md` Part II.1.2, II.1.3 |
| Edge categories | [kyoso_graph_crdt::edge_category](../../../crates/kyoso_graph_crdt/src/edge_category.rs) | `crdt.md` Part II.1 |
| `CrdtBackend` | [kyoso_graph_crdt::backend](../../../crates/kyoso_graph_crdt/src/backend.rs) | `crdt.md` Part IV |
| `Document<S>` | [kyoso_graph_crdt::document](../../../crates/kyoso_graph_crdt/src/document.rs) | `crdt.md` Phase H |
| `GraphSyncPlugin` | [kyoso_graph_sync::plugin](../../../crates/kyoso_graph_sync/src/plugin.rs) | `event_bus.md §1.3` |
| `EntityCrdtIndex` | [kyoso_graph_sync::index](../../../crates/kyoso_graph_sync/src/index.rs) | `crdt.md` Part IV |
| `ClientSyncEngine` | [kyoso_graph_sync::engine](../../../crates/kyoso_graph_sync/src/engine.rs) | `crdt.md` Phase H, Part IV |
| `SchemaSyncedNodeComponentPlugin` | [kyoso_graph_sync::schema_sync](../../../crates/kyoso_graph_sync/src/schema_sync.rs) | `crdt.md` Phase G–H |
| `SyncedEdgeCategoryPlugin` | [kyoso_graph_sync::category](../../../crates/kyoso_graph_sync/src/category.rs) | `crdt.md` Part II.1 |
| Transport (`WsClient`, `WsBridge`) | [kyoso_sync](../../../crates/kyoso_sync/src/) | `crdt.md §3`, `event_bus.md §1.4` |
| Server (`Room`, `OpStore`) | [apps/kyoso_server](../../../apps/kyoso_server/src/) | `crdt.md §3.1`, `event_bus.md §1.2` |
| Comments model | [kyoso_comments_crdt](../../../crates/kyoso_comments_crdt/src/), [kyoso_comments_sync](../../../crates/kyoso_comments_sync/src/) | `memo.md` |

[envelope]: ../../../crates/kyoso_crdt/src/envelope.rs
[globalseq]: ../../../crates/kyoso_crdt/src/id.rs
[move-op]: ../../../crates/kyoso_graph_crdt/src/op.rs
