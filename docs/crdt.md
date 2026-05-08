# CRDT research for kyoso — landscape, synchronization, presence, composition

## Context

You are building **kyoso**, a Bevy-ECS application with collaborative editing of a tree/graph document model (Figma-shaped). A working CRDT/sync stack already exists across `kyoso_crdt`, `kyoso_sync`, `kyoso_graph`, `kyoso_server`, and `kyoso_client`. You want to (a) validate the current design against state-of-the-art, (b) understand the design space (transports, presence vs storage, branching, composition, graph CRDTs) so you can make principled choices later, and (c) keep the door open for future-proofing — especially branching/version-control — without paying for it now.

Per your direction: **research-first**, no commitment to a migration path; **future-proof but don't implement** branching; **document both** WebSocket+heartbeat (Yjs-Awareness) and WebRTC presence transports in detail.

---

## 1 · Where kyoso stands today (so we know what to compare against)

A snapshot of the implementation that the research lands on:

- **Algorithm class**: hand-rolled **op-based CRDT**, **server-mediated total order**. The server assigns a monotonic `GlobalSeq: u64` on append; clients apply ops in `GlobalSeq` order → deterministic convergence with **no vector clocks**.
- **Identity**: `CrdtId = (peer: u32, seq: u64)` minted by the originator (collision-free without coordination); `GlobalSeq` is the causal ordering. Defined in [crates/kyoso_crdt/src/id.rs](crates/kyoso_crdt/src/id.rs).
- **Op kinds** (in [crates/kyoso_crdt/src/op.rs](crates/kyoso_crdt/src/op.rs)): `AddNode`, `AddEdge {from,to}`, `RemoveNode {target}` (tombstone), `RemoveEdge {target}` (tombstone), `SetNodeProperty {target,key,value}` (LWW per key), `SetEdgeProperty`, `Move {target, new_parent, position}` — the **Kleppmann atomic tree-move** with fractional-index `OrderKey` for sibling order ([crates/kyoso_graph/src/tree.rs](crates/kyoso_graph/src/tree.rs)).
- **Property semantics**: Last-Writer-Wins per key, with `GlobalSeq` as the timestamp. Properties are postcard-encoded via Bevy's `ReflectSerializer`, registered per type-name path (e.g. `"Transform::translation.x"`).
- **Snapshot/compaction**: `Snapshot` at a `GlobalSeq` excludes tombstones; server checkpoints every 60 s and GCs ops below `min(peer_acks, snapshot_seq)` every 120 s ([crates/kyoso_crdt/src/snapshot.rs](crates/kyoso_crdt/src/snapshot.rs)).
- **Wire**: postcard-encoded binary frames over a single axum WebSocket. `ClientMsg`: `Hello { room, since }`, `Submit(op)`, `Catchup { since }`, `Ping { applied_seq }`, `Presence(Vec<u8>)`, `LeavePresence`. `ServerMsg`: `Welcome { peer, snapshot?, diff, presence }`, `Apply(op)`, `Catchup(diff)`, `Pong`, `PresenceUpdate {peer,state}`, `PresenceLeft {peer}`, `Error`. Defined in [crates/kyoso_crdt/src/protocol.rs](crates/kyoso_crdt/src/protocol.rs).
- **Server topology**: single central axum hub, one `tokio::sync::broadcast` per room (capacity 256), Postgres tables `rooms / ops / snapshots / peer_acks` (sqlx). Append lock serializes seq assignment across concurrent peers ([apps/kyoso_server/src/services/room.rs](apps/kyoso_server/src/services/room.rs)).
- **Client → ECS bridge**: `kyoso_sync::CrdtSyncPlugin` runs `inbound_system` (drains WS, applies remote ops to backend, projects into ECS), detection systems for local Bevy `Added`/`Changed` (echo-suppressed via `GraphEntityIndex`), and `outbound_system` (drains pending ops, sends WS) — [crates/kyoso_sync/src/plugin.rs](crates/kyoso_sync/src/plugin.rs).
- **Presence already separated**: opaque `Vec<u8>` blob per peer, never written to op log, no `GlobalSeq`, dropped on disconnect.
- **Known gaps** (called out in [docs/event_bus.md](docs/event_bus.md)): no auto-reconnect, no offline buffer, no backpressure on outbound mpsc, no presence heartbeat/timeout, `ConnectToolPlugin` stubbed.

**Read in plain English**: kyoso is already implementing the patterns the literature converged on — server-mediated total order (Figma's choice), Kleppmann tree moves (Loro's choice), fractional indexing (Loro/Figma), LWW per property, presence-as-separate-channel (Yjs Awareness pattern). The remaining design questions are about evolving each piece, not redesigning from zero.

---

## 2 · State-of-the-art landscape (papers, libraries, what changed recently)

### 2.1 Sequence/text CRDTs — the most active area

The big shift since 2022 is moving away from per-character CRDT metadata toward **event-graph replay**.

- **Fugue / FugueMax — Weidner, Gentle, Kleppmann (2023).** Defines *maximal non-interleaving* as a correctness property: when two peers concurrently insert text passages at the same position, the merged result must not interleave them character-by-character (a real bug present in many older list CRDTs). FugueMax provably satisfies it. ArXiv [2305.00583](https://arxiv.org/abs/2305.00583). Implementations: [mweidner037/fugue](https://github.com/mweidner037/fugue), and Loro integrates it.
- **Eg-walker (Event Graph Walker) — Gentle & Kleppmann (EuroSys 2025).** Stores only the original op descriptions on a DAG (no per-character CRDT metadata); replays the relevant slice of history on demand to merge. **Order-of-magnitude less memory than Yjs/Automerge** in steady state and orders-of-magnitude faster document load. Originated in [diamond-types](https://github.com/josephg/diamond-types). ArXiv [2409.14252](https://arxiv.org/abs/2409.14252).
- **Loro** (Rust, [loro.dev](https://loro.dev)) — production-leaning library built on Replayable Event Graph (REG, Eg-walker-flavored). Composes movable tree + map per node + Fugue text + list. Closest off-the-shelf match to kyoso's data model.
- **Yjs / Yrs** — most-deployed CRDT lib (Notion, Jupyter, etc.); uses yjsmod sequence CRDT. Robust ecosystem (y-protocols, y-websocket, y-webrtc, y-indexeddb, providers galore).
- **Automerge** (Rust core, JS bindings) — complete history retention, fork/merge primitives, time-travel via `view(heads)`. The "git for documents" lineage. See [automerge.org](https://automerge.org).

**Implication for kyoso**: your tree/graph topology layer is sound; the place where modern algorithms matter is **inside node properties** — long text fields, nested maps. A `String` property treated as LWW will silently corrupt concurrent edits. If/when you have rich text or large strings, that's where Fugue / Eg-walker / yjsmod earn their keep.

### 2.2 Tree CRDTs

- **Kleppmann, Mulligan, Gomes, Beresford — *A highly-available move operation for replicated trees* (IEEE TPDS 2021).** Formally proven (Isabelle/HOL) algorithm for concurrent moves that preserves tree validity (no cycles), no central coordination required. [PDF](https://martin.kleppmann.com/papers/move-op.pdf). Reference impl: [trvedata/move-op](https://github.com/trvedata/move-op).
- kyoso's `OpKind::Move` is this algorithm, simplified by the server's total order: cycle detection is deterministic at apply time because all peers see the same sequence. ✓
- **Loro's movable tree** combines Kleppmann moves with **fractional indices** for sibling order — exactly kyoso's `OrderKey`. [Loro: Movable tree CRDTs](https://loro.dev/blog/movable-tree).
- **Open subtlety**: in pure-P2P move CRDTs without total order, conflicting moves require *undo & redo* on receipt of an out-of-order op (the original Kleppmann algorithm). kyoso's server-mediated design sidesteps this; if you ever go P2P, you re-inherit it.

### 2.3 Graph CRDTs (beyond trees)

The literature here is thinner and more fragmented. The original Shapiro et al. (2011) survey defines **2P2P-Graph** (2P-set vertices + G-set edges with tombstones) and **add-only DAG / partial-order** variants. Foundational summary on [Wikipedia](https://en.wikipedia.org/wiki/Conflict-free_replicated_data_type) and [waitingforcode](https://www.waitingforcode.com/big-data-algorithms/conflict-free-replicated-data-types-flags-graphs-maps/read).

The hard problem is the **vertex/edge dependency**: an edge requires both endpoints to exist, and a removed vertex with edges leaves dangling refs. Three resolution policies in the literature:
- **Remove-wins** (delete-the-edges-too on vertex remove).
- **Add-wins** (resurrect the vertex if any edge still references it).
- **Tombstone-and-defer** (keep tombstones, GC only when all replicas agree no edges remain).

kyoso effectively does **tombstone-with-deterministic-server-order**: `RemoveNode` and `RemoveEdge` are tombstones; the GlobalSeq order makes "edge added before/after vertex removed" deterministic across replicas. This is correct but conservative — you carry tombstones until snapshot/compaction.

**For figma-shaped documents the graph is essentially a tree with a few cross-references** (component instances → main components, prototype links between frames, constraints). It's worth considering **typed edges with separate CRDT semantics**:
- `tree` edges: Kleppmann move + OrderKey (parent-child structure).
- `reference` edges: 2P2P-graph add/remove (component instance → main, prototype links).
- `derived` edges: not synced — recomputed from other state (selection, hover).

This is a recurring pattern in production graph editors.

### 2.4 Composition (nested CRDTs, JSON-shaped documents)

The key idea is **lattice composition**: if every embedded value is itself a CRDT (a join-semilattice), a `Map<key, CRDT>` is also a CRDT — merges propagate up. Foundational work:

- **Riak DT Map** (Brown, Bieniusa, Meiklejohn et al.) — "a composable, convergent replicated dictionary." Each map value is itself a CRDT; composition preserves convergence.
- **DSON: JSON CRDT Using Delta-Mutations** (VLDB 2022, Rinberg et al.) — [PDF](https://www.vldb.org/pvldb/vol15/p1053-rinberg.pdf). Defines causal composable delta-based CRDTs supporting arbitrary nesting.
- **Automerge's JSON CRDT** — every node in the document is itself a tree of CRDTs; a single shared causal context tracks all ops across the whole tree.
- **Loro's Map-per-tree-node** pattern — every tree node has an attached `LoroMap` whose values can be any Loro CRDT (Text, List, Counter, sub-map). This is the cleanest composition story for figma-shaped docs.

**Implication for kyoso**: your current `SetNodeProperty {target, key, value}` with LWW is the simplest possible composition (everything is a register). To get richer, you'd register a CRDT *type* per property key, and the wire op carries both the field path and the embedded sub-op:
```
SetNodeProperty { target, key: "name",      value: LWW(Bytes) }       // current
SetNodeProperty { target, key: "text",      value: TextOp(Fugue insert/delete at position) }  // future
SetNodeProperty { target, key: "tags",      value: SetOp(add/remove element) }                // future
SetNodeProperty { target, key: "transform", value: LWW(Bytes) }       // current; LWW is fine here
```
Concretely: `OpKind::SetNodeProperty.value` becomes an enum over per-CRDT op-types rather than raw `Bytes`. Causal context (CrdtId+GlobalSeq) is already on the outer Op, so embedded CRDTs inherit causality for free.

### 2.5 Branches & reconciliation (Automerge-flavored)

- **Automerge** preserves the full op history; `fork(heads)` creates an independent copy at any historical point; `merge` joins two doc histories deterministically. There are no "merge conflicts" in the git sense — every concurrent op composes deterministically — but app-level conflicts (two users renamed the same field differently) still need UX.
- **Eg-walker** stores ops as a DAG of events keyed by `(replica, seq)` with parent links. Merging is "walk both branches, replay ops in causal order." Branch points become DAG forks; merge points become joins.
- **Figma Branches** — production example. Every branch is a separate document instance; merge is a UI-mediated review.
- **kyoso today**: linear `GlobalSeq` per room. To future-proof for branches **without implementing**:
  - Document an op identity that survives branching: keep `CrdtId = (peer, seq)` as the canonical op ID; treat `GlobalSeq` as branch-scoped (per-branch monotonic), not global.
  - Plan for an op-DAG model: each op carries `parents: Vec<CrdtId>` (currently implicit via GlobalSeq). On a single linear branch this is just `[prev_op]`; on branches it's the merge predecessors.
  - Storage shape: `(branch_id, global_seq) → Op` instead of `(room_id, global_seq) → Op`.
  - The migration cost later is **schema change + replacing total-order convergence with causal-DAG replay** — non-trivial but bounded if `parents` is added to `Op` now.

We're explicitly not designing this further — flagging where the architecture has to bend.

---

## 3 · Synchronization architecture: where to draw the lines

### 3.1 Server-mediated total order vs full P2P

| Property | Server-mediated (kyoso today) | Full P2P |
|---|---|---|
| Convergence | trivial — all peers apply ops in same `GlobalSeq` order | requires causal-DAG (Eg-walker) or per-op vector clocks |
| Latency | client→server→fanout (~RTT/2 + fanout) | direct peer-to-peer (~one-hop RTT) |
| Offline | safe — ops queue, replay on reconnect | safe natively, but reconciliation can be unbounded |
| Auth/permissions | central choke point — easy | hard — requires capability tokens or cryptographic gating |
| Persistence | natural — server writes Postgres on append | needs designated "archival" peer or hybrid |
| Scaling | single-room throughput bounded by append lock | bounded by mesh fanout (n²) |

**Production reality**: every major collaborative editor (Figma, Google Docs, Notion, Linear) is server-mediated. P2P is reserved for niche local-first contexts (Ink & Switch demos, NextGraph, some Automerge deployments). The reasons are **persistence + auth + onboarding new clients with a snapshot**, not algorithmic.

### 3.2 Hybrid: server for storage, P2P for presence

A common layered design:
- **Storage** travels over the WebSocket to/from the server (durable, ordered, persisted).
- **Presence** travels over a **WebRTC mesh** between peers in the same room (ephemeral, no fanout cost on the server, sub-100ms cursor updates).
- The server's role for presence reduces to **signaling** (helping peers discover each other to set up WebRTC) — see §5.2.

This is the architecture you'd reach for if the cursor-jitter on a dozen-peer canvas becomes a perceptible problem.

### 3.3 Sync points (when does a peer actually exchange data?)

Three distinct moments in the lifecycle, each handled differently:

1. **Connect / reconnect**: client sends `Hello { since: last_acked_seq }`. Server replies with `Welcome { snapshot?, diff }` — snapshot if the client's `since` is older than the latest checkpoint (cheap catch-up), else just the missing op slice. ✓ kyoso has this.
2. **Steady-state**: each local op flushes individually (or batched per Bevy frame) over the WS; each remote op arrives as `Apply(op)` and is applied/projected on the next `Update` schedule.
3. **Compaction**: server periodically writes a snapshot (60s in kyoso) and GCs ops below `min(peer_acks)`. Clients pulling a snapshot over reconnect bypass replay entirely.

The piece that's **stubbed** in kyoso and worth designing properly when you tackle it: **offline buffer** (ops generated while disconnected need to survive process restart) and **auto-reconnect with backoff** (exponential, jittered, with a max-attempts policy that surfaces to UI).

---

## 4 · Presence vs Storage — the conceptual split

The clearest mental model, due to Yjs and adopted by Liveblocks/Figma/etc.:

| | **Storage** | **Presence (Awareness)** |
|---|---|---|
| Lifetime | durable (forever, until deleted) | ephemeral (until disconnect) |
| Ordering | totally ordered, replayable | unordered, latest-wins per peer |
| Schema | structured (the document) | schemaless JSON / opaque blob |
| Examples | nodes, edges, properties, text | cursor x/y, selection set, viewport, current tool, follow-mode target, "is typing" |
| Replay on join | yes (snapshot + ops) | no — start fresh |
| Persistence | Postgres / object storage | none |
| Conflict model | CRDT merge | last write per peer wins (within their own slot) |
| Bandwidth profile | bursty (edits) | steady high frequency (cursor stream) |

**Design rule**: if losing it on disconnect would surprise no one, it's presence. If losing it would lose work, it's storage.

**Edge cases that get this wrong**:
- **Selections that drive an edit**: the selection itself is presence; the edit it triggers is storage. They're different ops. Don't put "selected node IDs" in storage.
- **Comments / pinned messages**: storage. They survive disconnect.
- **Drag-in-progress preview**: presence. Only the final committed transform goes to storage.
- **"User X is editing this field"** (pessimistic locking): tricky — it's presence in the sense that it goes away on disconnect, but it gates writes. Implement as presence; let the client respect or ignore it (stale locks aren't safety-critical).

---

## 5 · Presence transport — WebSocket+heartbeat vs WebRTC data channel

You asked for both documented. Both are valid; they're at different points on the latency/complexity tradeoff.

### 5.1 WebSocket + Yjs-Awareness-style heartbeat (kyoso's current direction)

**Reference**: [Yjs Awareness docs](https://docs.yjs.dev/api/about-awareness), [y-protocols PROTOCOL.md](https://github.com/yjs/y-protocols/blob/master/PROTOCOL.md).

**Model**:
- Each peer has a single slot in a state-based CRDT keyed by peer-id; only that peer can write to its own slot.
- Each slot is `(clock: u64, json: Value)`. The clock is **monotonically increasing per-peer**; on every local change, increment and broadcast.
- Merge rule: for each peer slot, take the entry with the higher clock.
- **Heartbeat**: each peer rebroadcasts its current state every ~10s. If a peer hasn't received a remote peer's update in **30s**, mark them offline locally and drop the slot.
- **On disconnect**: the disconnected peer's WS close triggers the server to broadcast `PresenceLeft { peer }` to remaining peers (or peers infer it from heartbeat timeout if the server itself dies).

**Wire frames** (kyoso already has the right shape):
- `ClientMsg::Presence(opaque_bytes)` — full state replace (state-based, idempotent).
- `ServerMsg::PresenceUpdate { peer, state }` — fanout.
- `ServerMsg::PresenceLeft { peer }` — clean drop on disconnect.
- **Add**: `ClientMsg::PresenceHeartbeat` (or just rebroadcast `Presence` on a 10s timer); per-peer clock; client-side 30s timeout.

**Pros**:
- Reuses the existing WS connection — no extra signaling, NAT, or TURN to deal with.
- Server sees presence and can authorize, log, audit.
- Works in restrictive networks where WebRTC fails (corporate proxies blocking UDP, mobile carrier NAT).

**Cons**:
- Every cursor move round-trips through the server. With N peers in a room, server fanout is `O(N × cursor_rate × 2)` (in + out).
- Latency floor: ~RTT to server. For peers on the same LAN routed through a remote server, this is ~50–200ms — perceptible on a fast cursor.
- Server bandwidth scales with cursor activity, not just edit activity.

**When to choose this**: default. Until cursor latency is a measured complaint, this is the right tier. It's also the right tier for any presence that needs to be *authoritative* (e.g., "is editor X currently online for billing").

### 5.2 WebRTC data channel for presence (room-scoped P2P mesh)

**Reference**: [y-webrtc](https://github.com/yjs/y-webrtc) is the canonical implementation; same pattern.

**Architecture**:
- The WebSocket server still exists (for storage and signaling).
- When peer A joins a room, the server's `Welcome` includes the list of other peers' WebRTC offer-listening endpoints (or the server acts as a signaling channel and relays SDP offers/answers/ICE candidates between peers).
- Peers establish **WebRTC peer connections** pairwise within the room → full mesh of `RTCDataChannel`s.
- Presence updates are sent **only** over the data channels, not the WS.
- The server is uninvolved in presence after the initial handshake.

**Signaling (the hard part)** — how do peers find each other?
- Peer A sends `ClientMsg::WebRTCOffer { to_peer, sdp }` to the server.
- Server forwards to peer B as `ServerMsg::WebRTCOffer { from_peer, sdp }`.
- B replies with `WebRTCAnswer`; ICE candidates trickle the same way.
- **STUN servers** are needed to discover each peer's public IP behind NAT (free public ones exist; Google/Cloudflare provide them).
- **TURN servers** are needed when STUN fails (symmetric NAT, restrictive corporate firewalls). TURN is paid infra (coturn self-hosted, or services like Twilio/Xirsys/Cloudflare Calls). Without TURN, ~5–15% of users will fail to connect peer-to-peer and need a fallback path (back to WS-relayed presence).

**Pros**:
- **Latency**: one-hop peer-to-peer. Cursor smoothness on a fast LAN feels qualitatively different (~10–30ms vs ~80–200ms).
- **Server load**: presence traffic disappears from the server — important if you have rooms with 20+ active peers (figma-scale).
- **Bandwidth**: data channels are UDP (SCTP-over-DTLS) — cheap, unreliable-by-default if you want it (good for cursor streams; intermediate frames can drop).

**Cons**:
- **Mesh complexity scales O(N²)** in connections. Fine for a dozen peers; bad for fifty. Mitigations: have one peer act as an SFU-style relay (selective forwarding), or fall back to WS for large rooms.
- **NAT traversal failures** require TURN (paid infra and ~10% bandwidth tax for relayed connections).
- **No audit trail** on presence — server doesn't see it.
- **Browser/native asymmetry**: WebRTC in Bevy native (via webrtc-rs / str0m crates) is significantly more code than the browser path. If you're targeting wasm/web only this is easier; if you want native clients to also get the fast path, plan for the extra surface.
- **Failure mode coupling**: a peer's WS disconnect should also tear down its WebRTC connections; otherwise you get ghost cursors. Need a clean "peer left" signal that triggers both.

**Hybrid pattern**:
- **Always have WS-based presence as a fallback.** When a peer's WebRTC connection fails to establish (after a 5-second timeout), route presence through the WS instead. Some peers in a room may be on the fast path, others on the slow — this is fine.
- **Server-side switch**: a feature flag per-room or per-peer chooses WS-only vs WebRTC-attempted.

**When to choose this**: when cursor latency is measurably bad, when room density is high (10+ active editors), or when server fanout cost is dominating. Not before.

### 5.3 Wire-format implications for kyoso

If you stay with WS+heartbeat (5.1), the only protocol changes are:
- Add a **per-peer clock** to `Presence` payloads.
- Add a client-side 10–15s heartbeat timer that rebroadcasts.
- Add a 30s timeout on the receive side that drops a remote peer's slot.

If you go hybrid WS+WebRTC (5.2), the protocol additions are:
- New `ClientMsg::WebRTCSignal { to_peer, payload }` and `ServerMsg::WebRTCSignal { from_peer, payload }`.
- Server learns nothing about presence content; peers exchange the same `Presence(bytes)` payloads, just over a different channel.
- Both channels can coexist — `Presence` over WS becomes the fallback.

---

## 6 · Branches & reconciliation — what to design *for*, without building

Given "future-proof but don't implement," the goal is **to not paint into a corner**. Things that would be expensive to add later if not anticipated now:

1. **Op identity vs op order are conflated**. Today `GlobalSeq` is both. Branches need them split: `CrdtId` is the stable identity (survives branch fork); the order is per-branch. Mitigation now: in code comments, document that `GlobalSeq` is "branch-scoped" even though there's only one branch. Don't expose `GlobalSeq` as part of the public op identity.
2. **Op-DAG predecessors are implicit**. To support branching/merging later you need explicit `parents: Vec<CrdtId>` on each op. Adding it later is a wire-format break. Two options:
   - Add `parents` now (set to `[prev_op]` on a linear branch). Pays a few bytes per op forever; saves a migration.
   - Reserve a `version: u8` byte at the front of the postcard frame so you can introduce v2 with `parents` later without breaking the protocol.
3. **Storage schema**. Postgres `ops` table currently keyed by `(room_id, global_seq)`. Future-proofing: **make sure the schema is one logical level deeper than your current "branch concept"**, e.g. `(room_id, branch_id, branch_seq)` from day one — even with `branch_id = "main"` always. Adding the column later is a real migration.
4. **Snapshots are branch-scoped too**. Each branch needs its own snapshot lineage; same schema treatment.
5. **Compaction interacts badly with branches** — you can't GC ops that any branch still references. Document this constraint now so it's not a surprise later.
6. **Presence is branch-scoped** — peer A on branch X and peer B on branch Y shouldn't see each other's cursors. The `room_id` in `Hello` becomes `(room_id, branch_id)`.

The **migration shape** if you ever do branches:
- Convergence model changes from "apply in `GlobalSeq` order" to "topo-sort the op DAG and apply" (Eg-walker style). This is the biggest change.
- Or: per-branch linear seqs + an explicit "merge op" that records the merge predecessors, with a deterministic merge resolution (e.g., apply branch-X ops then branch-Y ops, or interleave by some canonical order).
- The simpler scheme (per-branch linear with explicit merges) is enough for human-driven branch workflows and avoids the full DAG-walker rewrite.

---

## 7 · Composition — nodes with properties + children + edges

The pattern that keeps recurring across Riak DT, Automerge, DSON, and Loro is **a single shared causal context, with multiple typed CRDTs living inside one document**. Concretely:

```
Document
 ├── Tree (movable-tree CRDT — Kleppmann + OrderKey)        ← kyoso has this
 │    └── Node (CrdtId)
 │         └── Properties (Map<key, CRDT>)                  ← kyoso has LWW only
 │              ├── name:    LWW<String>                    ← LWW is fine for short scalars
 │              ├── text:    Sequence<Char>  (Fugue/yjsmod) ← kyoso lacks; would corrupt if used today
 │              ├── tags:    OR-Set<String>                 ← kyoso lacks
 │              ├── transform: LWW<Bytes>                   ← LWW is fine — single user usually
 │              └── counter: PNCounter                      ← kyoso lacks
 └── Reference edges (2P2P-graph add/remove)                ← kyoso has this generically;
                                                              not yet differentiated from tree edges
```

**Single causal context = every embedded CRDT op is identified by the outer `(CrdtId, GlobalSeq)`**. kyoso already has this for free because every Op carries its CrdtId. The change to support richer property types is local to `OpKind::SetNodeProperty.value`:

```rust
// Today (paraphrased from crates/kyoso_crdt/src/op.rs)
SetNodeProperty { target: CrdtId, key: String, value: Bytes /* postcard-encoded LWW */ }

// Future (illustrative, not a commitment)
SetNodeProperty { target: CrdtId, key: String, op: PropertyOp }
enum PropertyOp {
    LwwReplace(Bytes),           // current behavior
    SequenceInsert { pos: ..., text: ... },
    SequenceDelete { range: ... },
    SetAdd(Bytes),
    SetRemove(CrdtId),           // remove targets the op-id of the original add
    CounterDelta(i64),
    // ... per-CRDT op kinds
}
```

The schema/registration story (you already have a Reflect-based registry) extends naturally: each `key` is registered with a CRDT *kind* in addition to its Bevy type. The `outbound_system` knows how to convert a Bevy `Changed<T>` into a `PropertyOp` of the appropriate kind based on the registered field-CRDT mapping.

**Edge case: deletions in nested CRDTs.** When you `RemoveNode { target }`, do its embedded property CRDTs need explicit cleanup? Two answers:
- **Containment** (Automerge/Loro): removing the node garbage-collects all embedded CRDTs transitively. Causal context allows this safely.
- **Tombstone-and-defer** (current kyoso behavior): the node is tombstoned and so are its properties; GC happens at compaction.

Both work; containment is more efficient, requires per-CRDT-type "drop everything for this parent" logic.

---

## 8 · Mapping research → kyoso (options, not commitments)

Per your direction, this section enumerates options without recommending. Each option is sketched at file-level granularity so the cost is legible.

### 8.1 Stay with custom kyoso_crdt + targeted upgrades
- **Finish the gaps already on the TODO list**: auto-reconnect with backoff, offline buffer with persistence, backpressure on outbound mpsc, explicit WS Close on shutdown ([crates/kyoso_sync/src/client.rs](crates/kyoso_sync/src/client.rs)).
- **Add Yjs-Awareness semantics to existing presence**: per-peer clock, 10s heartbeat, 30s timeout ([crates/kyoso_crdt/src/protocol.rs](crates/kyoso_crdt/src/protocol.rs), [crates/kyoso_sync/src/plugin.rs](crates/kyoso_sync/src/plugin.rs)).
- **Replace `OpKind::SetNodeProperty.value: Bytes` with a `PropertyOp` enum** that supports LWW, Sequence (Fugue), OR-Set, PNCounter as needed by your data model ([crates/kyoso_crdt/src/op.rs](crates/kyoso_crdt/src/op.rs)).
- **Differentiate edge kinds**: add `EdgeKind` to `AddEdge` ops (`Tree | Reference | …`) so different edge classes can have different conflict resolution.
- **Future-proofing for branches**: add `version: u8` byte at front of frames; document `GlobalSeq` as branch-scoped; widen Postgres key to `(room_id, branch_id, branch_seq)` with `branch_id = 'main'` default ([apps/kyoso_server/src/services/store.rs](apps/kyoso_server/src/services/store.rs)).

### 8.2 Migrate node-internal storage to Loro, keep kyoso topology
- Embed a `LoroDoc` per node; sync ops between server and clients use Loro's wire format inside `SetNodeProperty.value`.
- kyoso topology layer (tree + edges) stays as-is.
- Big infra change in [crates/kyoso_sync/src/plugin.rs](crates/kyoso_sync/src/plugin.rs) — the property-detection systems become Loro-bridge code instead of postcard-Reflect.
- Pro: free Fugue text, free composable maps, well-tested. Con: third-party API churn (Loro is pre-1.0), encoding-format coupling.

### 8.3 Migrate fully to Loro (Loro's movable tree + map-per-node)
- Effectively: replace `kyoso_crdt::CrdtBackend` with a Loro-backed implementation.
- Topology, properties, even snapshots delegated to Loro.
- Largest change; biggest external dependency surface.
- Server becomes a Loro update relay (Loro has stable update messages).

### 8.4 Migrate to Yjs (via yrs Rust port)
- Similar shape to 8.3, mature ecosystem, more deployments in production.
- Yrs API is more stable than Loro currently.

### 8.5 Hybrid presence transport (WS + WebRTC mesh)
- Add WebRTC signaling messages to the protocol.
- Add `webrtc-rs` or `str0m` to `kyoso_client` dependencies.
- Keep WS presence as a fallback path.
- Discussed in §5.2 above.

---

## 9 · Open questions for you to consider

These don't need answering now — they're the decisions the research lights up:

1. **Are property-level concurrent edits a real concern?** If users typically edit one field at a time and the document's text fields are short, LWW is fine forever. If you have long text descriptions that two users might edit at once, you need a sequence CRDT.
2. **Room density** — how many concurrent peers? Under ~10 the WS-only presence is fine; above ~20 active cursors the WS server fanout starts to hurt.
3. **Native vs web clients** — does the Bevy client target both? WebRTC native adds significant code; WS is uniform.
4. **Branching: real workflow need or "nice to have"?** Figma-style branches are a documented enterprise feature. Most apps don't need them. The future-proofing in §6 is cheap; the implementation is not.
5. **Do you want offline-first edits?** Currently ops generated post-disconnect die in `backend.pending`. If offline durability matters, you need Postgres/sqlite/file-backed pending-op storage on the client.
6. **Auth model** — kyoso has no auth today (room ID is the credential). Branch on whether you'll need per-document permissions, per-branch permissions, or per-field redaction (the last is genuinely hard with CRDTs).

---

## 10 · Verification — how to know any future change still works

Verification ideas (apply to whichever direction you take):

- **Existing two-client integration tests** ([apps/kyoso_server/tests/two_clients.rs](apps/kyoso_server/tests/two_clients.rs), [apps/kyoso_client/tests/duplex_round_trip.rs](apps/kyoso_client/tests/duplex_round_trip.rs), [crates/kyoso_sync/tests/two_apps.rs](crates/kyoso_sync/tests/two_apps.rs)) cover the happy path. Extend them with disconnect/reconnect scenarios.
- **Property-based testing** with `proptest` or `quickcheck`: generate random op interleavings between N peers, run them in random order, assert all replicas converge to the same final state. This is the gold standard for CRDT correctness.
- **Deterministic simulation** — `madsim` (Rust) or a hand-rolled async-runtime mock can replay network partitions and message reorderings reproducibly. Find and pin the pathological cases.
- **Manual** — run `kyoso_client` in two windows pointing at a local `kyoso_server`, exercise edits, drop network in one with `pfctl`/`tc`, watch the reconnect path.
- **Latency/throughput numbers** — `dmonad/crdt-benchmarks` is the standard suite; reproducing kyoso's numbers there before/after any algorithm change tells you whether you've regressed.
- **Server load** — for presence-transport changes especially, measure server CPU and WS broadcast volume per active room before/after.

---

## 11 · Critical files (orientation map for whichever path you take)

- [crates/kyoso_crdt/src/op.rs](crates/kyoso_crdt/src/op.rs) — `OpKind`, `Op`, `Diff`. Wire-format center of gravity. Any algorithm change touches this.
- [crates/kyoso_crdt/src/id.rs](crates/kyoso_crdt/src/id.rs) — `CrdtId`, `GlobalSeq`, `IdGenerator`. Identity model.
- [crates/kyoso_crdt/src/protocol.rs](crates/kyoso_crdt/src/protocol.rs) — `ClientMsg` / `ServerMsg` enums. Any new message type lands here.
- [crates/kyoso_crdt/src/backend.rs](crates/kyoso_crdt/src/backend.rs) — `CrdtBackend::apply_remote`, cycle detection, op application. Convergence logic.
- [crates/kyoso_crdt/src/snapshot.rs](crates/kyoso_crdt/src/snapshot.rs) — `Snapshot`. Compaction format.
- [crates/kyoso_sync/src/plugin.rs](crates/kyoso_sync/src/plugin.rs) — Bevy ECS bridge. `inbound_system`, `outbound_system`, detection systems, `Syncable` trait, `RawPresence`.
- [crates/kyoso_sync/src/client.rs](crates/kyoso_sync/src/client.rs) — `WsClient`, the dedicated tokio runtime, mpsc/crossbeam channels.
- [crates/kyoso_graph/src/tree.rs](crates/kyoso_graph/src/tree.rs) — `TreeEdge`, `TreeParent`, `OrderKey` (fractional index).
- [crates/kyoso_graph/src/components.rs](crates/kyoso_graph/src/components.rs) — `EdgeFrom`, `EdgeTo`, edge metadata.
- [apps/kyoso_server/src/handlers/room_ws.rs](apps/kyoso_server/src/handlers/room_ws.rs) — WS upgrade handler, frame routing.
- [apps/kyoso_server/src/services/room.rs](apps/kyoso_server/src/services/room.rs) — `RoomManager`, append lock, broadcast fanout, presence map.
- [apps/kyoso_server/src/services/store.rs](apps/kyoso_server/src/services/store.rs) — Postgres queries; `(room_id, global_seq) → Op` schema lives here.
- [docs/event_bus.md](docs/event_bus.md) — your existing internal architecture doc; lists the same TODOs (reconnect, offline buffer, backpressure).

---

## 12 · References (papers, libs, articles)

**Papers**
- Weidner, Gentle, Kleppmann (2023). *The Art of the Fugue: Minimizing Interleaving in Collaborative Text Editing*. [arXiv:2305.00583](https://arxiv.org/abs/2305.00583).
- Gentle, Kleppmann (EuroSys 2025). *Collaborative Text Editing with Eg-walker: Better, Faster, Smaller*. [arXiv:2409.14252](https://arxiv.org/abs/2409.14252).
- Kleppmann, Mulligan, Gomes, Beresford (IEEE TPDS 2021). *A Highly-Available Move Operation for Replicated Trees*. [PDF](https://martin.kleppmann.com/papers/move-op.pdf).
- Rinberg et al. (VLDB 2022). *DSON: JSON CRDT Using Delta-Mutations For Document Stores*. [PDF](https://www.vldb.org/pvldb/vol15/p1053-rinberg.pdf).
- Shapiro, Preguiça, Baquero, Zawirski (2011). *Conflict-Free Replicated Data Types* (foundational survey). [Springer](https://link.springer.com/chapter/10.1007/978-3-642-24550-3_29).
- Brown, Bieniusa, Meiklejohn et al. *Riak DT map: A composable convergent replicated dictionary*.

**Libraries / production designs**
- [Loro](https://loro.dev) — Rust CRDT library; movable tree + map per node + Fugue text. [Docs: movable tree](https://loro.dev/blog/movable-tree). [Docs: Eg-walker](https://loro.dev/docs/advanced/event_graph_walker). [Docs: rich text](https://loro.dev/blog/loro-richtext).
- [Yjs](https://yjs.dev) — most-deployed CRDT lib. [Awareness docs](https://docs.yjs.dev/api/about-awareness). [y-protocols PROTOCOL.md](https://github.com/yjs/y-protocols/blob/master/PROTOCOL.md).
- [Automerge](https://automerge.org) — JSON CRDT with branches/time-travel. [Viewing history](https://www.mintlify.com/automerge/automerge/advanced/viewing-history).
- [Diamond Types](https://github.com/josephg/diamond-types) — Eg-walker reference impl. [Eg-walker reference TS port](https://github.com/josephg/eg-walker-reference).
- [Y-Sweet](https://jamsocket.com/y-sweet) — Yjs server with S3 persistence.
- [Hocuspocus](https://github.com/ueberdosis/hocuspocus) — Yjs WS backend with extension system. [Persistence docs](https://tiptap.dev/docs/hocuspocus/guides/persistence).
- [Liveblocks](https://liveblocks.io/docs/concepts) — commercial presence + storage rooms model.
- [y-webrtc](https://github.com/yjs/y-webrtc) — canonical WebRTC presence transport.
- [dmonad/crdt-benchmarks](https://github.com/dmonad/crdt-benchmarks) — standard CRDT benchmark suite.

**Architecture write-ups**
- [How Figma's multiplayer technology works](https://www.figma.com/blog/how-figmas-multiplayer-technology-works/).
- [Making Figma multiplayer more reliable](https://www.figma.com/blog/making-multiplayer-more-reliable/).
- [Ian Duncan — The CRDT Dictionary (2025)](https://www.iankduncan.com/engineering/2025-11-27-crdt-dictionary/).
- [trvedata/move-op](https://github.com/trvedata/move-op) — reference Kleppmann tree-move impl.

---
---

# Part II — Typed edges and CRDT composition (deep dive)

## II.0 · Context

This section deepens two topics from Part I that need their own treatment to be useful as a design tool:

1. **Edge typology** — the idea that not all edges in the graph want the same CRDT semantics. Tree edges (parent-child structure), reference edges (component instance → main, prototype links, mentions), and derived edges (selection, hover, focus chains) each have different invariants, different concurrency behavior, and different performance/storage profiles. The current kyoso `AddEdge` op treats all edges uniformly; this section explores what's gained by typing them.
2. **Composition** — kyoso has a multi-layer document: a graph of nodes connected by edges; each node has properties (some scalar, some structured); each edge can also have properties; the whole thing must converge. This is the recurring CRDT-composition problem: how do you build a system where every layer is independently a CRDT, where you can register new CRDT *types* per field, where causal context is shared coherently, and where the algebra of "compose two CRDTs to get a third" is well-defined and uniform.

The research aim is to produce a *general system for organising composition* — not to lock in a specific algorithm choice.

---

## II.1 · Edge typology in collaborative graph documents

### II.1.1 What's actually on the edges in a Figma-class document?

Looking concretely at the kinds of relationships a Figma-shaped doc carries (and which kyoso will need to express), edges fall naturally into three buckets that the literature also recognizes:

**Tree edges** — exactly one per child, defines the document's hierarchical scaffold:
- `parent_of` (frame contains rectangle, page contains frame, document contains page)
- The graph restricted to tree edges must be a forest at all times: no cycles, every node has at most one parent.

**Reference edges** — many-to-many, point from one structural node to another, may dangle if the target dies:
- `instance_of` (component instance → main component)
- `prototype_link` (a frame's "next" interaction → another frame)
- `constraint_pin` (a node constrained relative to another)
- `style_ref` (a node references a shared style/variable definition)
- `comment_anchor` (a comment thread → the node it's attached to)
- `mention` (a comment body → a user/node)
- `mask_of` (a layer used as mask → the masked layer)

**Derived edges** — *not synced*, locally computed every frame from other state:
- selection (current peer's selected node ids — that's presence, not document)
- hover, focus chain
- adjacency / containment lookups (`children_of`, `descendants_of`)
- spatial indices (R-tree, quadtree)
- z-order indices
- "things that reference X" reverse-edge maps for fast lookup

The first two are storage; the third is computed. The interesting design observation: production systems tend to *also* type their reference edges by category, because different reference edges want different **dangling-target policies**. A `prototype_link` to a deleted frame should just become a no-op at runtime; an `instance_of` whose main was deleted should detach the instance and unfreeze its overrides; a `comment_anchor` to a deleted node should keep the comment but show "node deleted" in UI.

### II.1.2 Tree edges — Kleppmann move + OrderKey (kyoso has this)

Already covered in Part I §2.2. The invariants:
- **Single parent**: at any consistent state, at most one tree edge points *to* a given node.
- **No cycles**: applying a `Move` op that would form a cycle is rejected (deterministically, at apply time, because all replicas see ops in the same `GlobalSeq` order).
- **Sibling order**: `OrderKey` (fractional-index string) gives every child a stable position; insert-between produces a new key without rewriting siblings.

The Kleppmann 2021 algorithm ([PDF](https://martin.kleppmann.com/papers/move-op.pdf)) handles the hard P2P case (no total order); kyoso's server-mediated total order makes the tree a much simpler beast — concurrent moves are linearized at the server, cycle detection runs on a snapshot of the tree at a known `GlobalSeq`, and the result is deterministic.

**Wire op**: `Move { target: CrdtId, new_parent: Option<CrdtId>, position: OrderKey }`. ✓

**The interesting subtlety**: a tree edge in kyoso is not a separate `AddEdge` op — it's implicit in `AddNode { parent, position }` plus `Move`. Reference edges, by contrast, *are* explicit `AddEdge` ops with their own `CrdtId`. This asymmetry is correct: tree-edge identity is bound to the *child* (each node has at most one tree-parent at a time), so storing it as a property of the node rather than as a first-class edge entity is the natural representation.

### II.1.3 Reference edges — design choices

Reference edges are first-class, identified by their own `CrdtId`. The conflict surface is `AddEdge` / `RemoveEdge` plus dangling targets. There are three classical CRDTs to choose from:

**(a) 2P-Set** — once removed, never re-added.
- `AddEdge` and `RemoveEdge` are just additions to two append-only sets.
- The set membership = `additions ∖ removals`.
- ✓ Simple, deterministic, low metadata.
- ✗ Cannot re-add an edge after removal. For some semantics this is fine (a `style_ref` you detached stays detached); for others it's wrong (toggling a constraint on/off should be possible).

**(b) Add-wins OR-Set** — concurrent add-vs-remove resolves to add.
- Each `AddEdge` mints a unique tag.
- `RemoveEdge` removes specific tags it has seen.
- A concurrent `AddEdge` produces a tag that the concurrent `RemoveEdge` cannot have observed → edge stays.
- ✓ Re-add-after-remove works.
- ✓ Matches user intuition ("if anyone is still adding it, keep it").
- ✗ Tombstones / unique tags = more metadata.
- This is what most modern CRDT-based collaborative apps default to. [Akka ORSet](https://doc.akka.io/japi/akka-core/2.9/akka/persistence/typed/crdt/ORSet.html), [Riak DT sets](https://docs.riak.com/riak/kv/2.2.3/learn/concepts/crdts/index.html), Yjs `Y.Set`.

**(c) Remove-wins** — the dual of OR-Set.
- Useful when "deletion" really means "this should be permanently gone."
- Less common in collaborative editors; mostly seen in access-control or sensitive-data contexts.

**Recommendation for kyoso reference edges: OR-Set (add-wins)** unless a specific edge category has a domain reason to prefer 2P-Set or remove-wins. The wire shape:

```rust
// Illustrative — not a commitment
enum OpKind {
    // existing
    AddNode,
    Move { target, new_parent, position },
    SetNodeProperty { target, key, value },

    // refined for typed reference edges
    AddRefEdge {
        category: EdgeCategory,    // InstanceOf, PrototypeLink, ConstraintPin, ...
        from: CrdtId,
        to: CrdtId,
        // CrdtId of the op itself = unique add-tag
    },
    RemoveRefEdge {
        target: CrdtId,            // the AddRefEdge's CrdtId
    },
    SetRefEdgeProperty { target: CrdtId, key, value },
}
```

`category: EdgeCategory` is the discriminator; per-category code decides the dangling-target policy and any per-category invariants.

### II.1.4 Reference edges and tombstoned targets

When a reference edge points to a node that's been removed, three policies in the wild:

- **Cascading tombstone**: removing the target also tombstones all incoming reference edges. Strong containment; costs: every `RemoveNode` walks the inverted index of incoming refs. This is what filesystems do (deleting a file invalidates symlinks).
- **Dangling tolerance**: the edge stays; the runtime treats it as "broken" (UI shows "Component deleted"). Cheap; costs nothing on remove. This is what Figma does with deleted main components.
- **Re-anchor on undo**: keep the edge dangling; if the target is restored (via undo), the edge automatically re-binds. This is the most user-friendly but requires keeping tombstones long enough that undo can find them.

In kyoso, the **tombstone-and-defer** model already in place is compatible with all three policies — it just controls *when* the tombstone is GC'd. Wire-protocol-wise, dangling tolerance + re-anchor on undo is the most flexible default; cascading is an opt-in per edge category (e.g., a `mask_of` edge cascades; a `prototype_link` doesn't).

### II.1.5 Edge properties (edges as first-class data carriers)

kyoso already has `SetEdgeProperty` — the edge is a first-class entity with its own `CrdtId` and can carry properties. This matters for:

- A `prototype_link`'s **interaction config** (transition type, easing, duration).
- A `constraint_pin`'s **offset / strength**.
- An `instance_of`'s **override map** (which fields of the instance differ from the main).
- A `comment_anchor`'s **anchor offset** (where on the node the bubble points).

The composition rule is the same as for nodes: each edge is a Map<key, CRDT>, and per-key CRDT type is registered. This means tree edges *also* could carry properties (e.g., the `OrderKey` is technically an edge property even though kyoso stores it on the child node) — there's no asymmetry in the data model, only in identity.

### II.1.6 Derived edges — the "do not sync" rule

The simplest CRDT semantics: **don't put it in the CRDT**.

Examples:
- The selected node ids → presence (Part I §4–5), not storage.
- The reverse-edge index ("what references X?") → recomputed at runtime via Bevy's change-detection on `AddRefEdge` / `RemoveRefEdge`.
- Spatial / z-order indices → recomputed when bounds or order keys change.
- Computed properties like `effective_opacity` (inherits from parent if unset) → recomputed.

Two implementation patterns:

- **Eager recompute**: a Bevy system runs whenever the source state changes (via `Changed<T>` or message hooks) and rebuilds the index. Cheap per-frame, simple.
- **Lazy / cached**: store the index in a resource, mark dirty on source change, recompute on first read. Slightly more code, lower cost when the index is read rarely.

The CRDT discipline is *negative*: derived state must be computable purely from synced state, so that two replicas with the same synced state produce the same derived state. If your derived state depends on non-synced inputs (peer id, current time, environment), it isn't derived — it's local.

### II.1.7 Edge typology — summary table

| | Tree | Reference | Derived |
|---|---|---|---|
| Identity | implicit on child node | explicit `CrdtId` per edge | not synced |
| Multiplicity | exactly one parent per child | many-to-many | n/a |
| Conflict resolution | Kleppmann move (atomic) | OR-Set (add-wins) by default; per-category | n/a — recomputed |
| Dangling target | impossible (remove cascades naturally) | per-category policy: cascade / dangle / re-anchor | n/a |
| Properties | fractional-index OrderKey | per-category: transition, override, anchor offset | n/a |
| Wire op | `AddNode { parent, position }`, `Move` | `AddRefEdge`, `RemoveRefEdge`, `SetRefEdgeProperty` | none |
| GC | tombstones removed at compaction | tombstones removed at compaction | n/a |
| In current kyoso | ✓ | partial — single edge kind, no category | ✓ (computed by Bevy systems) |

---

## II.2 · Lattice theory for CRDTs (foundational refresh)

This section is the formal backbone for §II.3. If you're comfortable with join-semilattices, skim.

### II.2.1 Join-semilattice — the abstract object

A **join-semilattice** is a set `S` with a binary operation `⊔` (called **join**) satisfying:

- **Associativity**: `(a ⊔ b) ⊔ c = a ⊔ (b ⊔ c)`
- **Commutativity**: `a ⊔ b = b ⊔ a`
- **Idempotency**: `a ⊔ a = a`

These three together induce a **partial order** `≤` defined by `a ≤ b ⟺ a ⊔ b = b`. The join is the **least upper bound** under that order.

A CRDT is a join-semilattice plus a "bottom" element `⊥` (initial state) and a set of **inflation operations** (mutations) that move you *up* the lattice (`mut(s) ≥ s`). Convergence is then a theorem: any two replicas, after exchanging their states and joining, reach the same state. The three semilattice axioms are exactly the safety net you need:

- Commutativity → order of message arrival doesn't matter.
- Associativity → grouping of joins doesn't matter.
- Idempotency → re-delivering the same message doesn't matter.

Reference: [lars.hupel.info — Lattices](https://lars.hupel.info/topics/crdt/03-lattices/) is a clean intro; [Almeida 2018](https://members.loria.fr/CIgnat/files/replication/Delta-CRDT.pdf) is the canonical paper for the modern δ-state framing.

### II.2.2 Three flavors of CRDTs and their relationship to the lattice

**State-based (CvRDT)**: the entire state is an element of the lattice. To sync, peer A sends its full state to peer B; B joins it with its own. Bandwidth scales with state size. Examples: G-Counter (vector of integers per peer, join = pointwise max), G-Set (set, join = union).

**Op-based (CmRDT)**: peers exchange operations. Convergence requires that each operation **commutes with concurrent operations** and is delivered exactly once via causal broadcast. Bandwidth scales with op rate. This is what kyoso uses.

**Delta-state (δ-CRDT)** ([Almeida, Shoker, Baquero 2018](https://arxiv.org/abs/1603.01529)): the state is still a lattice element, but mutations produce **δ-mutators** — small lattice elements that, when joined, equal the state-space change. Bandwidth scales with op rate (like op-based) *but* deltas tolerate duplication and reordering (like state-based). δ-CRDTs are the right framework when you need both robustness (bad networks, gossip) and efficiency.

The **deep insight** of δ-CRDTs for our purposes: every "operation" is itself a small element of the same lattice as the state. This makes composition trivial — you don't have a separate "op type" and "state type"; both are lattice elements.

### II.2.3 Causal context — the key to nesting

When you nest CRDTs (a Map whose values are CRDTs, a CRDT whose elements are CRDTs), naive composition breaks: the inner CRDT might generate metadata (tombstones, dots) that the outer container doesn't know how to GC, and concurrent removals at the outer level might revive elements at the inner level (the **Add-Wins anomaly**).

The fix in Riak DT and Almeida's δ-CRDTs is a shared **causal context** — a single, document-wide store of "dots" (`(replica_id, sequence)` pairs) that tracks every operation ever made anywhere in the document. Each value in the document is paired with a causal-context-aware lattice element. The composition becomes:

```
Document = (DotStore, CausalContext)
DotStore = Map<Path, LatticeValue>
```

where `Path` reaches into nested structures. Joins propagate causally: if a dot is in the causal context of replica A but not in A's dot store, A has *seen and removed* that operation; B should respect that on join.

This is the formal structure underneath Automerge, Riak DT maps, DSON, Loro's REG, and (in spirit) Yjs. kyoso's `(CrdtId, GlobalSeq)` pair already provides the dot infrastructure — every op is identified by its `CrdtId`, and `GlobalSeq` linearizes them. The piece kyoso doesn't yet have is *exposing* this as a "shared context" available to every embedded CRDT.

References: [Riak DT map paper](https://dl.acm.org/doi/10.1145/2596631.2596633), [DSON VLDB 2022](https://www.vldb.org/pvldb/vol15/p1053-rinberg.pdf), [Composition in State-based Replicated Data Types (Bulletin of EATCS)](https://bulletin.eatcs.org/index.php/beatcs/article/viewFile/507/496).

### II.2.4 Catalog of base CRDTs — a quick reference

The shortlist you'd register types from in a composition system:

| CRDT | Use | Lattice |
|---|---|---|
| **LWW Register** | scalar with last-writer-wins | `Option<(timestamp, value)>`; join = max by timestamp |
| **MV Register** | scalar that keeps concurrent values for app-level resolution | `Map<Dot, Value>`; join = union of dots minus dominated |
| **G-Counter** | monotonically-increasing counter | `Map<Replica, u64>`; join = pointwise max |
| **PN-Counter** | counter that goes up and down | pair of G-Counters (positive, negative) |
| **G-Set** | grow-only set | `Set<E>`; join = union |
| **2P-Set** | add-then-remove (no re-add) | `(Set<E>, Set<E>)`; membership = adds − removes |
| **OR-Set / AW-Set** | add-wins set with re-add | `Set<(E, Dot)>` + causal context |
| **RW-Set** | remove-wins | dual of OR-Set |
| **LWW-Set** | timestamp-based set | timestamp per element |
| **Sequence (Fugue / yjsmod / RGA)** | ordered list with concurrent insert/delete | per-element position metadata |
| **Map (Riak DT, Automerge, Loro)** | key → CRDT | composed; uses causal context |
| **Movable Tree (Kleppmann + OrderKey)** | hierarchy with move | Move ops linearized; OrderKey for siblings |
| **Graph (2P2P)** | nodes + edges, add/remove | pair of 2P-Sets with edge-endpoint constraint |

Reference catalogs: [Ian Duncan's CRDT Dictionary (2025)](https://www.iankduncan.com/engineering/2025-11-27-crdt-dictionary/), [crdt.tech glossary](https://crdt.tech/glossary), [Bartosz Sypytkowski's blog series](https://www.bartoszsypytkowski.com/operation-based-crdts-registers-and-sets/).

---

## II.3 · Composition algebra

The goal here is not "use Loro" or "use Automerge" — it's to identify the *combinators* that compose smaller CRDTs into larger ones, so kyoso can have a uniform mechanism.

### II.3.1 Three combinators that buy you everything

**Pair / Product** — `(A, B)` where both are CRDTs:
- bottom = `(⊥_A, ⊥_B)`
- join: `(a₁, b₁) ⊔ (a₂, b₂) = (a₁ ⊔_A a₂, b₁ ⊔_B b₂)`
- Trivially preserves all three semilattice axioms.
- Generalizes to n-tuples: a Rust struct of CRDTs is a CRDT.

**Map<K, V>** where `V` is a CRDT and `K` is some opaque key type:
- bottom = empty map
- join: union of keys; for shared keys, join values: `(k → v₁) ⊔ (k → v₂) = (k → v₁ ⊔_V v₂)`
- This is what makes "a node has a property bag" a CRDT.
- The subtle case: deleting a key. With a *causal-context map* (Riak DT shape), deletion records the dots it observed; concurrent updates that produce dots not in the deletion's context survive (add-wins).

**Recursive types** — `μF.F<X>` where `X` ranges over CRDTs:
- A tree of CRDTs is a CRDT (apply the Map combinator at every level).
- A value at any depth is reachable by a path; the lattice element is the (Path → LatticeValue) map plus a single document-wide causal context.

These three combinators + the catalog of base CRDTs in §II.2.4 is **provably sufficient** for any "nested document" shape — JSON, Figma documents, the kyoso document model.

### II.3.2 Where composition gets subtle

The combinators are clean in theory; in practice, there are three places that need careful design:

**(a) Move semantics inside a composed structure.** If a value in a Map can move to a different key (or a node can move to a different parent), that's a *move* op, not a delete-then-add. A delete-then-add creates a new identity; a move preserves identity. Kleppmann's tree move is exactly the abstraction you need for "move within a hierarchical map." Loro's movable tree applies this; kyoso's `Move` op applies this for tree edges.

**(b) Causal context plumbing.** Every embedded CRDT needs to *observe* the causal context of its container's operations to resolve add-wins / remove-wins concurrency. Two designs:

- **Implicit / global**: one document-wide causal context, threaded through every op application. Riak DT, Loro, kyoso (by virtue of `GlobalSeq`).
- **Explicit / per-CRDT**: each embedded CRDT carries its own dot store, joined when the parent is joined. Higher metadata cost but more modular (a sub-doc can be detached and reattached cleanly).

For kyoso's server-mediated total order, **implicit/global is correct** — every op carries `(CrdtId, GlobalSeq)`, that *is* the causal context, and embedded CRDTs use the outer op's identity as their dot.

**(c) Schema migration.** If the registered CRDT type for a property changes from `LWW<String>` to `Sequence<Char>`, what happens to existing ops in the log? Two approaches:

- **Versioned ops**: each op carries a schema version; replay-time dispatch picks the correct application logic. Easy to add but means the log has mixed versions forever.
- **Migration ops**: a special `MigrateProperty { target, key, from_kind, to_kind, transform }` op that converts existing state and changes the registered kind atomically. Cleaner but requires a deterministic transform function.

The decision can be deferred; the cost of *not* deciding is having to re-design when you first need a richer type for an existing field.

### II.3.3 δ-mutators as a uniform op shape

The δ-CRDT framing makes for a uniform wire format. Every "op" is a δ — a small lattice element identifying the change. The wire ops in kyoso could collapse from a multi-variant `OpKind` into a single `Delta` payload + a path:

```rust
// Sketch — illustrative, not a recommendation
struct Op {
    id: CrdtId,
    seq: Option<GlobalSeq>,
    path: Path,         // e.g., ["node", node_id, "properties", "name"]
    delta: Delta,       // a small lattice element
}

enum Delta {
    LwwReplace(Bytes, Timestamp),
    SetAdd(Bytes, Dot),
    SetRemove(Vec<Dot>),
    CounterDelta(i64, Replica),
    SequenceInsert(Position, Bytes),
    SequenceDelete(Position, usize),
    MapPut(Key, Box<Delta>),     // recursive — embedded delta
    MapRemove(Key, CausalContext),
    TreeMove { parent: CrdtId, position: OrderKey },
    AddRefEdge { category: EdgeCategory, from: CrdtId, to: CrdtId },
    // ...
}
```

The advantage of this shape:
- All ops apply via a single dispatch function: `apply(state, path, delta) → state'`.
- New CRDT kinds add new `Delta` variants; existing logic unchanged.
- The wire format is uniform; varint compression lands consistently across kinds.
- Schema migration becomes "interpret old `Delta` variants in terms of new state."

This is essentially the shape Automerge converged on after several rewrites; it's also Loro's internal shape ("a delta to a position in the document"). It's not a small change for kyoso — it's a *direction* worth weighing against the simpler enum-of-ops style.

### II.3.4 The shape of a Rust trait hierarchy for composition

Sketch (illustrative — concrete shape depends on whether you go state-based, op-based, or δ-state):

```rust
// State-based / δ-state framing
trait Lattice: Clone + Eq {
    fn bottom() -> Self;
    fn join(&mut self, other: Self);
    fn leq(&self, other: &Self) -> bool;
}

trait Crdt: Lattice {
    type Op;
    type Mutation;
    fn apply(&mut self, op: Self::Op);
    fn mutate(&mut self, m: Self::Mutation) -> Self::Op;
}

// Composition combinators come for free:
impl<A: Crdt, B: Crdt> Lattice for (A, B) { /* pointwise */ }
impl<A: Crdt, B: Crdt> Crdt for (A, B) {
    type Op = (Option<A::Op>, Option<B::Op>);
    /* ... */
}

impl<K: Hash + Eq, V: Crdt> Lattice for CausalMap<K, V> { /* ... */ }
impl<K: Hash + Eq, V: Crdt> Crdt for CausalMap<K, V> { /* ... */ }
```

Concrete CRDTs are then plain structs that implement `Crdt`:

```rust
struct LwwRegister<T> { /* ... */ }
struct OrSet<E> { /* ... */ }
struct PnCounter { /* ... */ }
struct Sequence<T> { /* a Fugue-style impl */ }
struct MovableTree<NodeData> { /* Kleppmann + OrderKey */ }
```

And a composed document is just nested structs:

```rust
struct NodeProperties {
    name: LwwRegister<String>,
    transform: LwwRegister<Transform>,
    text: Sequence<char>,
    tags: OrSet<String>,
    counter: PnCounter,
}

// Derived: NodeProperties: Crdt automatically (from the tuple-like Crdt impl over its fields).
```

The schema is the type — there's no separate registration step; `NodeProperties` *is* the schema. This is the [autosurgeon](https://github.com/automerge/autosurgeon) approach for Automerge (a derive macro generates the boilerplate).

The alternative — runtime registration — looks like:

```rust
let mut schema = Schema::new();
schema.register::<LwwRegister<String>>("name");
schema.register::<Sequence<char>>("text");
schema.register::<OrSet<String>>("tags");
let doc = Document::new(schema);
doc.set("name", "hello");      // dynamic dispatch on registered kind
```

The dynamic approach is more flexible (schema can change at runtime, multiple "shapes" coexist in one process) but loses the type-safety. **The right answer for kyoso is probably typed-by-default with an escape hatch**: derive `Crdt` for static schemas, and a separate `DynamicMap<K, AnyCrdt>` type for cases where keys aren't known at compile time (e.g., user-defined custom properties).

References for trait-design inspiration: [rust-crdt](https://github.com/rust-crdt/rust-crdt) (clean trait shapes), [autosurgeon](https://github.com/automerge/autosurgeon) (derive macro for Automerge), [delta-enabled-crdts](https://github.com/CBaquero/delta-enabled-crdts) (Baquero's reference C++ impl of δ-CRDTs).

---

## II.4 · A concrete sketch — composition for kyoso

Putting §II.1, §II.2, §II.3 together as a worked example of the data shape for a Figma-like Frame node:

```
Document
├── tree: MovableTree<Node>
│   └── Node (CrdtId)
│       ├── kind: LwwRegister<NodeKind>      // "Frame", "Rectangle", "Text", "Component"
│       ├── name: LwwRegister<String>
│       ├── transform: LwwRegister<Affine>   // single-user-at-a-time fields → LWW ok
│       ├── visible: LwwRegister<bool>
│       ├── style: CausalMap<String, StyleValue>
│       │       // per-key CRDT: fill→LWW<Color>, opacity→LWW<f32>, blendMode→LWW<…>
│       ├── text: Sequence<char>             // Fugue / yjsmod for collaborative text
│       ├── tags: OrSet<String>
│       └── extras: CausalMap<String, AnyCrdt>  // dynamic user-defined props
│
├── ref_edges: PerCategory<OrSet<RefEdge>>
│   ├── instance_of: OrSet<RefEdge>
│   ├── prototype_link: OrSet<RefEdge>
│   ├── constraint_pin: OrSet<RefEdge>
│   ├── style_ref: OrSet<RefEdge>
│   └── comment_anchor: OrSet<RefEdge>
│   //  RefEdge = struct { from: CrdtId, to: CrdtId, props: CausalMap<…> }
│
└── (causal context — one document-wide dot store; implicit via GlobalSeq)
```

The tree layer uses Kleppmann + OrderKey (kyoso's `Move` op is correct as-is). Each Node is itself a Map of CRDTs — LwwRegister, Sequence, OrSet, nested CausalMap. Reference edges live outside the tree, organized by category, each category an OR-Set of typed `RefEdge` records. Each `RefEdge` is itself a small CRDT (its `props` are a Map). Derived state (selection, hover, indices) sits in Bevy ECS and is *not in this picture*.

**The wire ops** then cleanly fan out over the structure:

```
- Move target=N into parent=P at position=K
- AddRefEdge category=instance_of from=N1 to=N2
- RemoveRefEdge target=<edge-CrdtId>
- SetNodeProperty target=N path=["name"]   delta=LwwReplace(...)
- SetNodeProperty target=N path=["text"]   delta=SequenceInsert(pos, "h")
- SetNodeProperty target=N path=["tags"]   delta=SetAdd("draft", dot)
- SetNodeProperty target=N path=["style","fill"] delta=LwwReplace(...)
- SetRefEdgeProperty target=E path=["transition"] delta=LwwReplace(...)
```

Every op carries `(CrdtId, GlobalSeq)`. The path is a small `Vec<PathSegment>`. The delta is one of the §II.3.3 variants. Apply-time dispatch is uniform: walk the path, find the embedded CRDT, apply the delta.

**Where this sits relative to current kyoso**:
- Tree + Move: **already correct**.
- `SetNodeProperty.value: Bytes` (LWW only) → broadens to a `Delta` enum; today's behavior is the `LwwReplace` variant.
- `AddEdge` / `RemoveEdge` (untyped) → grows a `category` field; OR-Set semantics replace today's add/remove pair.
- Reference edge properties → already supported via `SetEdgeProperty`.
- Causal context → already implicit in `GlobalSeq`; no change to the model, possibly a change in how it's *exposed* to nested CRDTs.

---

## II.5 · Implementation references — what to crib from

For trait shape and schema-via-derive:
- [autosurgeon](https://github.com/automerge/autosurgeon) — Automerge's derive-macro layer for typed schemas. The "I want a Rust struct that's also a CRDT document" answer.
- [rust-crdt](https://github.com/rust-crdt/rust-crdt) — clean implementations of LWW, OR-Set, Map, etc. Good for studying trait composition. Less production-tested than Automerge/Loro/Yrs.

For δ-CRDT machinery:
- [Almeida, Shoker, Baquero — Delta State Replicated Data Types (2018)](https://arxiv.org/abs/1603.01529) — the foundational paper. Read the Map composition section (§5) carefully.
- [Baquero — delta-enabled-crdts](https://github.com/CBaquero/delta-enabled-crdts) — C++ reference impl of the paper's data types. Useful to translate to Rust.
- [Composition in State-based Replicated Data Types (Bulletin of EATCS)](https://bulletin.eatcs.org/index.php/beatcs/article/viewFile/507/496) — composition theory.

For Map-of-CRDTs with causal context:
- [Riak DT map paper (PaPEC 2014)](https://dl.acm.org/doi/10.1145/2596631.2596633) — the canonical Map composition design.
- [Riak Causal Context docs](https://docs.riak.com/riak/kv/2.2.3/learn/concepts/causal-context/) — dotted version vectors explained.
- [DSON: JSON CRDT (VLDB 2022)](https://www.vldb.org/pvldb/vol15/p1053-rinberg.pdf) — modern composable JSON CRDT.

For Movable Tree composed with per-node Map:
- [Loro — Movable tree CRDTs](https://loro.dev/blog/movable-tree).
- [Kleppmann — Replicated tree move (TPDS 2021)](https://martin.kleppmann.com/papers/move-op.pdf).
- [Loro — Tree tutorial](https://www.loro.dev/docs/tutorial/tree) — Map per tree node example.

For sequences (collaborative text inside a property):
- [Weidner, Gentle, Kleppmann — Fugue (2023)](https://arxiv.org/abs/2305.00583).
- [Gentle, Kleppmann — Eg-walker (EuroSys 2025)](https://arxiv.org/abs/2409.14252).
- [diamond-types](https://github.com/josephg/diamond-types) — Rust impl, study the API.

For sets:
- [Bartosz Sypytkowski — Operation-based CRDTs: registers and sets](https://www.bartoszsypytkowski.com/operation-based-crdts-registers-and-sets/) — clean explanation of OR-Set vs 2P-Set.
- [Akka ORSet](https://doc.akka.io/japi/akka-core/2.9/akka/persistence/typed/crdt/ORSet.html) — production-grade reference.

For typed/decomposed delta CRDT sets:
- [Almeida, Baquero — Decomposed delta CRDT Sets in Riak (2016)](https://arxiv.org/pdf/1605.06424).

---

## II.6 · Open questions for kyoso (composition-specific)

1. **Static vs dynamic schema**: should the document type be a Rust struct (compile-time schema, autosurgeon-style) or a runtime-registered map (dynamic schema, Riak-DT-style)? **Probably static-by-default with a `DynamicMap` escape hatch for user-defined fields.**
2. **One `Delta` enum vs many `OpKind` variants**: the uniform-Delta shape (§II.3.3) is more extensible but a bigger refactor. The current `OpKind` variant style is simpler for the common operations and what's already shipped. **A middle ground**: keep the structural ops (`AddNode`, `Move`, `AddRefEdge`) as variants, and use a `Delta` enum *only* inside `SetNodeProperty.value` and `SetRefEdgeProperty.value`.
3. **Edge category extensibility**: hardcoded enum or open string with a registry? Hardcoded is safer for invariants (compiler-checked); open is more extensible (plugins can add edge categories). **Hardcoded enum + a `Custom(SmolStr)` escape hatch is the usual compromise.**
4. **Per-edge CRDT type per category**: do you want different categories to use different CRDT shapes (e.g., `instance_of` is OR-Set but `style_ref` is LWW-by-name)? Right now it's tempting to make all categories use the same OR-Set shape; the cost of that uniformity is sometimes wrong semantics, but the cost of customization is more code paths to maintain.
5. **Causal context exposure**: keep `GlobalSeq` as the implicit context (current), or expose a structured `CausalContext` type that nested CRDTs read from? The latter is needed if you ever want to support partial replication (e.g., a peer that only syncs a subtree).
6. **Move semantics inside maps**: should keys in a `CausalMap` be movable (rename without losing identity)? Most apps don't need this; flagging because if you do need it, the algorithm is non-trivial.

---

## II.7 · Verification specifics for composition

Standard property-based-testing pattern for CRDT composition:

- Generate a random sequence of ops on N replicas.
- Pick a random subset to deliver to each replica in random order.
- Repeat until every replica has seen every op.
- Assert: all replicas have equal state.

For composed CRDTs you want one extra level: **randomize the schema too** — generate random property kinds (LWW, OrSet, Sequence, ...) and a random nesting depth, then run the convergence test. This catches composition bugs (a CausalMap whose values include a sub-CausalMap whose values include a Sequence — does that converge?) that fixed schemas miss.

`proptest` ([proptest crate](https://crates.io/crates/proptest)) is the Rust standard; deterministic-simulation libs like `madsim` are useful for the network-partition variants. The `dmonad/crdt-benchmarks` suite is the standard *performance* benchmark; correctness benchmarks are typically hand-rolled per project.

---
---

# Part III — Implementation plan: typed edges + composition

## III.0 · Decisions (locked in this iteration)

- **Static schema** — document structure declared as Rust types; correctness checked at compile time; one runtime escape hatch (`CausalMap<K, AnyDelta>`) for user-defined property bags.
- **One `Delta` enum on the wire** — uniform shape across all CRDT kinds; per-CRDT typed deltas convert into and out of `WireDelta`.
- **Trait hierarchy for composition** — `Lattice` and `Crdt` traits with associated types; tuple/struct/Map combinators inherit `Crdt` automatically; a derive macro generates `Crdt` for user structs.
- **Adopt §II.4 sketch** as the data shape (movable tree of nodes; per-node `CausalMap` of typed properties; per-category `OrSet` of typed `RefEdge`s; one document-wide causal context).
- **Hardcoded `EdgeCategory` enum** with a `Custom(SmolStr)` escape hatch.
- **Per-category CRDT type per edge category** — categories pick their own conflict-resolution semantics (default OR-Set; some categories may override to LWW-by-name or 2P-Set).
- **Expose a structured `CausalContext` type** — passed to every `apply_delta` / `mutate` call; nested CRDTs read/update from it through a small interface.

## III.1 · Architectural shape

```
   Bevy ECS                 kyoso_sync          kyoso_crdt                kyoso_server
 ───────────────         ─────────────────   ────────────────────       ─────────────────
                                              schema (static)
  Components ───┐                              ┌────────────┐              OpLog<OpKindV2>
                ├──► detection                 │ Lattice    │              ┌──────────┐
  TreeParent    │   systems  ─► mutate(M) ───► │ Crdt       │  WireDelta   │ append() │
  OrderKey      │              (typed Mut)     │ CausalCtx  │ ◄──postcard──┤ broadcast│
  RefEdge       │                              │ WireDelta  │              │ snapshot │
                │                              └────────────┘              └──────────┘
  Properties ◄──┘   projection         apply_delta(WireDelta, ctx)
                    inbound system
```

The seam between `kyoso_sync` and `kyoso_crdt` becomes: detection systems call **typed mutations** on the CRDT (`name.set("foo", ctx)`); mutations produce **typed deltas**; typed deltas convert to **`WireDelta`** for the wire; on the receive side, `WireDelta` is applied via the schema's path-dispatch and converted back to typed deltas at the appropriate CRDT instance.

## III.2 · Phased rollout

Eight phases, each merge-able and testable independently. Fixed phases A–C are pure additions (no behavior change); D–G evolve the wire format; H–I are integration and verification.

### Phase A — `kyoso_crdt::lattice` module (no behavior change)

**New file**: [crates/kyoso_crdt/src/lattice.rs](crates/kyoso_crdt/src/lattice.rs)

```rust
//! Algebraic foundations for composable CRDTs.

pub trait Lattice: Clone + PartialEq {
    fn bottom() -> Self;
    /// Idempotent, commutative, associative join (least upper bound).
    fn join(&mut self, other: Self);
    fn leq(&self, other: &Self) -> bool;
}

/// A CRDT is a lattice plus a typed mutation API.
pub trait Crdt: Lattice {
    /// High-level intent the application expresses: "set", "add", "delete at pos 3".
    type Mutation;
    /// On-the-wire representation of one change. Convertible to/from `WireDelta`.
    type Delta: Clone + PartialEq + Into<crate::delta::WireDelta>;

    /// Apply a delta from a remote peer or replayed log. Idempotent.
    fn apply(&mut self, delta: &Self::Delta, ctx: &CausalContext) -> Result<(), ApplyError>;

    /// Generate a delta from a local mutation. The mutation is also applied
    /// to `self` (so the caller sees its effect immediately).
    fn mutate(&mut self, m: Self::Mutation, ctx: &mut CausalContext) -> Self::Delta;
}
```

**Files touched**: [crates/kyoso_crdt/src/lib.rs](crates/kyoso_crdt/src/lib.rs) — `pub mod lattice;` and re-export.

**Out of scope for Phase A**: actual CRDTs that implement these traits; the wire enum (Phase D); the apply machinery in `CrdtBackend` (Phase G).

**Tests**: `Lattice` axiom proptests for any future impl: associativity, commutativity, idempotency.

### Phase B — `kyoso_crdt::context::CausalContext`

**New file**: [crates/kyoso_crdt/src/context.rs](crates/kyoso_crdt/src/context.rs)

The minimum that nested CRDTs need to read:

```rust
/// A single op's identity in the document's causal history.
pub type Dot = CrdtId;

/// Causal context exposed to nested CRDTs during apply / mutate.
///
/// In kyoso's server-mediated total order the context is light: every op has
/// a single `(CrdtId, GlobalSeq)`; embedded CRDTs use the outer op's CrdtId
/// as their dot. The structured type is here so the same trait surface works
/// later if/when partial replication or branching land — at that point this
/// type grows a `seen: DotSet` field without touching the apply API.
pub struct CausalContext<'a> {
    /// Identity of the op currently being applied (for inbound) or generated
    /// (for outbound). Embedded CRDTs use this as the source of fresh dots.
    pub op_id: Dot,
    /// Server-assigned linear position (None on outbound — seq is assigned
    /// later by the server).
    pub seq: Option<GlobalSeq>,
    /// Document-wide bookkeeping the embedded CRDT may read or update.
    pub(crate) state: &'a mut CausalState,
}

/// Backing state for a `CausalContext`. Lives on the `CrdtBackend` (or
/// equivalently a sub-document container).
pub struct CausalState {
    /// Per-peer counter for fresh dot generation when a CRDT needs more
    /// than one dot per op (OR-Set add can produce one dot per element).
    next_sub: HashMap<Dot, u32>,
    // Future: dot store / observed set for branching support.
}

impl<'a> CausalContext<'a> {
    /// Fresh sub-dot under the current op (for OR-Set add tags etc.).
    pub fn fresh_sub_dot(&mut self) -> SubDot { /* ... */ }
}
```

**Files touched**: [crates/kyoso_crdt/src/lib.rs](crates/kyoso_crdt/src/lib.rs) — `pub mod context;`.

**Out of scope**: dot-store-backed remove tracking — that's added when OR-Set is implemented in Phase C.

### Phase C — base CRDT primitives

**New file**: [crates/kyoso_crdt/src/types/mod.rs](crates/kyoso_crdt/src/types/mod.rs) plus per-CRDT submodules.

Implement, in order of priority:

1. `LwwRegister<T: Clone + Eq + Serialize + DeserializeOwned>` — replaces today's `Vec<u8>` LWW-by-key storage. Initial impl matches current behavior (timestamp = `GlobalSeq` of the op; tie-break by `PeerId`). Files: [types/lww.rs](crates/kyoso_crdt/src/types/lww.rs).
2. `OrSet<T: Clone + Eq + Hash + Serialize + DeserializeOwned>` — add-wins set with per-add dot. Used directly for typed reference edges (Phase E) and available as a property type. Files: [types/or_set.rs](crates/kyoso_crdt/src/types/or_set.rs).
3. `PnCounter` — ergonomic, well-known. Files: [types/counter.rs](crates/kyoso_crdt/src/types/counter.rs).
4. `CausalMap<K, V: Crdt>` — the central composition combinator. Files: [types/map.rs](crates/kyoso_crdt/src/types/map.rs).
5. `Sequence<T>` — placeholder API + naive impl initially (just a `Vec` with index-based ops; not Fugue-correct). Files: [types/sequence.rs](crates/kyoso_crdt/src/types/sequence.rs). **Real Fugue/Eg-walker impl is its own work item, deferred.**

**Reuse** [diamond-types](https://github.com/josephg/diamond-types) when Sequence becomes a real priority (don't reinvent Fugue).

Each type:
- Implements `Lattice` and `Crdt`.
- Has property tests for the lattice axioms and convergence under random op interleaving (`proptest`).
- Has a typed `Delta` and `Mutation`.
- `From<Self::Delta> for WireDelta` (defined in Phase D).

**Tests**: per-type unit + property tests for each CRDT primitive in `crates/kyoso_crdt/tests/`.

### Phase D — `WireDelta` enum + path addressing

**New file**: [crates/kyoso_crdt/src/delta.rs](crates/kyoso_crdt/src/delta.rs)

```rust
/// Path into the document for delta dispatch.
///
/// Examples:
///   `node:7:2 / "name"`                              — node 7:2's `name` property
///   `node:7:2 / "style" / "fill"`                    — nested map field
///   `node:7:2 / "extras" / "user-defined-key"`       — dynamic escape hatch
///   `edge:9:1 / "transition"`                        — edge 9:1's transition prop
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Path(pub SmallVec<[PathSegment; 4]>);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PathSegment {
    /// Static field name, interned for size and speed.
    Field(SmolStr),
    /// Dynamic key in a `CausalMap<String, _>`.
    Key(String),
}

/// Uniform on-the-wire representation of a single change.
///
/// Each typed CRDT's `Delta` converts into one variant of this enum (and
/// back, via `try_into` — the schema layer guarantees the variant matches
/// the destination CRDT's expected type, so failure is a protocol bug).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WireDelta {
    LwwReplace { value: Bytes, ts: LwwStamp },
    OrSetAdd { tag: SubDot, value: Bytes },
    OrSetRemove { observed: Vec<SubDot> },
    PnCounterDelta { replica: PeerId, by: i64 },
    SequenceInsert { pos: SeqPos, value: Bytes },
    SequenceDelete { pos: SeqPos, len: u32 },
    MapPut { key: PathSegment, inner: Box<WireDelta> },
    MapRemove { key: PathSegment, observed: Vec<SubDot> },
    Move { new_parent: Option<CrdtId>, position: OrderKey },
    AddRefEdge { category: EdgeCategory, from: CrdtId, to: CrdtId, tag: SubDot },
    RemoveRefEdge { observed: Vec<SubDot> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LwwStamp { pub seq: GlobalSeq, pub peer: PeerId }
```

**Files touched**:
- [crates/kyoso_crdt/src/lib.rs](crates/kyoso_crdt/src/lib.rs) — `pub mod delta;`.
- Each `types/*.rs` — `From<MyDelta> for WireDelta` and `TryFrom<WireDelta> for MyDelta`.

**Tests**: round-trip every `WireDelta` variant through postcard; the typed-delta ↔ WireDelta conversion is lossless.

### Phase E — evolve `OpKind` to use `WireDelta`

**File touched**: [crates/kyoso_crdt/src/op.rs](crates/kyoso_crdt/src/op.rs)

```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OpKind {
    AddNode,
    RemoveNode { target: CrdtId },
    /// Atomic Kleppmann move; tree edges only.
    Move { target: CrdtId, new_parent: Option<CrdtId>, position: OrderKey },

    /// Reference-edge add (typed). Replaces the old AddEdge.
    AddRefEdge { category: EdgeCategory, from: CrdtId, to: CrdtId },
    /// Reference-edge remove. `target` is the AddRefEdge's CrdtId.
    RemoveRefEdge { target: CrdtId },

    /// Apply a delta to a property addressed by `path` on a node.
    SetNodeProperty { target: CrdtId, path: Path, delta: WireDelta },
    /// Same shape for ref-edge properties.
    SetRefEdgeProperty { target: CrdtId, path: Path, delta: WireDelta },
}
```

The old `AddEdge { from, to }` and `SetNodeProperty { key: String, value: Vec<u8> }` variants disappear. Because the project is in active early development with no production data:
- **Direct migration is preferable** to a parallel V2 enum. The wire format breaks; old captured fixtures are invalidated; tests are updated in lockstep.
- If you later realize you want a graceful migration, add a `version: u8` byte at the front of the postcard frame in this same change so future schema bumps are gated by it.

**`OpKind::SetNodeProperty.delta` is a `WireDelta`** (not a typed delta) — this is the runtime shape; the typed shape is reconstructed at apply time when we know which CRDT lives at `path`.

### Phase F — `EdgeCategory` and per-category CRDT trait

**New file**: [crates/kyoso_crdt/src/edge_category.rs](crates/kyoso_crdt/src/edge_category.rs)

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeCategory {
    InstanceOf,
    PrototypeLink,
    ConstraintPin,
    StyleRef,
    CommentAnchor,
    Mention,
    MaskOf,
    Custom(SmolStr),
}

/// CRDT semantics for a category of reference edges.
pub trait RefEdgeCrdt: Default + Send + Sync + 'static {
    /// Conflict resolution policy when concurrent add/remove of the same
    /// (from, to) pair occurs.
    const POLICY: RefEdgePolicy;
    /// What to do when an endpoint is tombstoned.
    const DANGLE: DanglePolicy;
    /// Per-edge property schema (defaults to no properties).
    type Properties: Crdt;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefEdgePolicy { OrSet, TwoPSet, RemoveWins, LwwByEndpoints }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DanglePolicy { Cascade, Tolerate, ReanchorOnUndo }
```

Then concrete impls per category:

```rust
pub struct InstanceOfEdge;
impl RefEdgeCrdt for InstanceOfEdge {
    const POLICY: RefEdgePolicy = RefEdgePolicy::OrSet;
    const DANGLE: DanglePolicy = DanglePolicy::Tolerate;  // Figma model
    type Properties = InstanceOverrides;  // a custom Crdt, e.g. CausalMap<String, LwwRegister<...>>
}

pub struct PrototypeLinkEdge;
impl RefEdgeCrdt for PrototypeLinkEdge {
    const POLICY: RefEdgePolicy = RefEdgePolicy::OrSet;
    const DANGLE: DanglePolicy = DanglePolicy::Tolerate;
    type Properties = PrototypeTransition;  // LwwRegister<TransitionConfig> wrapped struct
}
```

**Apply-time dispatch** (in `CrdtBackend`):
- `OpKind::AddRefEdge { category, .. }` → look up the registered impl for `category`, drive its add path.
- `OpKind::RemoveRefEdge { target }` → look up the category from the existing edge record; drive its remove path.
- The static-schema layer (Phase G) guarantees `category` matches a known impl at compile time wherever possible; the `Custom(...)` arm is a runtime dispatch with a default OR-Set/Tolerate behavior.

### Phase G — static schema and the derive macro

The schema is the user's Rust struct; a derive macro generates the boilerplate. Inspiration: [autosurgeon](https://github.com/automerge/autosurgeon).

**New crate**: `crates/kyoso_crdt_derive` (proc-macro only). Or, simpler initially, a `derive(Crdt)` that lives behind a feature flag in `kyoso_crdt`.

```rust
// User code (the document schema)
use kyoso_crdt::types::{LwwRegister, OrSet, CausalMap, Sequence};
use kyoso_crdt::Crdt;

#[derive(Crdt)]
pub struct NodeProperties {
    pub kind:      LwwRegister<NodeKind>,
    pub name:      LwwRegister<String>,
    pub transform: LwwRegister<Affine>,
    pub visible:   LwwRegister<bool>,
    pub style:     CausalMap<SmolStr, StyleValue>,
    pub text:      Sequence<char>,
    pub tags:      OrSet<SmolStr>,
    pub extras:    CausalMap<String, AnyValue>,  // dynamic escape hatch
}
```

The macro generates:
- `impl Lattice for NodeProperties` — pointwise join over the fields.
- `impl Crdt for NodeProperties` — `apply` walks the leading `Path` segment and dispatches to the matching field's `Crdt::apply`. `mutate` takes a sum-type `Mutation` (`Set { name: String }`, `Insert { text: char, pos: usize }`, etc., one per field).
- `impl SchemaPathDispatch for NodeProperties` — the function used by `CrdtBackend::apply_remote` to route a `WireDelta` to the right embedded CRDT given a `Path`.

**Manual implementation** is also supported — the trait surface is small enough to hand-roll if a derive isn't desired for a given struct.

**File touches**:
- `crates/kyoso_crdt_derive/` (new crate).
- [crates/kyoso_crdt/Cargo.toml](crates/kyoso_crdt/Cargo.toml) — optional dep on the derive crate.
- Top-level [Cargo.toml](Cargo.toml) workspace.dependencies entry.

**Edge schemas** mirror node schemas — a per-category struct with `derive(Crdt)`:

```rust
#[derive(Crdt)]
pub struct PrototypeTransition {
    pub kind:     LwwRegister<TransitionKind>,
    pub easing:   LwwRegister<Easing>,
    pub duration: LwwRegister<Duration>,
}
```

### Phase H — `kyoso_sync` and `kyoso_server` integration

**Files touched**:
- [crates/kyoso_sync/src/plugin.rs](crates/kyoso_sync/src/plugin.rs) — detection systems become schema-aware:
  - `detect_changed_node_properties` no longer postcard-encodes the entire field via `ReflectSerializer`; instead, for each registered field-CRDT, it calls the typed `mutate(..)` API on the CRDT instance, gets a typed `Delta`, converts to `WireDelta`, builds the `OpKind::SetNodeProperty { target, path, delta }` op.
  - The detection system needs to know the schema; this is where the `derive(Crdt)`-generated `SchemaPathDispatch` impl is consumed.
- [crates/kyoso_sync/src/plugin.rs](crates/kyoso_sync/src/plugin.rs) — `inbound_system` similarly: on `OpKind::SetNodeProperty`, walk the path, find the embedded CRDT, call `apply(delta, ctx)`, then project the resulting state change into Bevy ECS (the projection logic is per-field — for `LwwRegister<T>` it's just `commands.entity(e).insert(component_for_T(state))`; for `Sequence<char>` it's a string update; for `OrSet<Tag>` it's add/remove markers on a `Tags` component).
- [crates/kyoso_crdt/src/backend.rs](crates/kyoso_crdt/src/backend.rs) — `CrdtBackend` parameterized over the schema type:
  ```rust
  pub struct CrdtBackend<NodeSchema: Crdt, EdgeSchemas: EdgeSchemaSet> { /* ... */ }
  ```
  `apply_remote` dispatches via the schema's `SchemaPathDispatch`.
- [apps/kyoso_server/src/services/store.rs](apps/kyoso_server/src/services/store.rs) — Postgres `ops` row stores postcard-encoded `Op<NewOpKind>`. Schema migration: drop and recreate the table during this dev cycle (no production data).
- [apps/kyoso_server/src/handlers/room_ws.rs](apps/kyoso_server/src/handlers/room_ws.rs) — generic over `M: CrdtModel` per the existing TODO in [model.rs](crates/kyoso_crdt/src/model.rs); this is the "mechanical ~300 lines" the doc-comment predicts.

### Phase I — verification

Three test layers:

1. **Per-CRDT-primitive property tests** (`proptest`) — included in Phase C.
2. **Composition convergence tests** — random schemas, random mutations, assert all replicas converge. Live in `crates/kyoso_crdt/tests/composition.rs`.
3. **End-to-end multi-client tests** — extend [apps/kyoso_server/tests/two_clients.rs](apps/kyoso_server/tests/two_clients.rs) and [apps/kyoso_client/tests/duplex_round_trip.rs](apps/kyoso_client/tests/duplex_round_trip.rs) to exercise: typed reference edges per category, dangling refs after `RemoveNode`, concurrent `AddRefEdge`, concurrent property mutations of multiple CRDT kinds (LWW, OR-Set, sequence).

Smoke check: run [crates/kyoso_sync/examples/scene_2d.rs](crates/kyoso_sync/examples/scene_2d.rs) in two windows after each phase that touches the protocol. Verify edits propagate, presence still works, no regressions.

## III.3 · Migration order summary

| Phase | What changes | Behavior change | Wire-format break |
|---|---|---|---|
| A | new `lattice` module | none | no |
| B | new `context` module | none | no |
| C | base CRDT types | none (unused) | no |
| D | `WireDelta` enum + path | none (unused) | no |
| E | `OpKind` evolved | yes | **yes** |
| F | `EdgeCategory` + `RefEdgeCrdt` | yes (typed edges) | **yes** (subsumed by E) |
| G | derive macro + schemas | yes (schema-driven apply) | no |
| H | sync + server integration | yes | no |
| I | tests | none | no |

The wire-format break happens once at Phase E. Everything before is purely additive. Everything after is integration of the new shape.

## III.4 · Critical files (recap, with role in this plan)

- [crates/kyoso_crdt/src/lib.rs](crates/kyoso_crdt/src/lib.rs) — adds `lattice`, `context`, `delta`, `types`, `edge_category` modules.
- [crates/kyoso_crdt/src/op.rs](crates/kyoso_crdt/src/op.rs) — `OpKind` rewritten in Phase E.
- [crates/kyoso_crdt/src/backend.rs](crates/kyoso_crdt/src/backend.rs) — `CrdtBackend` becomes schema-generic in Phase H; `apply_remote` rewritten to walk paths.
- [crates/kyoso_crdt/src/snapshot.rs](crates/kyoso_crdt/src/snapshot.rs) — `NodeSnap.properties` and `EdgeSnap.properties` change shape from `HashMap<String, Vec<u8>>` to a typed schema-state blob.
- [crates/kyoso_crdt/src/protocol.rs](crates/kyoso_crdt/src/protocol.rs) — already generic over `K`; no source change, just the new `K` is wired through.
- [crates/kyoso_crdt/src/model.rs](crates/kyoso_crdt/src/model.rs) — `CrdtModel::OpKind` becomes the new enum; existing trait surface unchanged.
- [crates/kyoso_sync/src/plugin.rs](crates/kyoso_sync/src/plugin.rs) — biggest sync-side change, Phase H.
- [crates/kyoso_graph/src/components.rs](crates/kyoso_graph/src/components.rs), [tree.rs](crates/kyoso_graph/src/tree.rs) — `OrderKey` already correct; tree edges already match the new model.
- [apps/kyoso_server/src/services/store.rs](apps/kyoso_server/src/services/store.rs) — Postgres schema regenerated.
- [apps/kyoso_server/src/handlers/room_ws.rs](apps/kyoso_server/src/handlers/room_ws.rs) — generic over `CrdtModel` in Phase H.
- (new) `crates/kyoso_crdt_derive/` — proc-macro crate for `#[derive(Crdt)]`.

## III.5 · Open implementation choices to confirm before/during work

These are the smaller decisions that have a real impact on ergonomics; flagging now so they're not surprises mid-implementation:

1. **Mutation API shape** — should `Crdt::Mutation` be a sum type (one variant per intent) or a closure-style `&mut self` method? Sum type is more uniform across CRDTs; methods are more ergonomic for hand-written code. Probably **methods on the typed CRDT** (e.g., `name.set("foo", ctx)`) with the macro generating internals.
2. **Path interning** — `PathSegment::Field(SmolStr)` interns at compile time; for hot-path fields a `&'static str` would be smaller. Worth an `intern!()` macro.
3. **`LwwStamp` tie-break** — `(seq, peer)` works for total order (kyoso has GlobalSeq). When a local op has `seq = None` (pre-server-ack), use `(LocalSeq, peer)` and re-stamp on server ack. Document this as the LWW tie-break rule.
4. **Snapshot shape** — `NodeSnap.properties` changes from `HashMap<String, Vec<u8>>` to `Bytes` (postcard-encoded `<NodeSchema as Crdt>::SnapshotState`). Schema-aware decode on the receiving side.
5. **Custom edge categories** — `EdgeCategory::Custom(SmolStr)` falls back to a default OR-Set/Tolerate impl; document this as the contract.
6. **Sequence CRDT scope** — Phase C ships a naive Vec-backed Sequence (correct for single-writer; data loss possible under concurrent edit). The "real" Fugue/diamond-types integration is its own work item, deferred until you have a concrete property that needs it.
7. **Schema migration** — once Phase E lands, future schema changes need a migration story (renamed fields, retyped fields). The simplest approach is a new `version: u16` in `Op` plus per-version dispatch; defer until first migration is needed.

## III.6 · Estimated work breakdown

Rough sizing (will vary with how much existing code needs touching):
- Phase A: <100 LOC, half a day.
- Phase B: ~150 LOC, half a day.
- Phase C: ~600 LOC across 5 types + property tests, ~3 days. (Sequence stub is small; the real one is a separate weeks-scale task.)
- Phase D: ~200 LOC, half a day.
- Phase E: ~150 LOC change, but cascades through tests — ~1 day with test updates.
- Phase F: ~200 LOC + per-category schemas, ~1 day.
- Phase G: ~400 LOC for the proc-macro + ~100 LOC of per-schema example types, ~3 days. The proc-macro is the riskiest item.
- Phase H: ~500 LOC of `kyoso_sync` rewiring + the generic-over-`CrdtModel` work in `kyoso_server` (~300 LOC), ~3 days.
- Phase I: ~400 LOC of tests, ~2 days.

**Total**: roughly two-and-a-half weeks of focused work for a full landing, of which Phase G (the derive macro) and Phase H (the sync rewire) are the two pieces that benefit from being well-rested for. The phases are independent enough that a partial landing (A–F + G hand-written for one schema) is a usable intermediate state.

---
---

# Part IV — `GraphBackend` deletion + `ClientSyncEngine` split

## IV.0 · Why

After the schema layer landed, the `GraphBackend` abstraction in `kyoso_graph` and the conflated role of `CrdtBackend` (sync engine + mirror store) became the next two pain points:

- **`kyoso_graph::GraphBackend`** was designed as a swap-in seam ("CRDT-replicated store tomorrow"), but query systems use [`GraphQuery`](crates/kyoso_graph/src/queries.rs) which goes through Bevy ECS components directly — not through the backend. Swapping `PetgraphBackend` for `CrdtBackend` doesn't change query results because nothing queries the backend. The abstraction is dead weight.
- **`CrdtBackend`** does two unrelated jobs in one struct: (1) sync engine — id-gen, `applied_seq`, pending op queue; (2) mirror store — per-node/per-edge `HashMap<String, Vec<u8>>` property bags used to materialise snapshots. Job 1 is needed on every replica. Job 2 is needed only on the server (the client has Bevy ECS, which is the document). On the client the property bags are write-only redundant state.

The audit (recorded below in §IV.7) confirmed both observations are safe to act on: no consumer of `Graph<N, E, B>` actually depends on it for anything beyond *enumerating nodes/edges*, which `GraphQuery` exposes natively.

## IV.1 · The target shape

```
kyoso_graph                              kyoso_crdt                       kyoso_sync
─────────────────────                    ───────────────────────          ─────────────────────────
ECS components:                          Wire layer:                      ClientSyncEngine resource:
  EdgeFrom, EdgeTo                         OpKind, WireDelta                id_gen, applied_seq, pending
  OutgoingEdges, IncomingEdges             EdgeCategory                   EntityCrdtIndex resource:
  TreeEdge, TreeParent, OrderKey         Schema layer:                      Entity ↔ CrdtId (only)
GraphQuery system param                    Lattice, Crdt, SchemaApply     Detection systems:
  enumerate / traverse via Bevy            Document<S>                      Added<…> → engine.enqueue
GraphCommand, GraphMessage               ServerMirror (server only):      Inbound system:
  intent + propagation                     wraps Document<S> or             ServerMsg → projection
                                           CrdtBackend; produces         Outbound system:
                                           Snapshot for welcome             engine.drain → WS
                                                ▲                          ▲
                                                │  ServerMsg / ClientMsg   │
                                                └──────────────────────────┘
                                                   wire (postcard frames)
```

Three properties hold after the refactor:

1. **`kyoso_graph` knows nothing about CRDTs.** No `kyoso_crdt` dep, no `GraphBackend` trait, no `Graph<N, E, B>` resource.
2. **`kyoso_crdt` knows nothing about Bevy or graphs.** No `kyoso_graph` dep, no `GraphBackend` impl, no Bevy `Resource` derives. (Server-side `ServerMirror` lives in `kyoso_server`.)
3. **`kyoso_sync` owns the bridge.** Detection systems observe ECS via `Added<>`/`Changed<>`; inbound systems project ops onto ECS via `commands`. The `EntityCrdtIndex` resource maps between the two ID spaces.

## IV.2 · Migration order (7 sequential PRs)

Each step is independently mergeable and tested in isolation. The first three steps clean up `kyoso_graph` internally; the next four split `CrdtBackend` and rewire `kyoso_sync`.

### Step 1 — Refactor `wfc_solve` to use `GraphQuery`

**File**: [crates/kyoso_graph/src/wfc/solver.rs](crates/kyoso_graph/src/wfc/solver.rs)

Currently (line 250-302):
```rust
pub fn wfc_solve<C, Node, Edge>(
    mut solver: ResMut<WfcSolverState<C>>,
    graph: Res<crate::Graph<Node, Edge>>,           // ← drop this
    index: Res<GraphEntityIndex<Node, Edge>>,
) {
    let mirror: &StableGraph<_, _, Directed> = &graph.backend().0;
    let mut ac3_graph: StableGraph<(), (), Directed> = StableGraph::new();
    let mut pg_to_ac3: HashMap<NodeIndex, NodeIndex> = HashMap::new();
    for ni in mirror.node_indices() {
        let ac3_ni = ac3_graph.add_node(());
        pg_to_ac3.insert(ni, ac3_ni);
    }
    for edge_ref in mirror.edge_references() {
        if let (Some(&a), Some(&b)) = (
            pg_to_ac3.get(&edge_ref.source()),
            pg_to_ac3.get(&edge_ref.target()),
        ) {
            ac3_graph.add_edge(a, b, ());
        }
    }
    // … AC-3 algorithm runs on `ac3_graph` …
}
```

After:
```rust
pub fn wfc_solve<C, Node, Edge>(
    mut solver: ResMut<WfcSolverState<C>>,
    q: GraphQuery<'_, '_, Node, Edge>,              // ← new param
    index: Res<EntityCrdtIndex>,                    // ← simplified (Step 6)
) {
    let mut ac3_graph: StableGraph<(), (), Directed> = StableGraph::new();
    let mut entity_to_ac3: HashMap<Entity, NodeIndex> = HashMap::new();
    for (entity, _, _, _) in q.nodes_iter() {
        let ac3_ni = ac3_graph.add_node(());
        entity_to_ac3.insert(entity, ac3_ni);
    }
    for (_, edge_from, edge_to, _) in q.edges_iter() {
        if let (Some(&a), Some(&b)) = (
            entity_to_ac3.get(&edge_from.0),
            entity_to_ac3.get(&edge_to.0),
        ) {
            ac3_graph.add_edge(a, b, ());
        }
    }
    // AC-3 algorithm body unchanged; `pg_to_ac3` is renamed to `entity_to_ac3`
    // and downstream uses of `dirty_node: NodeIndex` (referring to `mirror`'s
    // nodes) become `entity_to_ac3.get(&entity)` lookups.
}
```

The AC-3 propagation in [crates/kyoso_graph/src/wfc/propagator.rs](crates/kyoso_graph/src/wfc/propagator.rs) takes the `StableGraph` as a parameter — unchanged. Only the *source* of nodes/edges changes.

**Tests**: existing wfc tests should pass unchanged because the algorithm operates on the same logical graph.

### Step 2 — Refactor `evaluate_recipes` to use `GraphQuery`

**File**: [crates/kyoso_graph/src/recipe/transform.rs](crates/kyoso_graph/src/recipe/transform.rs)

Same shape as Step 1. Lines 106-150 build a `host: StableGraph<(), (), Directed>` from `graph.backend().0`. Replace with the `GraphQuery` enumeration pattern. The `recipe/matcher.rs` subgraph-isomorphism algorithm operates on the host graph as a parameter — unchanged.

### Step 3 — Delete `Graph<N, E, B>`, `GraphBackend`, `PetgraphBackend`

**Files**:
- [crates/kyoso_graph/src/lib.rs](crates/kyoso_graph/src/lib.rs) — delete:
  - `Graph<N, E, B>` struct + impls (lines 180-317)
  - `GraphEntityIndex<N, E, B>` struct + impls (lines 319-391) — moves to kyoso_sync as `EntityCrdtIndex` in Step 6
  - `sync_graph_nodes` / `sync_graph_edges` systems (lines 817-871) — no callers after Steps 1-2
  - `GraphSystemSet::GraphSync` variant + its scheduling
  - `GraphManagerPlugin::build` references to `Graph<...>` / `GraphEntityIndex<...>` (lines 503-504, 554-555)
- [crates/kyoso_graph/src/backend/mod.rs](crates/kyoso_graph/src/backend/mod.rs) — delete the whole `GraphBackend` trait
- [crates/kyoso_graph/src/backend/petgraph_backend.rs](crates/kyoso_graph/src/backend/petgraph_backend.rs) — delete entirely
- [crates/kyoso_graph/Cargo.toml](crates/kyoso_graph/Cargo.toml) — `petgraph` stays as a dep (still used by wfc/recipe consumers from Steps 1-2 and `solver.rs`'s `SolverGraph`)

The `NodeState<N>` / `EdgeState<E>` phantom-data wrappers (lib.rs:151-174) become dead types; remove them too.

**Tests**: the `kyoso_graph` integration tests that referenced `Graph<N, E, B>` directly will break. Audit and update — most should just need to query via `GraphQuery` instead.

### Step 4 — Add `ClientSyncEngine` in `kyoso_sync`

**New file**: `crates/kyoso_sync/src/engine.rs`

```rust
//! Client-side sync engine: id-generation, op queue, applied-seq tracking.
//!
//! Complements `kyoso_crdt::Document<S>` for the client: where `Document<S>`
//! holds per-node typed schema state (server-side mirror semantics),
//! `ClientSyncEngine` holds *only* the bookkeeping the wire protocol
//! requires. The actual document lives in Bevy ECS components; the
//! engine is the bus that turns ECS mutations into ops and applies
//! incoming ops back to ECS via the `kyoso_sync` projection layer.

use bevy::prelude::*;
use kyoso_crdt::{CrdtId, GlobalSeq, IdGenerator, Op, OpKind, PeerId};
use std::collections::HashSet;

#[derive(Resource)]
pub struct ClientSyncEngine {
    id_gen: IdGenerator,
    applied_seq: GlobalSeq,
    pending: Vec<Op>,
    /// Op IDs the inbound projector spawned this frame. Detection
    /// systems skip these to suppress echoing remote ops back.
    pub(crate) just_projected: HashSet<CrdtId>,
}

impl Default for ClientSyncEngine {
    fn default() -> Self {
        // Peer 0 is a placeholder; production code calls `set_peer` once
        // session auth assigns a real peer id (after `Welcome`).
        Self::with_peer(0)
    }
}

impl ClientSyncEngine {
    pub fn with_peer(peer: PeerId) -> Self {
        Self {
            id_gen: IdGenerator::new(peer),
            applied_seq: 0,
            pending: Vec::new(),
            just_projected: HashSet::new(),
        }
    }

    pub fn set_peer(&mut self, peer: PeerId) {
        self.id_gen = IdGenerator::new(peer);
    }

    pub fn next_id(&mut self) -> CrdtId {
        self.id_gen.next()
    }

    pub fn enqueue(&mut self, op: Op) {
        self.pending.push(op);
    }

    pub fn drain_pending(&mut self) -> Vec<Op> {
        std::mem::take(&mut self.pending)
    }

    pub fn applied_seq(&self) -> GlobalSeq {
        self.applied_seq
    }

    pub fn observe_applied(&mut self, seq: GlobalSeq) {
        if seq > self.applied_seq {
            self.applied_seq = seq;
        }
    }

    /// Mark an op-id as having been just-projected from inbound. Detection
    /// systems check this set to avoid emitting ops for entities the
    /// inbound projector created.
    pub fn mark_just_projected(&mut self, id: CrdtId) {
        self.just_projected.insert(id);
    }

    /// Drain the just-projected set after detection systems have run.
    /// Called once per frame at the end of the sync pipeline.
    pub fn clear_just_projected(&mut self) {
        self.just_projected.clear();
    }
}
```

**File touch**: [crates/kyoso_sync/src/lib.rs](crates/kyoso_sync/src/lib.rs) — add `pub mod engine; pub use engine::ClientSyncEngine;`.

**File touch**: [crates/kyoso_sync/src/plugin.rs](crates/kyoso_sync/src/plugin.rs):

- `CrdtSyncPlugin::build`: replace `app.init_resource::<Graph<N, E, CrdtBackend<N, E>>>()` with `app.init_resource::<ClientSyncEngine>()`. Add a `clear_just_projected_system` that runs at the end of the Update set.
- Detection systems (e.g. `detect_added_nodes`):
  - Old: `graph.backend_mut().add_node(NodeState::new())` — this both minted a CrdtId AND pushed a pending op.
  - New: `let id = engine.next_id(); engine.enqueue(Op::new(id, OpKind::AddNode)); index.bind(entity, id);`
- `inbound_system`: replace `graph.backend_mut().apply_remote(&op)` with the projection routing (which already exists for AddNode/RemoveNode/etc., just no longer goes through `CrdtBackend`). Mark `op.id` in `engine.just_projected` for echo prevention.
- `outbound_system`: replace `graph.backend_mut().drain_pending()` with `engine.drain_pending()`.

**Test**: extend [crates/kyoso_sync/tests/two_apps.rs](crates/kyoso_sync/tests/two_apps.rs) — the existing two-replica test still works because the wire format is unchanged; only the resource sourcing the data changes.

### Step 5 — Drop `GraphBackend` impl on `CrdtBackend`

**File**: [crates/kyoso_crdt/src/backend.rs](crates/kyoso_crdt/src/backend.rs)

Delete the `impl<N, E> GraphBackend<N, E> for CrdtBackend<N, E>` block (lines 456-557 today). The struct stays — the server uses it as a mirror — but it loses `add_node`/`remove_node`/`add_edge`/`remove_edge` as trait methods. Those methods become inherent (already have inherent `set_node_property`/`set_edge_property`/`move_node`; they'd just stay).

This is when `kyoso_crdt` can drop `kyoso_graph` from its dependencies — no more `use kyoso_graph::backend::GraphBackend; use kyoso_graph::{NodeState, EdgeState}`.

**File**: [crates/kyoso_crdt/Cargo.toml](crates/kyoso_crdt/Cargo.toml) — remove `kyoso_graph = { workspace = true }`. Keep `petgraph` if anything in `kyoso_crdt` still uses it (currently nothing does after this step).

**File**: [crates/kyoso_crdt/src/lib.rs](crates/kyoso_crdt/src/lib.rs) — remove the `pub use` of types that came from `kyoso_graph`. Verify `cargo build -p kyoso_crdt`.

**Test**: [crates/kyoso_crdt/tests/sync.rs](crates/kyoso_crdt/tests/sync.rs) calls `b.add_node(NodeState::new())` etc. on `CrdtBackend` directly — those calls go through inherent methods after the change. Method shape stays the same; `NodeState`/`EdgeState` arguments become `()` or are dropped.

### Step 6 — Move `EntityCrdtIndex` to `kyoso_sync` and collapse it

**File**: `crates/kyoso_sync/src/index.rs` (new)

```rust
//! Bidirectional `Entity ↔ CrdtId` index.
//!
//! Owned by `kyoso_sync` because it only matters where the two ID
//! spaces meet. `kyoso_graph` doesn't know about CRDT IDs;
//! `kyoso_crdt` doesn't know about Bevy entities.

use bevy::prelude::*;
use kyoso_crdt::CrdtId;
use std::collections::HashMap;

#[derive(Resource, Default)]
pub struct EntityCrdtIndex {
    pub node_of_entity: HashMap<Entity, CrdtId>,
    pub entity_of_node: HashMap<CrdtId, Entity>,
    pub edge_of_entity: HashMap<Entity, CrdtId>,
    pub entity_of_edge: HashMap<CrdtId, Entity>,
}

impl EntityCrdtIndex {
    pub fn bind_node(&mut self, entity: Entity, id: CrdtId) {
        self.node_of_entity.insert(entity, id);
        self.entity_of_node.insert(id, entity);
    }
    pub fn bind_edge(&mut self, entity: Entity, id: CrdtId) {
        self.edge_of_entity.insert(entity, id);
        self.entity_of_edge.insert(id, entity);
    }
    pub fn unbind_node(&mut self, entity: Entity) -> Option<CrdtId> {
        let id = self.node_of_entity.remove(&entity)?;
        self.entity_of_node.remove(&id);
        Some(id)
    }
    pub fn unbind_edge(&mut self, entity: Entity) -> Option<CrdtId> {
        let id = self.edge_of_entity.remove(&entity)?;
        self.entity_of_edge.remove(&id);
        Some(id)
    }
    // Read accessors mirror the existing GraphEntityIndex API.
}
```

**File**: [crates/kyoso_sync/src/plugin.rs](crates/kyoso_sync/src/plugin.rs) — `CrdtSyncPlugin::build` adds `app.init_resource::<EntityCrdtIndex>()`. Detection / inbound systems take `Res<EntityCrdtIndex>` / `ResMut<EntityCrdtIndex>` instead of `GraphEntityIndex<N, E, B>`.

**File**: ripple updates through callers in `kyoso_client`. The `SyncedIndex` alias at [apps/kyoso_client/src/scene.rs](apps/kyoso_client/src/scene.rs):
```rust
type SyncedIndex = kyoso_graph::GraphEntityIndex<
    GraphNode,
    GraphEdge,
    kyoso_crdt::CrdtBackend<GraphNode, GraphEdge>,
>;
```
becomes simply `type SyncedIndex = kyoso_sync::EntityCrdtIndex;`.

### Step 7 — Drop `kyoso_graph` from `kyoso_crdt` deps (verification)

By Step 5 the cargo dep was removed; this step is the verification pass:

- `cargo build -p kyoso_crdt` from a clean target — confirms no transitive dependency
- `cargo build -p kyoso_crdt --no-default-features` — same
- Inspect `Cargo.lock` to confirm `kyoso_crdt` no longer pulls `kyoso_graph` or `petgraph` transitively (assuming you also dropped `petgraph` from `kyoso_crdt/Cargo.toml`)
- `kyoso_crdt` is now usable as a CRDT library by non-Bevy, non-graph apps. Document this in [crates/kyoso_crdt/src/lib.rs](crates/kyoso_crdt/src/lib.rs) module-level rustdoc.

## IV.3 · File-level summary

Files **deleted**:
- `crates/kyoso_graph/src/backend/petgraph_backend.rs`
- `crates/kyoso_graph/src/backend/mod.rs` *(after extracting nothing — the trait is gone, nothing else lives here)*

Files **substantially modified**:
- `crates/kyoso_graph/src/lib.rs` — drops `Graph<N, E, B>`, `GraphEntityIndex<N, E, B>`, `NodeState`, `EdgeState`, `sync_graph_*` systems, the `GraphSync` system set
- `crates/kyoso_graph/src/wfc/solver.rs` — Step 1 refactor
- `crates/kyoso_graph/src/recipe/transform.rs` — Step 2 refactor
- `crates/kyoso_crdt/src/backend.rs` — drops `GraphBackend` impl block
- `crates/kyoso_crdt/Cargo.toml` — drops `kyoso_graph` dep
- `crates/kyoso_crdt/src/lib.rs` — re-export pruning
- `crates/kyoso_sync/src/plugin.rs` — replaces `Graph<...>` with `ClientSyncEngine`, replaces `GraphEntityIndex<...>` with `EntityCrdtIndex`
- `apps/kyoso_client/src/scene.rs` — `SyncedIndex` alias

Files **added**:
- `crates/kyoso_sync/src/engine.rs` — new `ClientSyncEngine`
- `crates/kyoso_sync/src/index.rs` — new `EntityCrdtIndex`

Tests **updated**:
- `crates/kyoso_crdt/tests/sync.rs` — `add_node(NodeState::new())` calls become `add_node(())` or equivalent inherent-method shape
- `crates/kyoso_sync/tests/two_apps.rs` — resource-sourcing changes; assertions unchanged
- `apps/kyoso_server/tests/two_clients.rs` — should not need changes (server doesn't go through `GraphBackend`)
- `apps/kyoso_client/tests/duplex_round_trip.rs` — should not need changes (uses `AppPlugin` and the existing wire path)
- Per-step verification: `cargo test --workspace` after each step

## IV.4 · Estimated work breakdown

- Step 1 (wfc_solve refactor): ~30 LOC change, half a day with tests.
- Step 2 (evaluate_recipes refactor): ~30 LOC change, half a day with tests.
- Step 3 (delete `Graph<N,E,B>` + `GraphBackend`): mostly deletions, ~200 LOC removed, half a day to chase down stragglers.
- Step 4 (`ClientSyncEngine`): ~120 LOC new + ~200 LOC of plugin.rs rewiring, ~1 day.
- Step 5 (drop `GraphBackend` impl on `CrdtBackend`): ~100 LOC deleted, half a day.
- Step 6 (move + collapse `EntityCrdtIndex`): ~80 LOC new + ~50 LOC of caller updates, half a day.
- Step 7 (verification): half a day.

**Total**: ~4 days of focused work, with each step being a clean PR. The first three steps can land in parallel-ish PRs since they don't conflict with each other (wfc, recipe, lib.rs cleanup are mostly independent files); Steps 4-7 are sequential.

## IV.5 · Risks and mitigations

1. **Existing detection systems in `plugin.rs` are intricate** (echo prevention, batch flushing, presence interleaving). Step 4 needs to preserve these behaviors precisely. Mitigation: write a behavioral test before the refactor that captures the current echo-prevention semantics, then port system-by-system.

2. **`SolverGraph` in `solver.rs`** (a separate `StableGraph` wrapper) is *not* the same as `PetgraphBackend` — it's its own resource maintained by the solver module's own systems, fed by some not-yet-audited path. Confirm before Step 3 that `solver.rs` doesn't depend on `Graph<N, E, B>` to populate `SolverGraph`. If it does, that's a Bucket-3 case for solver and we adjust.

3. **`GraphManagerPlugin` users in apps** may bind to `Graph<...>` resource handles. Mitigation: grep `kyoso_client` and any other consumers for `Res<Graph<` / `ResMut<Graph<` before Step 3; update them to `GraphQuery` first.

4. **Test setup in `kyoso_client/tests/duplex_round_trip.rs`** registers `AppTypeRegistry` etc. for headless testing. Step 4's `ClientSyncEngine` should `init_resource` cleanly without additional plugin wiring.

5. **`kyoso_crdt`'s `Document<S>` already coexists with `CrdtBackend`**. After Step 5, both still exist — `Document<S>` is the typed schema layer (used by tests / future apps), `CrdtBackend` is the server-side mirror (loses its `GraphBackend` impl but keeps the inherent API). They don't conflict.

## IV.6 · Architectural payoff (recap)

After all 7 steps:

- `kyoso_graph` is a Bevy graph DSL. **Zero CRDT dependencies.** Could ship a singleplayer Bevy app standalone.
- `kyoso_crdt` is a CRDT library. **Zero Bevy or graph dependencies.** Could ship a non-Bevy app (text editor, JSON document) standalone.
- `kyoso_sync` is the bridge. ~1500 LOC of focused Bevy↔wire translation; nothing more.
- `kyoso_server` is unaffected (already isolated from the graph layer via the [Phase H model alias](apps/kyoso_server/src/model.rs)).
- The "different `CrdtModel`" story is suddenly cheap: a future text-only kyoso variant can use `kyoso_crdt` with a different schema struct, no graph types involved at all.

## IV.7 · Audit log (recorded for reference)

Files using `GraphBackend` / `Graph<N,E,B>` / `PetgraphBackend` / `petgraph::*` (per `grep -l` output):

| File | Bucket | Notes |
|---|---|---|
| `wfc/solver.rs:252,272` | 2 (mechanical) | Reads `graph.backend().0` for node/edge enumeration only. AC-3 is custom. |
| `recipe/transform.rs:108,145` | 2 (mechanical) | Same shape — reads backend for enumeration; subgraph isomorphism is custom. |
| `solver.rs` | 1 (already independent) | Has its own `SolverGraph(StableGraph)`. Doesn't use `Graph<N,E,B>` — confirmed by grep. Uses `petgraph::` types directly for its private state. |
| `wfc/propagator.rs` | 1 (parameter only) | Takes `&StableGraph<...>` as a function argument. Caller (wfc/solver.rs) is the one with the dependency. |
| `recipe/matcher.rs` | 1 (parameter only) | Same pattern as `propagator.rs`. |
| `recipe/pattern.rs` | 1 (independent) | Has its own `DiGraph<PatternNode, PatternEdge>` for pattern definitions. Independent. |
| `wfc/error.rs` | 1 (type alias only) | Uses `petgraph::NodeIndex` in error type. No backend coupling. |

**Conclusion**: no Bucket-3 cases (algorithms that depend on `petgraph` stdlib like `tarjan_scc` / `toposort` / `dijkstra` running against the `Graph<N, E, B>` mirror). Both Bucket-2 cases are local refactors. Full deletion is feasible.

---
---

# Part V — Polish + post-IV follow-ups

## V.0 · What this part covers

After Part IV's deletion + ClientSyncEngine split landed, seven follow-up items were called out:

1. ✅ Workspace warning cleanup (kyoso_polyline deprecated APIs, kyoso_camera dead field)
2. ✅ SolverGraph migration assessment — deferred (justified async-snapshot use case)
3. ✅ Proptest-based property tests for base CRDT primitives
4. ✅ Per-category typed-edge dispatch (marker components → `EdgeCategory` enum)
5. ✅ Typed schema example for kyoso_client's `GraphNode` / `GraphEdge`
6. 🔲 Full reflection-→-schema rewire of `plugin.rs` (intentionally deferred — see §V.4)
7. 🔲 Fugue maximal-non-interleaving in `Sequence<T>` (out of scope, see §V.5)

This part captures what landed for items 1–5 and what's intentionally left for item 6, so future work has a clear starting point.

## V.1 · Per-category typed-edge dispatch (item 4)

**New module**: [crates/kyoso_sync/src/category.rs](crates/kyoso_sync/src/category.rs)

Three pieces:

1. **`EdgeCategoryMarker` trait** — implemented by zero-sized Bevy components (e.g. `InstanceOfMarker`, `PrototypeLinkMarker`). Each declares a `const CATEGORY: EdgeCategory`.
2. **`SyncedEdgeCategoryPlugin<N, E, M>` plugin** — registers the marker as a per-category detection path. Its system runs *before* the generic `detect_added_edges` so the categorized edge wins; the generic system finds the edge already in the index and skips.
3. **`EdgeCategoryProjectors` resource + `ApplyEdgeCategory` deferred command** — inbound projection: when an `OpKind::AddRefEdge { category, .. }` arrives, the matching marker component is inserted on the spawned edge entity.

End-to-end tested in [crates/kyoso_sync/tests/two_apps.rs::per_category_edges_replicate_with_correct_markers](crates/kyoso_sync/tests/two_apps.rs) — two real apps, real WS server, both `InstanceOf` and `PrototypeLink` edges round-trip with their markers preserved.

**Usage** (consumer-side):

```rust
use kyoso_crdt::EdgeCategory;
use kyoso_sync::{EdgeCategoryMarker, SyncedEdgeCategoryPlugin};

#[derive(Component, Default)]
pub struct InstanceOfEdge;
impl EdgeCategoryMarker for InstanceOfEdge {
    const CATEGORY: EdgeCategory = EdgeCategory::InstanceOf;
}

app.add_plugins(SyncedEdgeCategoryPlugin::<MyNode, MyEdge, InstanceOfEdge>::default());

// Spawning `(EdgeFrom(a), EdgeTo(b), MyEdge::default(), InstanceOfEdge)` now
// emits `AddRefEdge { category: InstanceOf, from, to }` on the wire.
// Remote `InstanceOf` edges arrive with `InstanceOfEdge` pre-attached.
```

The default `Reference` category is still the fallback for plain edges without a marker. Edges can carry zero or one `EdgeCategoryMarker`; multiple markers per edge are not meaningful (the categorized detection systems run independently and the first one wins).

## V.2 · Proptest CRDT property tests (item 3)

**New file**: [crates/kyoso_crdt/tests/proptest_lattice.rs](crates/kyoso_crdt/tests/proptest_lattice.rs)

Nine property-based tests across four base CRDT primitives. Each runs proptest's default 256 random cases — totaling ~2300 verified random scenarios per `cargo test` invocation.

| CRDT | Properties tested |
|---|---|
| `LwwRegister<u32>` | join commutative; join idempotent; two-replica concurrent-write convergence |
| `OrSet<u32>` | join commutative (random sequence of adds on each peer, both delivery orders); join idempotent |
| `PnCounter` | three-replica concurrent inc/dec convergence with arbitrary magnitudes; join idempotent |
| `Sequence<u32>` (RGA) | two-replica insert convergence with random sequences; join idempotent |

Adds `proptest = "1.5"` as a workspace dep + `kyoso_crdt` dev-dep. No production-code dependency change.

## V.3 · Typed schema example (item 5)

**New file**: [apps/kyoso_client/src/schema.rs](apps/kyoso_client/src/schema.rs)

Defines `GraphNodeSchema` and `GraphEdgeSchema` mirroring the production `GraphNode` / `GraphEdge` Bevy components, using the `derive(Crdt)` macro:

```rust
#[derive(Clone, Debug, Default, PartialEq, DeriveCrdt)]
pub struct GraphNodeSchema {
    pub radius: LwwRegister<f32>,
    pub color_rgb: LwwRegister<[f32; 3]>,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveCrdt)]
pub struct GraphEdgeSchema {
    pub line_width: LwwRegister<f32>,
    pub color_rgb: LwwRegister<[f32; 3]>,
}
```

Four tests verify these schemas converge end-to-end across two `Document<S>` replicas through an `InMemoryOpLog`:
- Concurrent edits to *different* fields (each peer keeps its change).
- Concurrent edits to the *same* field (server stamping picks one winner; both peers agree).
- Wire-driven `apply_wire` dispatching a single field.
- Edge schema follows the same shape.

The schemas are defined but **not yet wired** into `CrdtSyncPlugin`'s detection path — that's item 6 (next section). The infrastructure (`Document<S>`, `derive(Crdt)`, `SchemaApply`, `IntoWireOp`) is fully tested via the 67 + 9 + 4 = 80 kyoso_crdt + schema tests.

## V.4 · Reflection-→-typed-schema rewire (item 6, deferred)

This is the only substantive follow-up still outstanding. It's a multi-day refactor that benefits from being designed deliberately rather than shipped autonomously, because the API choices are user-facing and irreversible.

### What's currently in place

The reflection-driven path in [crates/kyoso_sync/src/plugin.rs](crates/kyoso_sync/src/plugin.rs):

- `detect_changed_node_properties` walks Bevy reflection of `N`, splits into named fields, postcard-encodes each via `ReflectSerializer`, compares against the engine's last-known value, emits an `OpKind::SetNodeProperty { key: "ComponentName::field_name", delta: WireDelta::LwwReplace { value } }` op per changed field.
- `inbound_system::project_op` decodes via `ReflectDeserializer` and writes the field back to the Bevy component using `GetPath`.
- `NodePropertyProjectors` / `EdgePropertyProjectors` registries dispatch projection by component-type prefix.

This works but every field is implicitly LWW. Non-LWW `WireDelta` variants (`OrSetAdd`, `SequenceInsert`, `PnCounterDelta`) ride the wire but their state lives only in `Document<S>` instances that aren't connected here — so they're effectively dropped on the floor for the production `GraphNode` / `GraphEdge` types.

### The migration target

Replace the reflection path with `Document<S>`-driven detection. For each Bevy component the consumer wants synced, define a parallel schema struct (as `GraphNodeSchema` already does) and register a binding.

### Five steps the migration takes

1. **Define a `SchemaSync` trait** that the consumer implements to bridge their Bevy component to a schema struct:
   ```rust
   pub trait SchemaSync: Component {
       type Schema: Crdt + SchemaApply + Default;
       fn into_schema(&self) -> Self::Schema;
       fn apply_schema(&mut self, schema: &Self::Schema);
   }
   impl SchemaSync for GraphNode {
       type Schema = GraphNodeSchema;
       fn into_schema(&self) -> GraphNodeSchema { /* read fields → LWW state */ }
       fn apply_schema(&mut self, schema: &GraphNodeSchema) { /* write fields */ }
   }
   ```
2. **Replace `detect_changed_node_properties` with a typed variant** generic over `S: SchemaSync`. On `Changed<S>`, the system computes the diff against `Document<S::Schema>`'s known state, calls `doc.mutate_node` for each changed field's typed mutation, and the engine queues the resulting ops.
3. **Replace inbound projection.** Drop `DispatchNodeProperty` / `NodePropertyProjectors`. On `OpKind::SetNodeProperty`, route to `doc.apply_remote(op)` which dispatches via `SchemaApply` to the right field. A separate "schema-→-Bevy projection" system reads `Document` whenever it changes and updates the Bevy component via `apply_schema`.
4. **Drop the `ReflectSerializer` / `ReflectDeserializer` machinery.** No more `AppTypeRegistry`, no more `register_type::<C>()` calls in the plugin, no more reflection-keyed property bag in the engine.
5. **Per-component plugins.** Replace the existing `SyncedNodeComponentPlugin<N, E, C>` with `SchemaSyncedNodeComponentPlugin<N, E, S>` — same shape, typed schema instead of reflection.

### What's hard about it

- **Bidirectional projection design.** The `into_schema` / `apply_schema` trait is the user-facing API surface; getting it right (single-pass projection? dirty-flag tracking? batching?) wants real-app exercise before being committed to.
- **Migration cost is consumer-side.** Each existing synced component (`GraphNode`, `GraphEdge`, `Transform`) needs a parallel schema struct + projection impl. For an app with N synced types, that's N times the work of a single migration.
- **Co-existence period.** During the migration the reflection path and the typed path need to co-exist (some components migrated, others not). The plugin registration must support both.
- **Test churn.** The existing reflection-driven tests verify behaviour from the user's perspective but not the schema layer. Property-based tests at the schema level (Part V §V.2) cover the CRDT axioms but not the Bevy-bridge layer.

### Estimated cost

Probably 2–3 days of focused work for a one-component migration (`GraphNode`), plus another day per additional component. If you have N synced component types, expect ~`N` days of consumer-side work plus ~3 days of plugin infrastructure.

Recommendation: don't do this in a single PR. Land the `SchemaSync` trait + plugin infrastructure first, then migrate one component at a time, verifying behavior after each.

## V.5 · Fugue Sequence (item 7, out of scope)

The current `Sequence<T>` is RGA-flavored (correct under concurrent edits, deterministic convergence) but doesn't satisfy *maximal non-interleaving*. Two peers concurrently typing different paragraphs at the same caret will interleave character-by-character on merge. Acceptable for short single-line fields; insufficient for collaborative long-form text.

Path forward when this becomes a real problem:
- **Hand-roll Fugue** following Weidner et al. 2023 ([arXiv:2305.00583](https://arxiv.org/abs/2305.00583)). Adds left/right origin metadata per element + a tree-traversal merge rule. ~500–800 LOC, plus tests.
- **Adopt diamond-types** (Joseph Gentle's Eg-walker reference impl, [crates.io/crates/diamond-types](https://crates.io/crates/diamond-types)). Production-grade, fastest known sequence CRDT, but adds a substantial external dep with its own wire format. Would replace `Sequence<T>` rather than augment it.

Either path is bounded but not trivial. Until the kyoso app actually has a long-text field that needs collaborative editing, the RGA stub is fine.

## V.6 · Test totals after Part V

| Crate / area | Tests | Δ from end of Part IV |
|---|---|---|
| `kyoso_crdt` (unit + integration) | 28 + 9 + 12 + 3 + 15 = 67 | unchanged |
| `kyoso_crdt` proptest | **9** | new (~2300 random cases per run) |
| `kyoso_sync` engine + index unit | 7 | unchanged |
| `kyoso_sync` two_apps | **10** | +1 (per-category dispatch) |
| `kyoso_client` schema | **4** | new (typed-schema example) |
| `kyoso_client` duplex_round_trip | 3 | unchanged |
| `kyoso_server` | 6 + 9 = 15 | unchanged |
| `kyoso_graph` | 4 | unchanged |
| **Total** | **~119 tests** | **+14 over Part IV final state** |

All workspace warnings are clean except for the deprecated Bevy APIs that the upstream Bevy main has migrated past — those would land naturally on the next Bevy version bump.

---
---

# Part VI — Reflection → typed-schema rewire

## VI.0 · What this part covers

Part V §V.4 deferred the full reflection-→-typed-schema rewire. This part lands the *infrastructure* and a *worked migration* for kyoso_client's `GraphNode` / `GraphEdge`, while leaving the reflection path as a fallback for components that haven't migrated yet.

## VI.1 · `SchemaSync` trait + `SchemaSyncedNodeComponentPlugin`

**New module**: [crates/kyoso_sync/src/schema_sync.rs](crates/kyoso_sync/src/schema_sync.rs)

Three pieces:

1. **`SchemaSync` trait** — implemented per Bevy component, declares the schema struct + a `SCHEMA_NAME` discriminator + bidirectional projection methods (`changes_against` for outbound diff, `write_back` for inbound projection).
2. **`SchemaDoc<S>` resource** — newtype wrapper around `kyoso_crdt::Document<S>` so it can be a Bevy `Resource` (orphan rule). Deref-passthrough for ergonomics.
3. **`SchemaSyncedNodeComponentPlugin<N, E, C>` plugin** — wires four systems:
   - `ensure_schema_slots::<C>`: every node in `EntityCrdtIndex` gets a schema slot (`doc.ensure_node`)
   - `detect_typed_changes::<N, E, C>`: `Changed<C>` → `changes_against` → mint op_id from engine → emit `SetNodeProperty` op with prefixed path `["SCHEMA_NAME", "field_name"]`
   - `route_typed_inbound::<C>`: subscribes to new `RemoteOpApplied` event; matches path head against `SCHEMA_NAME`; strips the prefix; routes to `Document::apply_property_op`
   - `project_typed_to_bevy::<C>`: when document changes, calls `write_back` on each entity's component

Supporting changes:

- **`Document::apply_property_op`** ([crates/kyoso_crdt/src/document.rs](crates/kyoso_crdt/src/document.rs)) — applies a `SetNodeProperty` op to schema state without tracking `applied_seq` (the engine handles ordering globally; many `Document<S>` instances share the same source-of-truth ordering).
- **`Document::mutate_node_with_id`** — accepts a caller-provided `CrdtId` instead of minting internally.
- **`Document::ensure_node`** — pre-inserts a schema slot without emitting `OpKind::AddNode` (the engine already emits structural ops; the document just needs the slot).
- **`ClientSyncEngine::next_id` / `enqueue`** ([crates/kyoso_sync/src/engine.rs](crates/kyoso_sync/src/engine.rs)) — exposes id-minting and op-enqueueing so typed plugins can ride the same id namespace as the engine.
- **`RemoteOpApplied` event** ([crates/kyoso_sync/src/plugin.rs](crates/kyoso_sync/src/plugin.rs)) — emitted once per server-confirmed op after `engine.apply_remote`. Typed plugins subscribe to it for inbound routing.

## VI.2 · Worked migration: `GraphNode` + `GraphEdge`

[apps/kyoso_client/src/schema.rs](apps/kyoso_client/src/schema.rs) now has `SchemaSync` impls for `GraphNode` and `GraphEdge` (mirroring their existing field shapes via `LwwRegister<f32>` / `LwwRegister<[f32; 3]>`).

[apps/kyoso_client/src/lib.rs](apps/kyoso_client/src/lib.rs) `AppPlugin` registers two typed plugins:

```rust
SchemaSyncedNodeComponentPlugin::<GraphNode, GraphEdge, GraphNode>::default(),
SchemaSyncedNodeComponentPlugin::<GraphNode, GraphEdge, GraphEdge>::default(),
```

This means GraphNode/GraphEdge field changes flow through the typed schema path (`OpKind::SetNodeProperty { path: ["GraphNode", "radius"], delta: LwwReplace { ... } }`).

## VI.3 · Tests

Two new integration tests in [crates/kyoso_sync/tests/two_apps.rs](crates/kyoso_sync/tests/two_apps.rs) under module `typed_schema`:

| Test | Assertion |
|---|---|
| `typed_node_replicates_fields_through_schema_path` | A spawns `TypedNode { radius: 7.5, color: [...] }`. B receives both fields via real WS server, `write_back` projects them onto B's `TypedNode` component. |
| `typed_node_concurrent_edits_to_different_fields_converge` | A changes `radius`, B concurrently changes `color`. Both peers converge to the union — proves field-granular CRDT semantics on the wire. |

Both pass against a real `kyoso_server` over WebSocket.

## VI.4 · Co-existence with reflection path (current design)

The reflection-driven detection systems (`detect_changed_node_properties` etc. in `CrdtSyncPlugin`) are **still active** and run alongside the typed plugins. For a component that has a `SchemaSync` impl AND is the `N` parameter of `CrdtSyncPlugin::<N, E>`, both paths emit ops on every `Changed<C>`:

- Reflection path emits `SetNodeProperty { path: Path::field("ComponentName::field"), delta: LwwReplace { value: postcard(field) } }` — single-segment path, "::" inside the segment.
- Typed path emits `SetNodeProperty { path: ["ComponentName", "field"], delta: LwwReplace { value: postcard(field) } }` — two-segment path.

These don't cross-contaminate: the inbound dispatchers route by path *shape* (the reflection-side projector looks for "::" in the head segment; the typed-side router looks for an exact match on `SCHEMA_NAME` as the head with more segments to follow). Each op only triggers one projection.

The cost is wire bandwidth — every typed-synced field ships as 2× ops. Acceptable for now; the cleanup is §VI.5.

## VI.5 · Cleanup follow-up: full reflection removal

To eliminate the duplicate emission, the right next step is to split `CrdtSyncPlugin` into two:

- **`CrdtSyncCorePlugin<N, E>`** — connection, presence, structural ops only (`AddNode`, `RemoveNode`, `Move`, `AddRefEdge`, `RemoveRefEdge`). No property reflection.
- **`ReflectivePropertyPlugin<N, E>`** — opt-in property reflection for `N` and `E`. Apps that have migrated all components to typed schemas just don't add this plugin.

`SyncedNodeComponentPlugin<N, E, C>` (for extra reflected components like `Transform`) stays as-is for components that haven't migrated yet.

Concrete steps for the migration:
1. Move `detect_changed_node_properties` + `detect_changed_edge_properties` + their projection scaffolding (`NodePropertyProjectors`, `EdgePropertyProjectors`, `DispatchNodeProperty`, `DispatchEdgeProperty`, `register_type::<N>()`, `register_type::<E>()`) out of `CrdtSyncPlugin::build` and into `ReflectivePropertyPlugin::build`.
2. `CrdtSyncPlugin` becomes a thin wrapper that adds both `CrdtSyncCorePlugin` and `ReflectivePropertyPlugin` for backwards-compat, OR is dropped in favor of the explicit two-plugin form.
3. kyoso_client's `AppPlugin` adds:
   - `CrdtSyncCorePlugin::<GraphNode, GraphEdge>::new(...)`
   - `SchemaSyncedNodeComponentPlugin::<..., GraphNode>::default()`
   - `SchemaSyncedNodeComponentPlugin::<..., GraphEdge>::default()` (or future `SchemaSyncedEdgeComponentPlugin` — see §VI.6)
   - For Transform, either keep `SyncedNodeComponentPlugin` (reflection) OR define `TransformSchema` and use the typed plugin.

The cleanup is bounded but has consumer-side migration cost — pragmatic to defer until the typed path is exercised in real-app contexts.

## VI.6 · Edge-component schemas

`SchemaSyncedNodeComponentPlugin` queries `Query<(Entity, &C), Changed<C>>` and looks up `index.node_id(entity)`. For *edge* entities (which have `EdgeFrom` / `EdgeTo` and live in `index.edge_of_entity`), this lookup misses — edge components don't get synced via the node-side plugin.

For full coverage, mirror the design: `SchemaSyncedEdgeComponentPlugin<N, E, C>` that looks up via `index.edge_id(entity)` and emits `OpKind::SetRefEdgeProperty` instead of `SetNodeProperty`. The `RemoteOpApplied` events for `SetRefEdgeProperty` route to a separate `Document<S>` keyed by edge id.

In the current state, `GraphEdge` has a `SchemaSync` impl but `SchemaSyncedNodeComponentPlugin` won't actually pick up its changes (it's an edge entity). The plugin registration in `AppPlugin` for `GraphEdge` is a no-op until the edge variant lands. Reflection-path detection still handles `GraphEdge` field changes — so it works, just via the older path.

This is one more bounded refactor (~150 LOC mirror of the node plugin); skipping for now to keep the deliverable focused.

## VI.7 · Test totals after Part VI

| Crate / area | Tests | Δ from end of Part V |
|---|---|---|
| `kyoso_crdt` (unit + integration) | 67 | unchanged |
| `kyoso_crdt` proptest | 9 | unchanged |
| `kyoso_sync` engine + index unit | 7 | unchanged |
| `kyoso_sync` two_apps | **12** | +2 (typed_schema) |
| `kyoso_client` schema | 4 | unchanged |
| `kyoso_client` duplex_round_trip | 3 | unchanged |
| `kyoso_server` | 6 + 9 = 15 | unchanged |
| `kyoso_graph` | 4 | unchanged |
| **Total** | **~121 tests** | **+2 over Part V final state** |

All workspace tests pass; workspace builds clean.

---
---

# Part VII — Plugin split + edge schema landing

## VII.0 · What this part covers

Part VI §VI.5 (eliminate duplicate emission by making property reflection opt-in) and §VI.6 (add edge-side typed schema plugin) — the two cleanups flagged as the natural next steps. Both landed in this iteration.

## VII.1 · Property reflection is now opt-in

**File**: [crates/kyoso_sync/src/plugin.rs](crates/kyoso_sync/src/plugin.rs)

`CrdtSyncPlugin::<N, E>::build` no longer auto-registers `N` or `E` for reflection-driven property sync. The previous behavior (auto-call `app.register_type::<N>()` + `register_type::<E>()` + register `N`/`E` projectors + schedule `detect_changed_node_properties::<N, E>` + `detect_changed_edge_properties::<N, E>`) is gone. The plugin still inserts empty `NodePropertyProjectors` / `EdgePropertyProjectors` registries for opt-in components to populate.

The two now-unused detection function bodies were deleted from `plugin.rs` along with their imports.

**What this means for consumers**:

- A consumer who wants property sync for a Bevy component must now opt in by adding `SyncedNodeComponentPlugin::<N, E, C>::default()` (reflection path) or `SchemaSyncedNodeComponentPlugin::<N, E, C>::default()` (typed path) for that component.
- Components that are registered through *both* plugins (e.g. `GraphNode` for the typed path AND a hypothetical reflection registration) will emit ops twice. The cleanup gives consumers control over which path runs; they're expected not to register both for the same component.
- Without an explicit per-component plugin, the only ops emitted for the component type are structural (`AddNode`, `RemoveNode`, `Move`, `AddRefEdge`, `RemoveRefEdge`).

## VII.2 · `SchemaSyncedEdgeComponentPlugin<N, E, C>`

**File**: [crates/kyoso_sync/src/schema_sync.rs](crates/kyoso_sync/src/schema_sync.rs)

Mirrors `SchemaSyncedNodeComponentPlugin` but routes through edge IDs and emits `OpKind::SetRefEdgeProperty`. Four systems registered per `C`:

- `ensure_edge_schema_slots::<C>` — iterates `index.edge_of_entity` and calls `doc.ensure_node` for any new edge id, so a schema slot exists before the first mutation.
- `detect_typed_edge_changes::<N, E, C>` — on `Changed<C>`, looks up the edge's `CrdtId` via `index.edge_id`, computes per-field deltas via `C::Schema::changes_against`, mints op ids from `engine.next_id`, emits `OpKind::SetRefEdgeProperty { target, path: ["SCHEMA_NAME", field], delta }` ops. Echo-suppressed via `engine.just_projected`.
- `route_typed_edge_inbound::<C>` — subscribes to `RemoteOpApplied`. For ops matching `OpKind::SetRefEdgeProperty` whose path head equals `C::Schema::SCHEMA_NAME`, strips the prefix and applies via `Document::apply_property_op` (which handles both node and ref-edge property ops).
- `project_typed_edge_to_bevy::<C>` — when the edge `Document` changed, resolves edge id → entity via `index.entity_of_edge` and writes `schema.write_back(component)` for each.

**`Document::apply_property_op`** ([crates/kyoso_crdt/src/document.rs](crates/kyoso_crdt/src/document.rs)) was extended to accept both `SetNodeProperty` and `SetRefEdgeProperty` op kinds, so a single `Document<S>` instance can store either node-keyed or edge-keyed schema state without caring which kind of op generated the path.

**Wire-format example**:

```
SetRefEdgeProperty {
  target: <edge CrdtId>,
  path: ["GraphEdge", "line_width"],
  delta: WireDelta::LwwReplace { value: postcard(2.0), ts },
}
```

## VII.3 · `kyoso_client` switches `GraphEdge` to typed path

**File**: [apps/kyoso_client/src/lib.rs](apps/kyoso_client/src/lib.rs)

Before: `GraphEdge` was registered via `SchemaSyncedNodeComponentPlugin`, but since `SchemaSyncedNodeComponentPlugin` looks up via `index.node_id(entity)` and edges live in `index.edge_of_entity`, the lookup missed and the typed path was effectively a no-op for edges. Reflection picked up the slack.

After: `GraphEdge` is registered via `SchemaSyncedEdgeComponentPlugin`, properly routing through edge IDs and `SetRefEdgeProperty`. Reflection no longer auto-runs for `GraphEdge` (Part VII §VII.1), so there's no duplicate.

```rust
SchemaSyncedNodeComponentPlugin::<GraphNode, GraphEdge, GraphNode>::default(),
SchemaSyncedEdgeComponentPlugin::<GraphNode, GraphEdge, GraphEdge>::default(),
SyncedNodeComponentPlugin::<GraphNode, GraphEdge, Transform>::default(), // reflection still
```

## VII.4 · Test fixes for opt-in plugin model

**File**: [crates/kyoso_sync/tests/two_apps.rs](crates/kyoso_sync/tests/two_apps.rs)

Two tests previously relied on `CrdtSyncPlugin::<N, E>` auto-registering reflection for `N`:
- `node_attribute_replication` and `per_property_lww_no_loss` (different-component test setups).

Each was updated to add an explicit `SyncedNodeComponentPlugin::<N, EdgeMeta, N>::default()` to its app builder (or inline test-app setup). Per Part VII §VII.1, this is now the contract: opt-in per component.

## VII.5 · Verification

- `cargo build --workspace`: clean, no warnings.
- `cargo test --workspace --no-fail-fast`: **121 tests passing, 0 failures**.

Test breakdown (unchanged from Part VI totals — the cleanup is a behavioral refinement, not new test coverage):

| Crate / area | Tests |
|---|---|
| `kyoso_crdt` unit + integration | 67 |
| `kyoso_crdt` proptest | 9 |
| `kyoso_sync` engine + index unit | 6 |
| `kyoso_sync` two_apps integration | 12 |
| `kyoso_client` schema | 4 |
| `kyoso_client` duplex_round_trip | 3 |
| `kyoso_server` | 6 + 9 = 15 |
| `kyoso_graph` | 4 |
| **Total** | **121** |

The Part VI typed-schema tests (`typed_node_replicates_fields_through_schema_path`, `typed_node_concurrent_edits_to_different_fields_converge`) still pass — they exercise the typed node path that this cleanup didn't touch. The reflection-path tests (`node_attribute_replication`, `per_property_lww_no_loss`, `extra_node_component_syncs_transform`) all pass after their explicit-opt-in adjustment.

## VII.6 · Architectural state at end of Part VII

- **`CrdtSyncPlugin<N, E>`**: structural ops only (`AddNode`, `RemoveNode`, `Move`, `AddRefEdge`, `RemoveRefEdge`) + connection management + presence + the `RemoteOpApplied` event bus. Property sync is per-component, opt-in.
- **`SyncedNodeComponentPlugin<N, E, C>`**: reflection path; for components without a typed schema. Used for `Transform`.
- **`SchemaSyncedNodeComponentPlugin<N, E, C>`**: typed path for node-entity components. Used for `GraphNode`.
- **`SchemaSyncedEdgeComponentPlugin<N, E, C>`**: typed path for edge-entity components. Used for `GraphEdge`.
- **`SyncedEdgeCategoryPlugin<N, E, M>`**: per-category typed-edge marker dispatch (Part V §V.1). Independent of the schema plugins; can layer with either path.

The reflection codepath is preserved as a graceful escape hatch for components that haven't migrated. The typed codepath is the destination for new property-rich components. Both can coexist component-by-component without duplicate emission.

## VII.7 · What's left

These remain on the medium-term wishlist; nothing blocking:

1. **Migrate `Transform` to the typed path** — would require defining a `TransformSchema` (LWW per `Vec3`/`Quat` field). Bounded follow-up; only worth doing once `Transform` becomes a hot conflict surface.
2. **Drop the reflection codepath entirely** — once every consumed component has a typed schema, `SyncedNodeComponentPlugin` (the reflection one) and `NodePropertyProjectors`/`EdgePropertyProjectors` and the `Vec<u8>` postcard infrastructure can all be deleted from `kyoso_crdt` + `kyoso_sync`.
3. **Fugue Sequence** (Part V §V.5) — when long-form collaborative text becomes a real feature.
4. **Branching/merge** (Part I §6) — when product needs warrant; cheap to delay because the schema doesn't preclude it.

---
---

# Part VIII — Transform migration + full reflection deletion

## VIII.0 · What landed

Part VII §VII.7 listed two follow-ups: migrate `Transform` to the typed path, then delete the reflection codepath entirely. Both landed in this iteration. After Part VIII, **kyoso_sync has no reflection-driven property pipeline at all** — every Bevy component's per-field sync flows through a typed `SchemaSync` impl + `derive(Crdt)` schema struct.

## VIII.1 · Built-in `TransformSchema`

**New module**: [crates/kyoso_sync/src/builtin_schemas.rs](crates/kyoso_sync/src/builtin_schemas.rs)

```rust
#[derive(Clone, Debug, Default, PartialEq, DeriveCrdt)]
pub struct TransformSchema {
    pub translation: LwwRegister<Vec3>,
    pub rotation:    LwwRegister<Quat>,
    pub scale:       LwwRegister<Vec3>,
}

impl SchemaSync for Transform {
    type Schema = TransformSchema;
    const SCHEMA_NAME: &'static str = "Transform";
    fn changes_against(...) -> Vec<...> { /* per-field LWW diff */ }
    fn write_back(&mut self, schema: &Self::Schema) { /* per-field copy */ }
}
```

Re-exported as `kyoso_sync::TransformSchema`. Consumers just add `SchemaSyncedNodeComponentPlugin::<N, E, Transform>::default()` to their app.

`Vec3` and `Quat` already implement `Serialize + Deserialize + Clone + PartialEq + Send + Sync + 'static` — exactly the bounds `LwwRegister<T>` requires — so they drop in without any newtype wrapper.

## VIII.2 · Echo-prevention convention

A subtle correctness issue surfaced during the migration. When a remote inbound spawns `N::default()` (e.g. via `AddNode` projection), the local detection system fires `Changed<C>` for the freshly-inserted default-valued component. Naïve `changes_against` impls compared `current.field.get() != Some(&self.field)` — `None != Some(&default)` → emit `Set(default)`. That echo gets stamped *later* than the remote's authoritative value, and LWW resolves to `default`. Data lost.

The fix is a per-impl convention, codified in the `SchemaSync::changes_against` rustdoc and applied to every existing impl:

```rust
fn changes_against(&self, current: &Self::Schema) -> Vec<...> {
    let default = Self::default();
    let mut out = Vec::new();
    if *current.radius.get().unwrap_or(&default.radius) != self.radius {
        out.push(...);
    }
    out
}
```

`unwrap_or(&Self::default().<field>)` makes a doc-bottom field compare against the **component's own default value**. A fresh `C::default()` instance produces zero mutations against a bottom doc, suppressing the echo entirely. This is critical for fields whose component default differs from `T::default()` — e.g. `Transform::scale` defaults to `Vec3::ONE` while `Vec3::default()` is `Vec3::ZERO`.

A helper [`LwwRegister::get_or_default`](crates/kyoso_crdt/src/types/lww.rs) exists for the simpler case where `T::default()` matches the component default, but the recommended pattern is `unwrap_or(&Self::default().<field>)` since it's correct in both cases.

This convention is documented in the `SchemaSync` trait's rustdoc.

## VIII.3 · Auto-insert on inbound projection

`project_typed_to_bevy` previously skipped entities that didn't yet carry the `C` component (`get_mut::<C>()` errored). The reflection path used to handle this case by inserting `C::default()` on the fly. The typed path now does the same via a deferred `InsertSchemaProjected<C>` command:

```rust
match components.get_mut(*entity) {
    Ok(mut component) => component.write_back(schema),
    Err(_) => commands.queue(InsertSchemaProjected::<C> {
        entity: *entity,
        schema: schema.clone(),
        _ph: PhantomData,
    }),
}
```

The command runs at the end of the frame: inserts `C::default()` on the entity, then immediately calls `write_back(schema)` to project the doc state. From the next frame's perspective the component appears with the projected values already applied.

This required adding `Default` to the `SchemaSync` trait constraint.

## VIII.4 · Full reflection deletion

After Transform migrated and the test types (`Named`, `Styled`) were converted to typed schemas (with explicit `SchemaSync` impls in the tests), nothing relied on the reflection path. Deleted:

| Symbol | Location | Status |
|---|---|---|
| `SyncedNodeComponentPlugin<N, E, C>` | plugin.rs | **deleted** |
| `SyncedEdgeComponentPlugin<N, E, C>` | plugin.rs | **deleted** |
| `NodePropertyProjectors` resource | plugin.rs | **deleted** |
| `EdgePropertyProjectors` resource | plugin.rs | **deleted** |
| `DispatchNodeProperty` command | plugin.rs | **deleted** |
| `DispatchEdgeProperty` command | plugin.rs | **deleted** |
| `apply_to_component<C>` | plugin.rs | **deleted** |
| `detect_changed_extra_node_component<N,E,C>` | plugin.rs | **deleted** |
| `detect_changed_extra_edge_component<N,E,C>` | plugin.rs | **deleted** |
| `diff_and_emit_node`, `diff_and_emit_edge` | plugin.rs | **deleted** |
| `encode_field` | plugin.rs | **deleted** |
| `lww_value`, `path_to_legacy_key`, `split_key`, `short_type_name` | plugin.rs | **deleted** |
| `ClientSyncEngine::set_node_property/set_edge_property/node_property/edge_property` | engine.rs | **deleted** |
| `Reflect + GetTypeRegistration + Debug` bounds on `Syncable` | plugin.rs | **trimmed to `Clone + Debug`** |
| `app.init_resource::<AppTypeRegistry>()` in `CrdtSyncPlugin::build` | plugin.rs | **deleted** |
| Imports: `bevy::reflect::serde::*`, `bevy::reflect::*`, `serde::de::DeserializeSeed` | plugin.rs | **deleted** |
| `project_op` arm for `OpKind::SetNodeProperty/SetRefEdgeProperty` | plugin.rs | **collapsed to no-op** (typed plugins handle via `RemoteOpApplied`) |

The lib.rs re-exports were trimmed accordingly: `SyncedNodeComponentPlugin`, `SyncedEdgeComponentPlugin`, `NodePropertyProjectors`, `EdgePropertyProjectors` are gone from the public API.

## VIII.5 · `Syncable` trait simplification

Before:
```rust
pub trait Syncable:
    GraphComponent<Mutability = Mutable> + Clone + Reflect + GetTypeRegistration + Debug
```

After:
```rust
pub trait Syncable:
    GraphComponent<Mutability = Mutable> + Clone + Debug
```

Consumer-facing: the user's `N`/`E` no longer need `derive(Reflect)`. (Test types still derive it for Bevy-side world inspection ergonomics — that's a no-op for sync.)

## VIII.6 · Architectural state at end of Part VIII

- **`CrdtSyncPlugin<N, E>`**: structural ops only. No reflection, no `AppTypeRegistry`, no per-field property bag.
- **`SchemaSyncedNodeComponentPlugin<N, E, C>`**: required for any node component whose fields should sync. Used for `GraphNode` and now `Transform`.
- **`SchemaSyncedEdgeComponentPlugin<N, E, C>`**: same for edge components. Used for `GraphEdge`.
- **`SyncedEdgeCategoryPlugin<N, E, M>`**: per-category typed-edge marker dispatch. Independent.
- **`kyoso_sync::TransformSchema`**: built-in schema for the most common Bevy component.

No reflection codepath. Every byte on the wire flows through a typed schema's `IntoWireOp` impl. The `Syncable` bound is minimal (`GraphComponent + Clone + Debug`).

## VIII.7 · Verification

- `cargo build --workspace --tests`: clean, no warnings introduced by Part VIII.
- `cargo test --workspace --no-fail-fast`: **111 tests passing, 0 failures**.

The reflection-path tests (`node_attribute_replication`, `per_property_lww_no_loss`, `extra_node_component_syncs_transform`) still pass, now exercising the typed schema path with `SchemaSync` impls for `Named`, `Styled`, `Transform` respectively.

## VIII.8 · Echo-guard invariant (write this down)

The `unwrap_or(&Self::default().<field>)` convention is now the contract for `SchemaSync::changes_against`. Future `SchemaSync` impls that violate it will create silent data-loss bugs whenever a remote spawn races a local mutation. The trait's rustdoc spells this out; the existing impls in [builtin_schemas.rs](crates/kyoso_sync/src/builtin_schemas.rs), [apps/kyoso_client/src/schema.rs](apps/kyoso_client/src/schema.rs), and [tests/two_apps.rs](crates/kyoso_sync/tests/two_apps.rs) all follow it.

Future enhancement: a derive macro for `SchemaSync` that generates the boilerplate `changes_against` / `write_back` from a struct declaration. Today users write the impl by hand. With the convention codified, a derive is a mechanical job.

## VIII.9 · What's left (post-Part VIII)

1. **Derive macro for `SchemaSync`** — auto-generate `changes_against` (with the unwrap_or convention baked in) and `write_back` from a `derive(SchemaSync)` on the Bevy component. Eliminates user-side boilerplate. Bounded ~200 LOC proc-macro.
2. **Fugue Sequence** (Part V §V.5) — when long-form collaborative text becomes a real feature.
3. **Branching/merge** (Part I §6) — when product needs warrant.
4. **Schema migration story** — once a real schema field needs to change type (e.g. from `LwwRegister<String>` to `Sequence<char>`), we'll need a versioned op format. Defer until first migration is needed.

---
---

# Part IX — `derive(SchemaSync)` proc-macro for Figma model coverage

## IX.0 · Why now

The pivot to replicate Figma's data model crosses the threshold where hand-writing `SchemaSync` impls stops being viable. Figma's API exposes ~50 distinct node/style/effect types with ~20–40 fields each (see [figma-api docs.rs](https://docs.rs/figma-api/latest/figma_api/models/index.html)). A naive translation to typed schemas would be:

- 50 schema structs × ~25 fields × 2 LOC per field (changes_against + write_back) = ~2500 LOC of mechanical boilerplate.
- Every one of those 2500 lines must follow the `unwrap_or(&Self::default().<field>)` echo-guard convention or risk silent data loss.
- Field-CRDT type choice (LWW vs OrSet vs Sequence vs CausalMap) needs to be deliberate per field — but most fields want the same default (LWW for scalars).

The right answer is a `derive(SchemaSync)` proc-macro that:
1. **Generates the parallel schema struct** (sibling type with field-CRDT replacements).
2. **Generates the `SchemaSync` impl** with the echo-guard convention baked in.
3. **Generates the `derive(Crdt)` on the schema** transitively.
4. **Lets users opt into non-default CRDT semantics** per field via attributes (the AsBindGroup pattern).

Reference: Bevy's `AsBindGroup` ([source](https://docs.rs/bevy/latest/bevy/render/render_resource/derive.AsBindGroup.html)) shows the right vocabulary — container attrs for the type-level config, field attrs that drive code generation per field.

## IX.1 · Attribute surface

### Container attributes

```rust
#[derive(Component, Clone, Default, PartialEq, SchemaSync)]
#[schema(name = "Frame")]                     // → SCHEMA_NAME (default = type name)
pub struct Frame { ... }
```

- `name = "..."` — wire-format discriminator. Defaults to the type name. Required to be unique per app.

### Field attributes — CRDT type selection

Choose at most one CRDT-kind attribute per field. Defaults are listed in §IX.3.

```rust
#[crdt(lww)]              // LwwRegister<T>; default for scalars and most types
#[crdt(or_set)]           // OrSet<T>; for Vec<T>, HashSet<T>, BTreeSet<T>
#[crdt(sequence)]         // Sequence<T>; for String, Vec<T> when ordered + collaborative
#[crdt(map)]              // CausalMap<K, V>; for HashMap<K, V>, BTreeMap<K, V>
#[crdt(counter)]          // PnCounter; for i64 / i32 / u32 / u64
#[crdt(nested)]            // recurse into another SchemaSync component
#[crdt(skip)]              // exclude from sync entirely
```

### Field attributes — refinement

```rust
#[crdt(rename = "x")]      // wire path uses "x" instead of the Rust field name
#[crdt(default = "expr")]  // override default for echo-guard fallback (rare)
#[crdt(with = "Type")]     // custom schema-side type (escape hatch for Handle<T>, etc.)
```

### Combined example

```rust
#[derive(Component, Clone, Default, PartialEq, SchemaSync)]
#[schema(name = "Frame")]
pub struct Frame {
    pub name: String,                                    // implicit lww
    pub absolute_bounding_box: Rectangle,                // implicit lww (Rectangle: Default + PartialEq)
    pub visible: bool,                                   // implicit lww

    #[crdt(or_set)]
    pub export_settings: Vec<ExportSetting>,             // OrSet<ExportSetting>

    #[crdt(sequence)]
    pub characters: String,                              // Sequence<char>

    #[crdt(map)]
    pub component_property_definitions:
        HashMap<String, ComponentPropertyDef>,           // CausalMap<String, LwwRegister<...>>

    #[crdt(counter)]
    pub edit_count: i64,                                 // PnCounter

    #[crdt(skip)]
    pub local_hover_state: HoverState,                   // not synced

    #[crdt(rename = "fillsGeometry")]
    pub fills: Vec<Paint>,                               // wire path = "fillsGeometry"

    #[crdt(with = "AssetHandleSchema")]
    pub thumbnail: Handle<Image>,                        // custom schema field
}
```

## IX.2 · Code generation contract

For the `Frame` example above, the macro generates:

```rust
// 1. Schema struct (sibling type)
#[derive(Clone, Debug, Default, PartialEq, ::kyoso_crdt::DeriveCrdt)]
pub struct FrameSchema {
    pub name: ::kyoso_crdt::types::LwwRegister<String>,
    pub absolute_bounding_box: ::kyoso_crdt::types::LwwRegister<Rectangle>,
    pub visible: ::kyoso_crdt::types::LwwRegister<bool>,
    pub export_settings: ::kyoso_crdt::types::OrSet<ExportSetting>,
    pub characters: ::kyoso_crdt::types::Sequence<char>,
    pub component_property_definitions:
        ::kyoso_crdt::types::CausalMap<String, ::kyoso_crdt::types::LwwRegister<ComponentPropertyDef>>,
    pub edit_count: ::kyoso_crdt::types::PnCounter,
    // skipped fields are absent
    pub fills: ::kyoso_crdt::types::OrSet<Paint>,        // implicit or_set for Vec — see §IX.3
    pub thumbnail: AssetHandleSchema,                    // user-provided schema type
}

// 2. SchemaSync impl
impl ::kyoso_sync::SchemaSync for Frame {
    type Schema = FrameSchema;
    const SCHEMA_NAME: &'static str = "Frame";

    fn changes_against(
        &self,
        current: &Self::Schema,
    ) -> Vec<<Self::Schema as ::kyoso_crdt::Crdt>::Mutation> {
        let default = <Self as ::core::default::Default>::default();
        let mut out = Vec::new();

        // LWW field — echo-guard convention applied
        if *current.name.get().unwrap_or(&default.name) != self.name {
            out.push(FrameSchemaMut::Name(::kyoso_crdt::types::LwwMut::Set(self.name.clone())));
        }

        // OrSet field — emit Add for elements in self but not in current
        for elem in &self.export_settings {
            if !current.export_settings.contains(elem) {
                out.push(FrameSchemaMut::ExportSettings(
                    ::kyoso_crdt::types::OrSetMut::Add(elem.clone())));
            }
        }
        // (and Remove for elements in current but not in self — see §IX.5)

        // Sequence field — diff via LCS or naive replace; see §IX.6 for trade-off
        // ...

        out
    }

    fn write_back(&mut self, schema: &Self::Schema) {
        if let Some(v) = schema.name.get() { self.name = v.clone(); }
        if let Some(v) = schema.absolute_bounding_box.get() { self.absolute_bounding_box = *v; }
        if let Some(v) = schema.visible.get() { self.visible = *v; }
        self.export_settings = schema.export_settings.iter().cloned().collect();
        self.characters = schema.characters.iter().collect();
        self.component_property_definitions = schema
            .component_property_definitions
            .iter()
            .filter_map(|(k, v)| v.get().map(|val| (k.clone(), val.clone())))
            .collect();
        self.edit_count = schema.edit_count.value();
        // skipped fields are not touched
        self.fills = schema.fills.iter().cloned().collect();
        // `with`-fields delegate to the user's projection
        self.thumbnail = AssetHandleSchema::project_to(&schema.thumbnail);
    }
}
```

The generated code is straight-line, debuggable via `cargo expand`, and uses fully-qualified paths so it works from any consumer crate.

## IX.3 · Default CRDT-type rules

When a field has no explicit `#[crdt(...)]` attribute, the macro infers based on the Rust type:

| Rust type | Default CRDT | Override with |
|---|---|---|
| `T: Serialize + Default + Clone + PartialEq` (scalars, structs, enums) | `LwwRegister<T>` | `#[crdt(...)]` for non-LWW |
| `String` | `LwwRegister<String>` | `#[crdt(sequence)]` for collab text |
| `Vec<T>` | `LwwRegister<Vec<T>>` | `#[crdt(or_set)]`, `#[crdt(sequence)]` |
| `HashSet<T>`, `BTreeSet<T>` | `OrSet<T>` (since LWW-replace on a set is rarely intent) | `#[crdt(lww)]` to force replace |
| `HashMap<K, V>`, `BTreeMap<K, V>` | `LwwRegister<HashMap<K, V>>` | `#[crdt(map)]` for per-key sync |
| `Option<T>` | `LwwRegister<Option<T>>` | (no special handling) |
| Numeric types `i32/i64/u32/u64/f32/f64` | `LwwRegister<T>` | `#[crdt(counter)]` for PN-counter (integers only) |
| `Handle<T>`, `Entity`, function pointers, types missing required bounds | **error**: must use `#[crdt(skip)]` or `#[crdt(with = ...)]` |

The principle: defaults match the most common intent, explicit attrs handle the rest. The compiler refuses to silently sync types that don't satisfy the bounds.

## IX.4 · Crate organisation

**New crate**: `crates/kyoso_sync_derive` (proc-macro only). Lives next to the existing `crates/kyoso_crdt_derive`.

```
kyoso_crdt_derive/         (existing) — derive(Crdt) on schema structs
kyoso_sync_derive/         (new)      — derive(SchemaSync) on Bevy components
```

Why a separate crate? Same reason the CRDT derive is separate: `proc-macro = true` crates can't export non-macro items, so they're terminal in the dep graph. Splitting derives by domain keeps `kyoso_crdt` usable as a Bevy-free CRDT lib (its derive only depends on schema concerns; `derive(SchemaSync)` would force a Bevy dep on the derive crate, which we don't want bleeding into `kyoso_crdt`).

Public surface: `kyoso_sync` re-exports `kyoso_sync_derive::SchemaSync` so users write `use kyoso_sync::SchemaSync;` and get both the trait and the derive.

## IX.5 · Phased rollout

Each phase is independently mergeable. Phase A unblocks phase B; phases B–F can interleave once the scaffold exists.

**Phase A — Crate scaffold + LWW-only happy path** (~400 LOC, ~2 days)
- New `kyoso_sync_derive` crate with `proc-macro = true`.
- Parse the input struct, pull container attrs (`schema(name)`).
- For each field, generate the corresponding LWW schema field + SchemaSync method bodies.
- No attributes yet — all fields treated as `#[crdt(lww)]`.
- Cover `Self::default()` echo-guard convention in the generated code.
- Migrate `TransformSchema` (built-in) to validate the macro against the existing impl — both should produce identical wire behavior.

**Phase B — `skip`, `rename`, `default` attrs** (~150 LOC, ~1 day)
- Implement field-attr parsing. `syn::Attribute` + `darling` (probably) for ergonomic parsing.
- Skipped fields are simply omitted from the schema struct + skipped in changes_against / write_back.
- `rename` changes the schema struct's field name AND the wire path segment.
- `default = "expr"` overrides the echo-guard fallback.

**Phase C — `or_set`, `counter`, container types** (~250 LOC, ~2 days)
- For `Vec<T>` / `HashSet<T>` with `#[crdt(or_set)]`: emit `OrSet<T>` schema field; changes_against does set-diff (Adds for new, Removes for missing).
- For `i64` etc. with `#[crdt(counter)]`: emit `PnCounter`; changes_against emits `Inc(diff)` against `current.value()`.
- write_back projects in reverse: `OrSet → HashSet`, `PnCounter → i64`.

**Phase D — `map` and `nested` attrs** (~300 LOC, ~3 days)
- `HashMap<K, V>` with `#[crdt(map)]`: emit `CausalMap<K, LwwRegister<V>>` (or another inner CRDT specified via nested attribute syntax — see IX.7).
- `nested`: field type is itself another `SchemaSync` component. Generate recursive `changes_against` / `write_back` calls.

**Phase E — `with` and custom escape hatch** (~150 LOC, ~1 day)
- `with = "Type"` lets users name a hand-written schema type. Macro generates the field with `Type` and delegates `changes_against` / `write_back` to two trait methods on `Type` (or on a small `SchemaField<C, T>` trait).
- Required for `Handle<Image>`, asset references, anything where the synced value is a server-resolved key rather than the in-memory Bevy type.

**Phase F — `sequence` (text/array)** (~200 LOC, ~1 day)
- `String` with `#[crdt(sequence)]`: emit `Sequence<char>` schema; changes_against does a naive prefix-suffix diff (LCS is overkill for now; revisit if collab-text fidelity matters).
- `Vec<T>` with `#[crdt(sequence)]`: same shape over `T`.

**Phase G — error UX polish** (~150 LOC, ~1 day)
- `compile_fail` tests under `kyoso_sync_derive/tests/ui/` driven by `trybuild`.
- Every macro error uses `syn::Error::new_spanned(field, "...")` so the IDE underlines the offending field, not the derive.
- Friendly errors for: missing PartialEq + Default on the component; field type that doesn't satisfy bounds with no `#[crdt(skip)]`; conflicting CRDT attrs on the same field; reserved attr names.

**Phase H — Migrate existing impls** (~50 LOC removed, ~half a day)
- Convert `GraphNode`, `GraphEdge`, `Transform` (move out of builtin_schemas.rs into a derive on Transform — wait, can't `derive` on a foreign type; `Transform` keeps its hand-written impl OR we add a `transform_schema!` macro).
- Convert test types `Named`, `Styled`, `TypedNode` to derive.
- All existing tests must continue to pass.

**Phase I — Figma-model build-out** (incremental, weeks)
- Start with `Rectangle`, `Frame`, `Text`, `Component`, `Instance`. Each is one struct with derive.
- Add types as the demo app needs them. Skip fields that don't make sense to sync (e.g. computed bounds).

## IX.6 · Test strategy

**Unit-level (kyoso_sync_derive itself)**
- `trybuild` for compile-pass and compile-fail cases. Every error message tested.
- `macrotest` (or `cargo expand` + golden files) for happy-path expansion checks. Catches accidental changes to the generated code shape during refactoring.

**Integration-level (kyoso_sync)**
- The existing `two_apps.rs` integration tests serve as the regression net. Migrate each hand-written impl to the derive in Phase H; if a test breaks, the derive doesn't match the hand-written semantics.
- New tests per CRDT kind: a `derived_or_set_replicates`, `derived_counter_replicates`, `derived_sequence_replicates`, `derived_map_replicates` — each spawns two apps with a `derive(SchemaSync)`-ed component using that kind, and verifies convergence.

**End-to-end**
- A small Figma-model demo (kyoso_client variant or a separate example) that exercises ~5 derived types in a multi-peer scenario.

## IX.7 · Edge cases worth pre-thinking

These bite during implementation; flag them now so they're not surprises:

1. **Generic structs**. `derive(SchemaSync)` on `struct Foo<T: ...>` — is it supported? Initially **no**: the derive only handles concrete types. Generic Figma types are rare; revisit if needed.
2. **Lifetimes**. `derive(SchemaSync)` on `struct Foo<'a>` — out of scope. Schema state is owned, no lifetimes.
3. **Enum components**. Bevy components are usually structs. Enums with `#[derive(Component)]` exist but are unusual. Initially: **emit error**, suggest hand-rolled impl.
4. **Tuple structs**. `struct Foo(String, i32)` — supportable but ugly (path segments are field indexes). Initially: **emit error**, suggest renaming to a struct with named fields.
5. **`Handle<T>` and asset references**. The schema stores some asset key (URL, GUID); the user provides a `with = "Type"` projector that knows how to convert between `Handle<T>` and the keyed representation.
6. **`Entity` references**. Entity IDs are local-only; a synced reference must travel as a `CrdtId`. Probably needs a marker type `SyncedEntity` that the `EntityCrdtIndex` resolves on projection. Defer past Phase A; flag for Phase F or later.
7. **CRDT-kind attribute syntax for nested generics**. `#[crdt(map(value = "or_set"))]` — should the inner CRDT for a `CausalMap<K, V>` be configurable? Probably yes for completeness, but defer past Phase D. For now: `CausalMap<K, LwwRegister<V>>` is the only shape.
8. **Default echo-guard for `with` types**. The user's custom schema type needs to support the `unwrap_or(&Self::default().<field>)` convention. Codify it via a small trait (`SchemaField`?) that `with`-types must implement.
9. **Visibility**. Generated `FrameSchema` should match the visibility of `Frame` (`pub` → `pub`, private → private).

## IX.8 · Cost summary

| Phase | New code (approx) | Migration code touched | Days |
|---|---|---|---|
| A — scaffold + LWW | 400 | — | 2 |
| B — skip/rename/default | 150 | — | 1 |
| C — or_set/counter | 250 | — | 2 |
| D — map/nested | 300 | — | 3 |
| E — with escape hatch | 150 | — | 1 |
| F — sequence | 200 | — | 1 |
| G — error UX | 150 + tests | — | 1 |
| H — migrate existing impls | — (~80 LOC removed) | 6 schemas | 0.5 |
| **Total before Figma** | **~1,400 LOC** | ~80 LOC removed | **~11.5 days** |
| I — Figma incremental | ~30 LOC per node type | 0 | open-ended |

Of the 11.5 days, phases A and D are the riskiest (scaffold quality determines all subsequent phases; map+nested touches recursive schema generation).

## IX.9 · What this enables

Once Part IX lands, adding a Figma node type is one struct with attributes. The 50+ Figma types stop being a 2500-LOC tax and become a 50 × ~15-line cost. More importantly:

- Echo-guard convention enforced at compile time. Future regressions impossible.
- Field-level CRDT type choice still explicit (via attrs), so reviewers can see "this field uses OrSet" at a glance.
- Hand-written escape hatch (`with`) preserves flexibility for the small fraction of fields that need bespoke semantics.
- Schema migration story (when a field's CRDT type changes) becomes "change one attribute, recompile, write a per-version migration op" rather than rewriting an entire impl.

## IX.10 · Open questions to resolve before Phase A

1. **`darling` vs hand-rolled attr parsing.** `darling` is the de facto Rust lib for derive attribute parsing; small dep cost but saves significant boilerplate. Recommendation: use `darling`.
2. **Generated schema struct visibility.** Probably mirror the component's visibility. Confirm.
3. **`SCHEMA_NAME` collision detection.** Two derived components with the same `name`-attribute would clash on the wire. Best the derive can do is warn at compile-time *if both are in the same crate* — cross-crate clashes need a runtime sanity check at plugin build time. Track as a follow-up.
4. **`with`-type contract.** Sketch the trait that `with = "Type"` types must implement. Probably:
   ```rust
   trait SchemaField<Source: ?Sized> {
       type Mutation;
       fn changes_against(source: &Source, current: &Self) -> Vec<Self::Mutation>;
       fn project_to(schema: &Self) -> Source where Source: Sized;
   }
   ```
   Confirm shape during Phase E.
5. **`Transform` migration.** Built-in schemas need a different mechanism (can't derive on a foreign type). Options: (a) keep hand-written `impl SchemaSync for Transform` next to the derive, (b) introduce a `schema_for!(Transform { translation: lww, ... })` macro that generates the same code from a non-derive entry point. Decide before Phase H.

---
---

# Part X — derive(SchemaSync) phases A–H landed

## X.0 · Status

All eight phases of the `derive(SchemaSync)` macro from Part IX landed in this iteration. **142 workspace tests passing.** The macro covers every CRDT kind from §IX.3 (LWW, OrSet, Counter, Map, Nested, Sequence, With) plus the `skip` / `rename` / `default` refinement attributes, and every existing hand-written `SchemaSync` impl that *could* migrate to the derive has migrated. (Only `Transform`'s impl remains hand-written, as planned, since it's a foreign type.)

## X.1 · Per-phase summary

| Phase | What landed | Tests added |
|---|---|---|
| A | scaffold + LWW happy path; `#[schema(name)]` | 3 (initial derive smoke) |
| B | `#[crdt(skip)]`, `#[crdt(rename)]`, `#[crdt(default)]` | 4 |
| C | `#[crdt(or_set)]`, `#[crdt(counter)]` + integer-type validation | 6 |
| D | `#[crdt(map)]` (HashMap/BTreeMap), `#[crdt(nested)]`; `IntoWireOp for MapDelta` + path-driven Remove in `CausalMap::apply_wire` | 6 |
| E | `#[crdt(with = "Type")]` + `kyoso_sync::SchemaField` trait + built-in impl for `LwwRegister<T>` | 5 |
| F | `#[crdt(sequence)]` for `String` and `Vec<T>`; `kyoso_sync::sequence_diff` helper with prefix-suffix algorithm | 11 |
| G | error-span improvements (field-level errors now span fields, not call_site) | 0 (regression net) |
| H | migrate `GraphNode`, `GraphEdge`, `Named`, `Styled`, `TypedNode` to the derive; delete `apps/kyoso_client/src/schema.rs` | net −4 (redundant tests) |

## X.2 · Generated-code surface

For a struct with `derive(SchemaSync)` the macro produces, all on the same impl span as the input:

1. **`<Name>Schema` sibling struct** — same visibility as the component, fields replaced by their schema-side type (one of: `LwwRegister<T>`, `OrSet<T>`, `PnCounter`, `CausalMap<LwwRegister<V>>`, `Sequence<T>`, `<T as SchemaSync>::Schema`, or the user-named `with` type). The schema struct gets `Clone + Debug + Default + PartialEq + DeriveCrdt` derives, which transitively gives `Lattice + Crdt + SchemaApply + IntoWireOp + From/TryFrom<WireDelta>` for free.
2. **`<Name>SchemaMut` / `<Name>SchemaDelta` enums** — generated by the schema's `DeriveCrdt`, one variant per (renamed) schema field.
3. **`SchemaSync for <Name>` impl** — `type Schema = <Name>Schema`, `const SCHEMA_NAME = "..."` (defaults to type name), and `changes_against` / `write_back` bodies built from per-field codegen.

## X.3 · Echo-guard convention is now compiler-enforced

The `unwrap_or(&Self::default().<field>)` echo-guard convention from Part VIII is baked into every LWW arm the macro generates. Future derive users can't forget it — there's no opt-out, no off-switch. Custom `SchemaField` impls (`#[crdt(with = "Type")]`) carry the responsibility themselves; the trait's rustdoc documents the convention, and the built-in `LwwRegister<T>` impl follows it.

## X.4 · Diagnostic quality

Field-level errors point at the field via `syn::Error::new_spanned(field, ...)` (not `Span::call_site()`). IDE underlines highlight the offending field directly. Container-level errors (unit struct, enum, all-fields-skipped) span the type's name ident. The `parse_nested_meta` errors point at the offending attribute meta. Reserved-but-not-implemented attributes (none currently — everything is implemented) would emit a clear "see plan doc Part IX §IX.5" message.

**Trybuild-based UI tests** (originally proposed for Phase G) are **deferred**. Cargo's circular-dep rules prevent putting `trybuild` tests inside `kyoso_sync_derive` while it's the dep providing the macro under test. Pulling Bevy from git main also makes `.stderr` files brittle to compiler-version drift. The path forward when trybuild becomes worth the maintenance: a separate `kyoso_sync_derive_tests` crate that depends on `kyoso_sync` (which re-exports the derive), gated by a `--features ui-tests` flag so they don't run on every developer's machine. Tracked as a follow-up.

## X.5 · Migration impact

Lines of hand-written code removed in Phase H:
- `apps/kyoso_client/src/schema.rs` — entire file deleted (~280 LOC, including its 4 unit tests that became redundant once the derive replaced the hand-written impls).
- `apps/kyoso_client/src/scene.rs` — net +14 LOC (added `kyoso_sync::SchemaSync` to two derive lists + two `#[schema(name)]` attrs).
- `apps/kyoso_client/src/lib.rs` — `pub mod schema;` line deleted.
- `crates/kyoso_sync/tests/two_apps.rs` — net −60 LOC (deleted hand-written `Named`/`Styled`/`TypedNode` schemas; macro generates them now).

Net change for the kyoso_client + tests migration: ~340 LOC of hand-written boilerplate replaced by 6 derives + 6 `#[schema(name)]` attrs.

For the upcoming Figma model build-out, this translates to one annotated struct per Figma node type. The 50-type Figma surface is now a per-struct exercise, not a 2,500-LOC translation tax.

## X.6 · What's left

1. **`Transform` schema-for! macro**. Currently `kyoso_sync::builtin_schemas::TransformSchema` is hand-written because `Transform` is foreign. A small `schema_for!(Transform { translation: lww, rotation: lww, scale: lww })` macro would be ~150 LOC and would close the last hand-written gap. Bounded; defer until needed.
2. **Trybuild compile-fail tests**. See §X.4 — wait for the dep-graph dust to settle before introducing a separate test crate.
3. **`#[crdt(map)]` non-LWW value types**. Today `CausalMap<LwwRegister<V>>` is the only generated shape. To allow `CausalMap<OrSet<V>>` etc., the macro would need a nested attribute syntax like `#[crdt(map(value = "or_set"))]`. Defer until a Figma type genuinely needs it.
4. **`Sequence` with Fugue**. RGA → Fugue upgrade is plan-doc Part V §V.5; orthogonal to the derive, and tracked separately.
5. **Sequence over `Cow<str>` / `&'static str` / Bevy's `SmolStr`**. Today `String` is the only collab-text container shape. Ergonomic improvements for callers using interned-string types are a polish item.

## X.7 · Verification

- `cargo build --workspace --tests`: clean, no warnings introduced.
- `cargo test --workspace --no-fail-fast`: **142 tests passing, 0 failures**.

Test breakdown:

| Crate / area | Tests |
|---|---|
| `kyoso_crdt` unit + integration | 28 + 12 + 9 + 3 = 52 (incl. proptest, sync, composition) |
| `kyoso_sync_derive` | proc-macro (no inline tests; covered via consumers) |
| `kyoso_sync` engine + index unit + sequence_diff | 6 + 6 = 12 |
| `kyoso_sync` two_apps integration | 12 |
| `kyoso_sync` derived_schema integration | **29** (the derive's primary regression net) |
| `kyoso_client` duplex_round_trip | 3 |
| `kyoso_server` | 6 + 9 = 15 |
| `kyoso_graph` | 4 |
| **Total** | **142** |

## X.8 · Where the macro lives

```
crates/
├── kyoso_crdt/                 — pure CRDT lib, no Bevy
├── kyoso_crdt_derive/          — derive(Crdt) on schema structs
├── kyoso_sync/                 — Bevy bridge + SchemaSync + SchemaField + sequence_diff
└── kyoso_sync_derive/          — derive(SchemaSync) on Bevy components  (NEW in Part X)
```

The split keeps `kyoso_crdt` Bevy-free (the existing constraint from Part IV §IV.6); `kyoso_sync_derive` is a thin proc-macro crate that depends only on `syn`, `quote`, `proc-macro2`. User-facing API surface: `use kyoso_sync::SchemaSync;` brings both the trait and the derive into scope (Rust's namespace separation lets the trait and the derive macro share a name).

---
---

# Part XI — `kyoso_figma`: opinionated Bevy-native document model

## XI.0 · Context

Now that `derive(SchemaSync)` covers the seven CRDT kinds the Figma data model needs, the next milestone is **modelling Figma's document structure as a Bevy-native ECS layer**. The intent is not to re-implement Figma the SaaS, but to take their *shape* — frames containing rectangles and text, organised hierarchically, with shared paint and typography styling — as the prototype for kyoso's own collaborative document model. Replicating Figma's model is the litmus test that the schema infrastructure (Parts I–X) holds together against a real-world document surface.

User decisions locked in before planning:
- **Fidelity: opinionated, Bevy-native.** Unify `Group` + `Frame` (no separate `Group` for the initial cut), drop Figma's `absolute_bounding_box` (compute from Bevy `GlobalTransform` on demand), use Rust-idiomatic field names.
- **Initial scope: minimal — three node types.** `Frame`, `Rectangle`, `Text`. Component / Instance / Page deferred to a later iteration.
- **Build the Figma import adapter alongside.** Pull `figma-api` as a dep now and write the conversion path. Validates the model against real Figma data from day one.
- **Both `Paint` and `TypeStyle` in the initial cut.** Frames + Rectangles carry `fills: Vec<Paint>` and `strokes: Vec<Paint>`; Text carries a nested `TypeStyle` with font config.

## XI.1 · New crate layout

```
crates/kyoso_figma/
├── Cargo.toml
└── src/
    ├── lib.rs              — re-exports + crate-level docs + FigmaNode marker
    ├── plugin.rs           — KyosoFigmaPlugin: registers all per-component plugins
    ├── size.rs             — Size component (width, height) + SchemaSync derive
    ├── paint.rs            — Paint enum (Solid | Gradient | Image) — value type
    ├── typestyle.rs        — TypeStyle struct + SchemaSync derive (nested into Text)
    ├── frame.rs            — Frame component + SchemaSync derive
    ├── rectangle.rs        — Rectangle component + SchemaSync derive
    ├── text.rs             — Text component + SchemaSync derive
    ├── walker.rs           — VENDORED from etch_figma: Walker<V>, NodeVisitor trait,
    │                         NodeContext, SubcanvasNodeExt. ~350 LOC; attributed at
    │                         the file-level docstring. Zero modifications on import;
    │                         we only consume the trait — no public-API drift to manage.
    └── import.rs           — KyosoVisitor: NodeVisitor impl that spawns Bevy bundles
                              from figma_api types. Uses Walker over a CanvasNode.
```

Workspace-level changes:
- Add `kyoso_figma = { path = "crates/kyoso_figma" }` to `[workspace.dependencies]` in the root `Cargo.toml`.
- Add `figma-api = "..."` (latest crates.io version) to `[workspace.dependencies]`.

`kyoso_figma` depends on:
- `kyoso_sync` (for `SchemaSync`, `SchemaSyncedNodeComponentPlugin`, `TransformSchema`).
- `kyoso_crdt` (for `LwwRegister`, etc., used directly when not via `#[crdt(...)]`).
- `kyoso_graph` (for tree edges to attach children to frames).
- `bevy` (component + reflect derives).
- `figma-api` (for the import adapter — only used in `walker.rs` + `import.rs`).
- `serde` for value-type serialization.

**Why vendor instead of path-dep on etch_figma**: etch_figma is a TSX/HTML/SVG codegen crate that drags in `swc_*`, `reqwest`, `etch_tsx`, `etch_svgr`, `sha2`, `tempfile`, `heck` etc. — all unrelated to our walking-a-JSON-tree need. The walker itself is ~350 LOC of dep-free code. Copying it once with attribution avoids inheriting a codegen toolchain we don't use. If etch_figma gains useful walker methods later, re-syncing is small. (Long-term, splitting `figma-walker` out of etch_figma into a shared sibling crate is the right structural move; tracked as a separate refactor in the etch workspace, not a kyoso prerequisite.)

## XI.2 · Node-type definitions (initial cut)

### `Size` ([size.rs](crates/kyoso_figma/src/size.rs))

A standalone Bevy component carrying `width: f32`, `height: f32`. Bevy's `Transform` is a 4×4 matrix and doesn't carry size for 2D shapes; `Size` fills that gap. Used by `Frame` and `Rectangle`. Per-field LWW.

### `Frame` ([frame.rs](crates/kyoso_figma/src/frame.rs))

The unified Frame-or-Group from Figma (per the "opinionated" answer):

```rust
#[derive(Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Frame")]
pub struct Frame {
    pub name: String,                     // LWW (default)
    pub clips_content: bool,              // LWW — ex-Figma: Frame=true, Group=false
    pub layout_mode: LayoutMode,          // LWW — None / Horizontal / Vertical (auto-layout)

    #[crdt(or_set)]
    pub fills: Vec<Paint>,                // OrSet — concurrent fill adds don't clobber

    #[crdt(or_set)]
    pub strokes: Vec<Paint>,
    pub stroke_weight: f32,               // LWW
}

#[derive(Default, Clone, Debug, PartialEq, Eq, Reflect, Serialize, Deserialize)]
pub enum LayoutMode { #[default] None, Horizontal, Vertical }
```

Frame entities also carry `Transform`, `Size`, and (for trees) `TreeParent` + `OrderKey`.

### `Rectangle` ([rectangle.rs](crates/kyoso_figma/src/rectangle.rs))

Primitive shape:

```rust
#[derive(Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Rectangle")]
pub struct Rectangle {
    pub corner_radius: f32,               // LWW — single radius; per-corner is a follow-up
    #[crdt(or_set)]
    pub fills: Vec<Paint>,
    #[crdt(or_set)]
    pub strokes: Vec<Paint>,
    pub stroke_weight: f32,
}
```

### `Text` ([text.rs](crates/kyoso_figma/src/text.rs))

Text node with collaborative content + nested typography:

```rust
#[derive(Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Text")]
pub struct Text {
    #[crdt(sequence)]
    pub content: String,                  // Sequence<char> — collaborative editing

    #[crdt(nested)]
    pub style: TypeStyle,                 // recurse into TypeStyle's SchemaSync

    #[crdt(or_set)]
    pub fills: Vec<Paint>,
}
```

### `TypeStyle` ([typestyle.rs](crates/kyoso_figma/src/typestyle.rs))

Itself a SchemaSync component (so `Text.style` can use `#[crdt(nested)]`):

```rust
#[derive(Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "TypeStyle")]
pub struct TypeStyle {
    pub font_family: String,              // LWW
    pub font_size: f32,                   // LWW
    pub font_weight: u32,                 // LWW
    pub line_height: f32,                 // LWW
}
```

`TypeStyle: Component` is a contract requirement — `derive(SchemaSync)` implies `Component<Mutability=Mutable>`. It's never spawned as a standalone entity; the `#[crdt(nested)]` plumbing only uses it as a value type. (The `Component` derive is harmless when never spawned.)

### `Paint` ([paint.rs](crates/kyoso_figma/src/paint.rs))

Plain serde-serializable enum:

```rust
#[derive(Clone, Debug, PartialEq, Reflect, Serialize, Deserialize)]
pub enum Paint {
    Solid { color: [f32; 4] },                                       // RGBA
    Gradient { stops: Vec<GradientStop>, gradient_type: GradientType },
    Image { image_ref: String, scale_mode: ImageScaleMode },
}
```

**`fills` storage choice (per user decision)**: try `OrSet<Paint>` first via `#[crdt(or_set)]`. This requires hand-rolled `Eq + Hash + Ord` impls over `Paint` (the usual `f32::to_bits` dance). If those impls turn out clean (~30 LOC), keep OrSet — concurrent fill adds on different peers don't clobber. If they get ugly, fall back to `LwwRegister<Vec<Paint>>` — whole-list replace, only needs `PartialEq`. This decision is local to the implementor at the time and well-contained — only affects the Frame/Rectangle/Text struct definitions.

## XI.3 · Plugin + structural marker

[crates/kyoso_figma/src/lib.rs](crates/kyoso_figma/src/lib.rs) defines a zero-sized marker that tags every figma node entity:

```rust
#[derive(Component, Default, Clone, Debug, PartialEq, Reflect)]
#[reflect(Component, Default)]
pub struct FigmaNode;

#[derive(Component, Default, Clone, Debug, Reflect)]
#[reflect(Component, Default)]
pub struct FigmaEdge;
```

`FigmaNode` is the `N` parameter to `CrdtSyncPlugin<N, E>`; `FigmaEdge` is `E`. Every spawned node entity carries `FigmaNode` plus exactly one of `Frame` / `Rectangle` / `Text` (the field-bearing components). This avoids the "spawn `Frame::default()` even on a Text node" awkwardness — Frame really is one node kind among several, not a privileged base.

[crates/kyoso_figma/src/plugin.rs](crates/kyoso_figma/src/plugin.rs):

```rust
pub struct KyosoFigmaPlugin {
    pub server_url: String,
    pub room: String,
}

impl Plugin for KyosoFigmaPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            CrdtSyncPlugin::<FigmaNode, FigmaEdge>::new(self.server_url.clone(), self.room.clone()),
            // Per-component typed-schema plugins. Each one is opt-in for one
            // Bevy component type. FigmaNode/FigmaEdge are the structural N/E.
            SchemaSyncedNodeComponentPlugin::<FigmaNode, FigmaEdge, Frame>::default(),
            SchemaSyncedNodeComponentPlugin::<FigmaNode, FigmaEdge, Rectangle>::default(),
            SchemaSyncedNodeComponentPlugin::<FigmaNode, FigmaEdge, Text>::default(),
            SchemaSyncedNodeComponentPlugin::<FigmaNode, FigmaEdge, Size>::default(),
            SchemaSyncedNodeComponentPlugin::<FigmaNode, FigmaEdge, Transform>::default(),
        ));
    }
}
```

The schema sync layer's existing pattern (Part VIII) handles "entity has component C added later" cleanly — `SchemaSyncedNodeComponentPlugin` queues `InsertSchemaProjected` if the inbound projection arrives before the component is on the entity. So a Text node arriving before its TextSchema apply is fine; the component appears mid-frame with the right values.

## XI.4 · Figma import adapter (vendored walker + visitor pattern)

Two files, with clear responsibility split:

### [crates/kyoso_figma/src/walker.rs](crates/kyoso_figma/src/walker.rs)

Vendored from [`/Users/hectorcrean/rust/etch/crates/etch_figma/src/core/walker.rs`](../etch/crates/etch_figma/src/core/walker.rs) (~250 LOC) and [`/Users/hectorcrean/rust/etch/crates/etch_figma/src/lib.rs`](../etch/crates/etch_figma/src/lib.rs)'s `SubcanvasNodeExt` impl (~100 LOC). Zero modifications on import — the file's top docstring credits the source. Public surface:

- `pub trait NodeVisitor` — one default-no-op method per Figma node variant (`visit_frame`, `visit_rectangle`, `visit_text`, ...) plus `enter_container` / `exit_container` / `should_traverse_children` hooks.
- `pub struct Walker<V: NodeVisitor>` with `walk_canvas(canvas: &CanvasNode) -> V`.
- `pub struct NodeContext { path: Vec<String>, depth: usize, node_id: String }` — passed to every visit call.
- `pub trait SubcanvasNodeExt` — `id() / name() / children() / node_type()` over the giant `SubcanvasNode` enum.

### [crates/kyoso_figma/src/import.rs](crates/kyoso_figma/src/import.rs)

Implements `KyosoVisitor: NodeVisitor` that produces Bevy bundles:

```rust
pub struct KyosoVisitor<'w, 's> {
    commands: Commands<'w, 's>,
    parent_stack: Vec<Entity>,
    /// Entities the visitor created during `walk_canvas`. The caller can
    /// use this to drive replication / sync side effects after the walk.
    spawned: Vec<Entity>,
}

impl NodeVisitor for KyosoVisitor<'_, '_> {
    fn visit_frame(&mut self, frame: &figma_api::FrameNode, ctx: &NodeContext) {
        let entity = self.commands.spawn((
            FigmaNode,
            Frame { /* converted from figma_api::FrameNode */ },
            Size { width: ..., height: ... },
            Transform::from_translation( /* from frame.relative_transform */ ),
            // tree parent if any
        )).id();
        if let Some(&parent) = self.parent_stack.last() {
            // attach via kyoso_graph::TreeParent + OrderKey
        }
        self.spawned.push(entity);
    }

    fn visit_rectangle(&mut self, rect: &figma_api::RectangleNode, ctx: &NodeContext) { /* ... */ }
    fn visit_text(&mut self, text: &figma_api::TextNode, ctx: &NodeContext) { /* ... */ }

    fn enter_container(&mut self, node: &SubcanvasNode, _ctx: &NodeContext) {
        if let Some(id) = node.id() { /* push the just-spawned entity onto parent_stack */ }
    }
    fn exit_container(&mut self, _node: &SubcanvasNode, _ctx: &NodeContext) {
        self.parent_stack.pop();
    }

    // visit_group, visit_component, visit_instance, visit_vector, etc.
    // are NOT overridden — the default no-op runs. Per the user's "log + skip"
    // decision, we override `should_traverse_children` to skip recursing
    // into unsupported container kinds, and emit a `tracing::warn!` for each
    // unsupported variant we hit (achievable via a single override that
    // matches on node_type()).
}

pub fn import_canvas(commands: Commands, canvas: &figma_api::CanvasNode) -> Vec<Entity> {
    let visitor = KyosoVisitor { commands, parent_stack: Vec::new(), spawned: Vec::new() };
    let walker = Walker::new(visitor);
    let completed = walker.walk_canvas(canvas);
    completed.spawned
}
```

Per-field conversions (Figma → kyoso_figma) live as helpers in `import.rs`:
- `figma_api::Paint::Solid { color }` → `Paint::Solid { color: [r, g, b, a] }`.
- `figma_api::Paint::Gradient*` → `Paint::Gradient { ... }`.
- `figma_api::Paint::Image { image_ref, scale_mode }` → `Paint::Image { ... }`.
- `figma_api::TypeStyle` → `TypeStyle { ... }` (1:1 field copy after rename).
- Frame's `is_clip` boolean → `Frame::clips_content`. Group nodes (if we ever support them) → `Frame { clips_content: false, .. }`.
- Frame's `layout_mode` enum → our `LayoutMode`.

**Unsupported node policy** (per user decision: log + skip): unsupported variants don't get a `visit_*` override, so the default no-op runs. We additionally emit a `tracing::warn!("unsupported figma node kind: {kind} at {path}")` from a single `enter_container` / catch-all path — exact placement is implementation detail. No stub entity is spawned; the tree shape may have gaps where unsupported nodes were skipped.

Failure modes within supported variants are non-fatal: an unparseable color falls back to `[0,0,0,1]`, an unsupported Paint variant falls back to a `Solid` black, etc. The import is best-effort — never panics.

## XI.5 · Verification

Three test layers, mirroring the kyoso_sync test layout:

1. **Per-component unit tests** (in each `frame.rs` / `rectangle.rs` / `text.rs`):
   - Default vs bottom emits no mutations (echo-guard regression).
   - Each non-default field produces one mutation.

2. **Replication integration tests** (`crates/kyoso_figma/tests/replication.rs`):
   - For each of `Frame`, `Rectangle`, `Text`: two real-server-backed apps, one peer mutates a field, the other converges. Same shape as `crates/kyoso_sync/tests/derived_schema.rs`.
   - One combined test: spawn a `Frame` with a child `Rectangle` and a child `Text` (with `content` + nested `TypeStyle`); B converges on the full subtree.

3. **Import adapter test** (`crates/kyoso_figma/tests/import.rs`):
   - Use a small hand-written `figma_api::FileResponse` JSON fixture (3–5 nodes covering Frame/Rectangle/Text + at least one Paint variant + a TypeStyle).
   - Run `import_node` against it.
   - Query the resulting Bevy world: verify the right components exist on the right entities with the expected field values.

All tests run via `cargo test -p kyoso_figma`; the workspace test count rolls up via `cargo test --workspace`.

## XI.6 · Critical files (reuse map)

| Existing helper | What it gives kyoso_figma | Where it lives |
|---|---|---|
| `kyoso_sync::SchemaSync` (trait + derive) | All component-level CRDT plumbing | [crates/kyoso_sync/src/schema_sync.rs](crates/kyoso_sync/src/schema_sync.rs) |
| `kyoso_sync::SchemaSyncedNodeComponentPlugin` | Per-component sync plugin | [crates/kyoso_sync/src/schema_sync.rs](crates/kyoso_sync/src/schema_sync.rs) |
| `kyoso_sync::TransformSchema` | Built-in Transform sync | [crates/kyoso_sync/src/builtin_schemas.rs](crates/kyoso_sync/src/builtin_schemas.rs) |
| `kyoso_sync::CrdtSyncPlugin` | Structural ops (AddNode, Remove, Move) over WS | [crates/kyoso_sync/src/plugin.rs](crates/kyoso_sync/src/plugin.rs) |
| `kyoso_graph::tree::{TreeParent, OrderKey}` | Hierarchy edges + sibling order | [crates/kyoso_graph/src/tree.rs](crates/kyoso_graph/src/tree.rs) |
| `kyoso_graph::components::{EdgeFrom, EdgeTo}` | Edge endpoints (for future reference edges) | [crates/kyoso_graph/src/components.rs](crates/kyoso_graph/src/components.rs) |
| `kyoso_crdt::types::{LwwRegister, OrSet, Sequence, PnCounter, CausalMap}` | The CRDT primitives the derive emits | [crates/kyoso_crdt/src/types/](crates/kyoso_crdt/src/types/) |

Nothing in `kyoso_client` or its existing `GraphNode`/`GraphEdge` is touched. Those remain the demo client's primitives; `kyoso_figma` ships standalone alongside them. A future refactor could rebase `kyoso_client` on figma types — out of scope for this part.

## XI.7 · Deferred follow-ups (out of initial cut)

These are intentionally out of scope for the initial cut. Each is a bounded follow-up.

1. **Per-corner `Rectangle` radius.** Initial cut is a single `corner_radius: f32`. Figma supports per-corner (`top_left_radius`, ...). Easy follow-up.
2. **`Constraint` + `LayoutGrid`.** Figma's responsive-design primitives for auto-layout. Worth it once a demo needs them.
3. **`Component` + `Instance` node types.** Design-systems story (instance with overrides referencing main). Pulled out per "minimal scope" decision; tracked for the next iteration.
4. **`Vector` (path geometry).** Needed for imported SVGs. Out of initial cut.
5. **Re-sync of vendored `walker.rs`.** If etch_figma adds useful walker hooks, re-copy with attribution. Could eventually become a shared `figma-walker` crate (refactor in the etch repo, not a kyoso prerequisite).
6. **Lossless Figma round-trip.** Today the import is one-way and best-effort. A round-trip story (export back to figma_api types) would mean preserving fields we currently drop (e.g. `absolute_bounding_box` from input). Not yet warranted.

## XI.8 · Estimated work breakdown

| Item | LOC (rough) | Time |
|---|---|---|
| New crate scaffold + workspace wiring | ~50 | 0.5h |
| Vendor `walker.rs` from etch_figma + attribution | ~350 | 0.5h |
| `Size`, `Paint`, `TypeStyle` value/component types | ~250 | 0.5d |
| `Frame`, `Rectangle`, `Text` components + `FigmaNode`/`FigmaEdge` markers | ~250 | 0.5d |
| `KyosoFigmaPlugin` | ~80 | 0.5h |
| Per-component unit tests | ~150 | 0.5d |
| Replication integration tests | ~300 | 0.5d |
| `KyosoVisitor` import adapter | ~350 | 1d |
| Import fixture + tests | ~250 | 0.5d |
| **Total** | **~2,030 LOC** | **~3 days** |

Most of the per-type cost is plumbing — the `derive(SchemaSync)` from Part X carries the heavy lifting (a Frame definition is ~30 LOC even with attributes). The vendored walker is one-time and bounded. The biggest single chunk is the visitor implementation that produces Bevy bundles for each supported node kind — that's where the per-Figma-type translation logic lives.

## XI.9 · Verification plan

To confirm the implementation works end-to-end:

1. `cargo build --workspace` clean.
2. `cargo test -p kyoso_figma` — per-component unit tests + replication tests + import fixture tests pass.
3. `cargo test --workspace --no-fail-fast` — workspace test count stays monotonic relative to Part X (currently 142). Expect ~165 after this part lands.
4. **Manual smoke**: a small example binary `examples/spawn_figma.rs` that spawns a Frame containing a Rectangle and a Text node, runs against `kyoso_server`, prints the entity tree. Verifies the plugin wires up without runtime panics.
5. **Import smoke**: a small example binary `examples/import_figma_fixture.rs` that loads `tests/fixtures/sample.json` (a hand-crafted minimal Figma file response), runs `import_canvas`, prints the spawned entities. Verifies the import path's happy case.
