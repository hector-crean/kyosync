# `kyoso_sync` inside `kyoso_client` — message bus architecture

## Context

`kyoso_client` runs **two independent message buses** plus a CRDT-replicated
ECS world, and the relationships between them aren't obvious from any single
file. This document maps:

1. The four foundational layers underneath `kyoso_sync` (CRDT model, server
   total order, ECS↔CRDT bridge, client transport) — at conceptual depth.
2. The *other* in-process channels the client uses (`AppCommand`/`AppEvent`
   `DuplexPlugin`, `GLOBAL` singleton, per-tool sub-command/sub-event
   streams, `GraphMessage`/`GraphCommand` from `kyoso_graph`).
3. How those channels and `kyoso_sync` actually interact — what's plugged
   into what, where the two buses cross, and where they're deliberately
   kept apart.

No code changes; this is the mental model needed before any further work
on the client.

---

## The one-paragraph mental model

The client has **one nervous system (the Bevy ECS world)** with **two
independent peripheral nerves**:

- The **WebSocket nerve** (owned by `kyoso_sync::CrdtSyncPlugin`) carries
  CRDT ops between this client, the server, and every other peer.
- The **Duplex nerve** (owned by `kyoso_client::msg::DuplexPlugin`) carries
  high-level `AppCommand`s in from external producers (JS via wasm-bindgen,
  an MCP server, an agent framework, a CLI) and `AppEvent`s out to external
  observers.

Neither nerve talks to the other directly. Both terminate in the ECS:
external commands mutate components, remote ops mutate components, and Bevy
change-detection systems on the bridge layer fan changes back out to whichever
nerve cares (the sync layer broadcasts ops; the duplex layer broadcasts
`AppEvent`s). Because the buses share only the ECS, **`AppEvent::NodeAppeared`
fires identically whether the cause was a local `AppCommand::Create(...)` or
a remote `Apply(AddNode)`** — the cause is invisible by the time the event
is emitted.

```
   external producer            external observer
   (JS / MCP / agent)           (UI / log sink / MCP)
          │                              ▲
          ▼ AppCommand                   │ AppEvent
   ┌──────────────────────────────────────────────┐
   │ DuplexPlugin (crossbeam in / crossbeam out)  │
   └─────┬─────────────────────────────────▲──────┘
         │ MessageWriter<AppCommand>       │ MessageReader<AppEvent>
         ▼                                 │
   ┌──────────────────────────────────────────────┐
   │ Bevy ECS world                               │
   │  ├─ Tool state (Select / Create / Connect)   │
   │  ├─ entities w/ GraphNode, EdgeFrom/To,      │
   │  │   Transform, GraphEdge                    │
   │  └─ Graph<N,E,CrdtBackend>, GraphEntityIndex │
   └─────▲─────────────────────────────────┬──────┘
         │ inbound_system applies ops      │ detect_* systems emit ops
         │ projects into ECS               │
   ┌──────────────────────────────────────────────┐
   │ CrdtSyncPlugin internals                     │
   │  ├─ WsClient (tokio runtime + crossbeam IPC) │
   │  └─ NodePropertyProjectors (reflection)      │
   └────────────────┬─────────────────────────────┘
                    │ ClientMsg / ServerMsg (postcard, WebSocket)
                    ▼
              kyoso_server
```

---

## 1. The four foundational layers (recap)

Same model as before; here in compressed form.

### 1.1 CRDT semantics (`crates/kyoso_crdt`)

Hand-rolled, graph-shaped. Seven ops: `AddNode`, `AddEdge`, `RemoveNode`
(tombstone), `RemoveEdge` (tombstone), `SetNodeProperty` (LWW per key),
`SetEdgeProperty` (LWW per key), `Move` (atomic Kleppmann tree move with
deterministic cycle rejection).

Identity is two-tier:

- `CrdtId = (peer: u32, local_seq: u64)` — assigned by the originator,
  collision-free without coordination.
- `GlobalSeq: u64` — assigned by the **server** at append time. Every replica
  applies ops in `GlobalSeq` order, so concurrent edits converge on the same
  state without vector clocks.

Encoding is `postcard`. Property values use Bevy `ReflectSerializer` →
postcard → `Vec<u8>`, which is what makes `SyncedNodeComponentPlugin<_,_,T>`
able to replicate any `Reflect`able component without per-type op handlers.

Files: [id.rs](crates/kyoso_crdt/src/id.rs), [op.rs](crates/kyoso_crdt/src/op.rs),
[protocol.rs](crates/kyoso_crdt/src/protocol.rs),
[backend.rs](crates/kyoso_crdt/src/backend.rs).

### 1.2 Server total ordering (`apps/kyoso_server`)

One `/ws` route, one `/health`. Per-room state lives in a `DashMap` and is
lazy-loaded on first join. Hot path on `Submit`:

1. Take per-room `append_lock`.
2. `OpStore::append` — atomic Postgres CTE assigns `GlobalSeq` and inserts
   the postcard blob.
3. `mirror.apply_remote(&stamped)` so the server can build snapshots.
4. `broadcast::send(ServerMsg::Apply(stamped))` to every subscribed peer
   (including the originator).

Tables: `rooms`, `ops`, `snapshots`, `peer_acks`. Background tasks: snapshot
every 60s, GC ops below `min(min_peer_ack, snapshot_seq)` every 120s.
Handshake: `Hello { room, since }` → `Welcome { peer, snapshot?, diff }`.

No auth, no presence, no per-peer cursors.

Files: [room.rs:97](apps/kyoso_server/src/services/room.rs:97) for `submit()`,
[room_ws.rs:149](apps/kyoso_server/src/handlers/room_ws.rs:149) for the
handshake.

### 1.3 ECS↔CRDT bridge (`crates/kyoso_sync`)

The client keeps two stores in lockstep. The **`Graph<N,E,CrdtBackend>`**
resource is the local CRDT replica; the **Bevy ECS world** is the local
"observable" view. **`GraphEntityIndex`** is the bidirectional `CrdtId ↔
Entity` map that lets the bridge cross between them.

The pipeline runs every `Update`:

```
inbound_system  →  detect_added_*  →  detect_changed_*  →  outbound_system
   ▲                                                              │
   │                                                              ▼
   WsClient::try_recv()                                  WsClient::send(op)
```

Echo prevention is **structural, not flag-based**: when `inbound_system`
applies a remote `AddNode`, it spawns the entity AND inserts into
`GraphEntityIndex`. The detection systems then run, see `Added<GraphNode>`
on the new entity, check the index, find it already mapped, and skip
emitting an op. Same logic for property changes: a `SetNodeProperty` op
sets a flag on the projector saying "next change is from inbound, ignore",
and the projector clears it after one frame.

Property projection uses Bevy reflection: `SyncedNodeComponentPlugin::<_,_,T>`
walks `T`'s reflected fields once at plugin build, registers each as a
projector keyed `"T::field.path"`, and dispatches both directions through
those keys.

Files: [plugin.rs](crates/kyoso_sync/src/plugin.rs:46) for the system pipeline
and `Syncable` trait.

### 1.4 Client transport (`crates/kyoso_sync::client`)

`WsClient` owns a spawned `tokio::runtime::Runtime` on its own thread plus
two channels: `tokio::mpsc` outbound (Bevy → io task), `crossbeam_channel`
inbound (io task → Bevy). Crossbeam matters here — `try_recv()` from the
Bevy main loop needs to be non-blocking and runtime-free.

The io loop is `tokio::select!` over: outbound op, ack tick, inbound binary
frame, ws closed. `SyncStatus` resource transitions `AwaitingWelcome →
Connected{peer} → Disconnected`.

Real gaps: no reconnect on `Disconnected`, no offline buffer (ops generated
post-disconnect die in `backend.pending`), no backpressure on the outbound
mpsc, no explicit `Close` frame on shutdown.

Files: [client.rs](crates/kyoso_sync/src/client.rs).

---

## 2. The other message channels in `kyoso_client`

`kyoso_sync` is **one** of multiple parallel message systems in the client.
Here's the full inventory.

### 2.1 `DuplexPlugin<In, Out>` — the external API bridge

Lives in [msg/duplex.rs](apps/kyoso_client/src/msg/duplex.rs:43). A plain
crossbeam-to-Bevy pump:

- `PreUpdate` system drains a crossbeam `Receiver<In>` and writes Bevy
  `MessageWriter<In>`.
- `PostUpdate` system reads Bevy `MessageReader<Out>` and forwards on a
  crossbeam `Sender<Out>`.

Constructed via `create_duplex_plugin::<AppCommand, AppEvent>()` which
returns `(plugin, ext_rx: Receiver<AppEvent>, ext_tx: Sender<AppCommand>)`.
The plugin is added to the app; the `ext_*` ends are handed to whoever needs
them — JS bindings, MCP servers, the CLI, etc. All channels are MPMC, so
multiple producers can clone the same `Sender<AppCommand>` and feed the
same Bevy stream.

This bridge is **deliberately decoupled from `kyoso_sync`**: external
producers don't talk to the WebSocket directly. They mutate intent via
`AppCommand`; the resulting ECS mutation flows through the sync layer's
detection systems if and only if it touches a synced component.

### 2.2 `GLOBAL` — process-wide handle to the duplex bridge

[msg/global_channel.rs:113](apps/kyoso_client/src/msg/global_channel.rs:113).
A `once_cell::Lazy` static `GlobalEventChannel<AppCommand, AppEvent>` with
`set_sender` / `set_receiver` set once at startup and `send` / `try_receive`
callable from anywhere.

The reason this exists, and the only reason: some external producers
(wasm-bindgen FFI handlers, observer callbacks registered with no userdata
slot, agent tool implementations called from a foreign runtime) cannot have
a `Sender` threaded through to them. `GLOBAL` is the escape hatch. Wired up
in [lib.rs:111](apps/kyoso_client/src/lib.rs:111) inside `run()`.

### 2.3 `AppCommand` / `AppEvent` — the external API surface

[msg/command.rs](apps/kyoso_client/src/msg/command.rs:66) and
[msg/event.rs](apps/kyoso_client/src/msg/event.rs:19).

`AppCommand` is the **single inbound enum** for everything external producers
can ask the client to do. Today:

```rust
enum AppCommand {
    SetTool(Tool),                  // app-wide state change
    Select(SelectCommand),          // forwarded to Select tool
    Create(CreateCommand),          // forwarded to Create tool
}
```

`AppEvent` is the symmetric outbound enum:

```rust
enum AppEvent {
    Connected { peer: PeerId },
    NodeAppeared  { id: ExternalId, position: Pos2 },
    NodeMoved     { id: ExternalId, position: Pos2 },
    NodeRemoved   { id: ExternalId },
    EdgeAppeared  { id, from, to },
    CommandError  { message: String },
}
```

Two design notes that matter for understanding the bus topology:

- `ExternalId = kyoso_crdt::CrdtId`. The external API speaks **CRDT ids**,
  not Bevy `Entity`s. This is the right layer to expose because Bevy entities
  are local-process-only and aren't meaningful to JS/MCP/the server.
- `AppEvent` is **observed state, not causation**. `NodeAppeared` fires the
  same way for a locally spawned `GraphNode` and a remotely-applied `AddNode`
  — both produce an `Added<GraphNode>` query match in the same emission system
  ([scene.rs:112](apps/kyoso_client/src/scene.rs:112)).

### 2.4 Per-tool sub-commands and sub-events

Each tool has its own message pair, registered by its plugin and consumed
only when that tool is the active state.

`Select` — [tool/select.rs:17](apps/kyoso_client/src/tool/select.rs:17):

```rust
SelectCommand { Select { target }, ClearSelection, DeleteTargets { ids } }
SelectEvent   { Selected { target }, SelectionCleared, DeletedTargets { ids } }
```

`Create` — [tool/create.rs:18](apps/kyoso_client/src/tool/create.rs:18):

```rust
CreateCommand { SpawnNodeAt { position, color }, SpawnNodeAtCursor { color } }
CreateEvent   { NodeSpawned { entity: u64 } }
```

Why one enum per tool instead of one giant enum — quoting the doc comment in
[tool/mod.rs:10](apps/kyoso_client/src/tool/mod.rs:10):

- **Clear ownership** — each `*Command` lives next to its consumer.
- **Composability** — drop a tool plugin and the corresponding variant
  becomes a no-op.
- **Agent-friendly** — each enum can derive `JsonSchema` independently.

The per-tool messages are **internal**. They don't cross the duplex bridge.
External producers send `AppCommand::Select(SelectCommand::...)`; the
`dispatch_app_commands` system unpacks and forwards to
`MessageWriter<SelectCommand>` ([handlers.rs:20](apps/kyoso_client/src/handlers.rs:20)).

### 2.5 `GraphMessage` / `GraphCommand` from `kyoso_graph`

Defined in [kyoso_graph/src/lib.rs:53](crates/kyoso_graph/src/lib.rs:53) and
[lib.rs:113](crates/kyoso_graph/src/lib.rs:113) respectively.

`GraphCommand` is intent-based topology mutation: `Connect`, `Disconnect`,
`RemoveNode`, `RemoveEdge`, `InsertChild`, `Reparent`, `MoveSibling`. The
matching consumer is `consume_graph_commands` in `GraphManagerPlugin`.

`GraphMessage` is the corresponding observation: `NodeAdded`, `NodeRemoved`,
`EdgeAdded`, `EdgeRemoved`, `NodeConnected`, `NodeChanged`, `EdgeChanged`,
`PropagationTriggered`.

**`kyoso_client` does not currently add `GraphManagerPlugin`.** The CRDT
sync layer fills the same role — it owns the `Graph<N,E,CrdtBackend>`
resource and projects ops directly into ECS. So `GraphCommand` and
`GraphMessage` are dormant in the client; they're available for direct
consumers of `kyoso_graph` (the `scene_tree` example, the constraint/recipe
modules) but the client uses the more specific tool-level commands instead.

This is a useful boundary to know about. If the CRDT requirement were ever
relaxed (e.g. for a single-user offline mode), swapping `CrdtSyncPlugin` for
`GraphManagerPlugin` would give you the same ECS pipeline minus the
replication.

### 2.6 The UI nerves

[ui/toolbar.rs](apps/kyoso_client/src/ui/toolbar.rs) and
[ui/hotkey.rs](apps/kyoso_client/src/ui/hotkey.rs). Both convert in-process
Bevy input/UI events into `MessageWriter<AppCommand>::write(...)`. They go
through the **same** Bevy message stream that the duplex bridge writes into,
so a hotkey press is indistinguishable from an MCP-issued command by the
time `dispatch_app_commands` reads it. That symmetry is what makes the client
"agent-first" — every UI affordance has a programmatic equivalent for free.

---

## 3. End-to-end traces

### 3.1 Local origin: hotkey "C" → click → spawn → broadcast

```
[user presses C]
ui::hotkey writes MessageWriter<AppCommand>(AppCommand::SetTool(Tool::Create))
   ↓
handlers::dispatch_app_commands reads, calls NextState<Tool>.set(Create)
   ↓ (state transition)
Tool::Create active

[user clicks canvas; UI handler converts cursor → world coord]
ui writes MessageWriter<AppCommand>(AppCommand::Create(CreateCommand::SpawnNodeAt {...}))
   ↓
handlers::dispatch_app_commands forwards → MessageWriter<CreateCommand>
   ↓
tool::create::handle_create_commands  (run_if(in_state(Tool::Create)))
   ↓ commands.spawn((GraphNode {...}, Transform {...}))
   ↓ writes CreateEvent::NodeSpawned

[next frame, ChangeDetection set]
kyoso_sync::detect_added_nodes
   ↓ Added<GraphNode> query matches the new entity
   ↓ GraphEntityIndex: NOT mapped → genuinely local
   ↓ allocate CrdtId via IdGenerator, insert into index
   ↓ push Op::AddNode + Op::SetNodeProperty(*) into backend.pending

scene::emit_node_appeared
   ↓ Added<GraphNode> matches; GraphEntityIndex now has the id
   ↓ writes MessageWriter<AppEvent>(AppEvent::NodeAppeared { id, position })

kyoso_sync::outbound_system
   ↓ drains backend.pending
   ↓ WsClient outbound mpsc send → io_loop on kyoso-sync-ws thread
   ↓ ws.send(Binary(postcard(ClientMsg::Submit(op))))

[server]
   append_lock → store.append → GlobalSeq = N
   mirror.apply_remote(stamped)
   broadcast.send(ServerMsg::Apply(stamped))   ← every peer including us

[back here, io_loop]
   ws.recv → decode → crossbeam.send(Inbound::Apply(stamped))

[next Bevy frame]
kyoso_sync::inbound_system
   ↓ try_recv() → backend.apply_remote(stamped)
   ↓ idempotent: op id already present in our local log; no-op visible state change
   ↓ projects: nothing to do, projector finds the entity already exists

[end of frame]
DuplexPlugin's PostUpdate
   ↓ MessageReader<AppEvent>.read() → ext_tx.send(AppEvent::NodeAppeared)

[external observer thread]
   ext_rx.recv() — JS sidebar / MCP tool / log sink wakes up
```

### 3.2 Remote origin: peer creates a node

```
[other peer submits AddNode; server appends + broadcasts]

[here, io_loop]
   ws.recv → decode → crossbeam.send(Inbound::Apply(remote_op))

[next Bevy frame]
kyoso_sync::inbound_system
   ↓ try_recv() → backend.apply_remote(op)
   ↓ projects:
   ↓   commands.spawn((GraphNode { /* default */ }, Transform::IDENTITY))
   ↓   GraphEntityIndex.insert(remote_crdt_id, new_entity)
   ↓ for each property op in the same batch:
   ↓   projector dispatches reflection-decoded value into the component

kyoso_sync::detect_added_nodes
   ↓ Added<GraphNode> matches the just-spawned entity
   ↓ GraphEntityIndex: ALREADY mapped → echo, skip op emission

scene::emit_node_appeared
   ↓ Added<GraphNode>; index has the id → writes AppEvent::NodeAppeared

DuplexPlugin PostUpdate
   ↓ external observer notified — same shape, same key, same id
```

The two traces are **structurally identical from `emit_node_appeared`
downward**. That's the payoff of putting the bus seam at the ECS layer.

---

## 4. Where the buses cross — and where they don't

They cross in exactly one place: **the ECS world**. Specifically:

| Bus → ECS direction                          | Mechanism                                              |
|----------------------------------------------|--------------------------------------------------------|
| WebSocket → ECS                              | `inbound_system` applies ops, spawns/mutates entities  |
| Duplex (`AppCommand`) → ECS                  | `dispatch_app_commands` → tool plugins → `commands.spawn` |
| ECS → WebSocket                              | `detect_*` systems → `outbound_system`                 |
| ECS → Duplex (`AppEvent`)                    | `emit_node_appeared`, `emit_node_moved`, `emit_connected_once` (and tool sub-events transitively) |

They do **not** cross directly. `AppCommand` does not write into the
WebSocket. `AppEvent` is not produced by remote `Apply` frames. The two are
strictly bridged through ECS state changes.

That's a design choice with a stated escape hatch. From [lib.rs:23](apps/kyoso_client/src/lib.rs:23):

> "Multiple producers can clone the same `Sender<AppCommand>` and feed the
> same Bevy stream — crossbeam channels are MPMC. The CRDT sync layer
> continues to run alongside on its own WebSocket channel; if you want one
> unified bus, just route the server's broadcasts through
> `GLOBAL.send(AppCommand::...)` too."

So if you wanted server-pushed events to also be visible as `AppCommand`s
to external producers (e.g. an agent that wants a hook on every CRDT op),
you'd add a small adapter that read from `WsClient::try_recv()` and
republished into `GLOBAL`. Today that adapter doesn't exist.

---

## 5. Plug-in install order in `run()`

[lib.rs:108–127](apps/kyoso_client/src/lib.rs:108) is the actual wiring.
Reading top-to-bottom:

1. **`create_duplex_plugin::<AppCommand, AppEvent>()`** → `(duplex, ext_rx, ext_tx)`.
2. `GLOBAL.set_sender(ext_tx)` and `GLOBAL.set_receiver(ext_rx)` — process-wide
   handles for FFI/MCP code that can't carry the `Sender` directly.
3. `App::new()`.
4. `DefaultPlugins` with a custom `WindowPlugin` (title, 900×600).
5. `add_plugins(duplex)` — the duplex bridge is added BEFORE `AppPlugin` so
   its `PreUpdate` drain runs before any system that reads `AppCommand`.
6. `add_plugins(AppPlugin { server_url, room })` which internally adds:
   - `CrdtSyncPlugin::<GraphNode, GraphEdge>::new(url, room)` — opens the
     WebSocket synchronously during `build()`, blocks on `Hello`/`Welcome`,
     installs `Graph<N,E,CrdtBackend>` + `GraphEntityIndex` + `SyncStatus`
     resources and the inbound/outbound systems.
   - `SyncedNodeComponentPlugin::<GraphNode, GraphEdge, Transform>::default()`
     — registers per-field projectors for `Transform`.
   - `ToolsPlugin` — registers `Tool` state, `SelectToolPlugin`, `CreateToolPlugin`.
   - `register_type::<Vec3>` and `register_type::<Quat>` — required for
     `ReflectSerializer` to walk Transform's nested types.
   - The four cross-cutting `Update` systems: `dispatch_app_commands`,
     `emit_connected_once`, `emit_node_appeared`, `emit_node_moved`.
7. `add_plugins(VisualPlugin)` which adds:
   - `MeshPickingPlugin` (Bevy built-in)
   - `PolylinePlugin` (line rendering for edges)
   - `DragTransform2dPlugin` (mouse-drag → `Transform`)
   - `UiPlugin` (toolbar, hotkey)
   - `setup_camera` startup system, the scene `on_*_added` observers, and
     `update_edge_polylines` per-frame.
8. `.run()`.

The ordering is significant in two places:

- Duplex BEFORE AppPlugin: so the `PreUpdate` drain runs first and
  `AppCommand`s are visible the same frame they're produced.
- `CrdtSyncPlugin` BEFORE `SyncedNodeComponentPlugin`: the latter looks up
  resources installed by the former.

---

## 6. What's not yet wired

These are the seams to push on next, called out so the architecture map is
honest about its present state:

- **`Tool::Connect`** exists as a state and is enumerable from JS/MCP today,
  but `ConnectToolPlugin` isn't implemented — there's no handler that
  consumes `AppCommand::Connect(ConnectCommand)` (and the variant doesn't
  exist on `AppCommand` either).
- **`CreateCommand::SpawnNodeAtCursor`** is a stub; the cursor → world
  coord helper isn't wired ([create.rs:66](apps/kyoso_client/src/tool/create.rs:66)).
- **`SelectCommand::Select`** doesn't actually store selection state; it
  just emits `SelectEvent::Selected` ([select.rs:65](apps/kyoso_client/src/tool/select.rs:65)).
- **`AppEvent::EdgeAppeared`** is defined but no system writes it. Edge
  spawns are not currently surfaced to external observers.
- **Reconnect / offline buffer / backpressure** in the transport — already
  covered above.
- **Bidirectional bus bridge** — server-pushed CRDT events are not
  republished into `GLOBAL` as `AppCommand`s. The `lib.rs` doc comment
  flags this as an intentional point of extension.
- **Presence / awareness** — not in the protocol, not in the server, not in
  the client. Cursors, selections, "who's online" all absent.

---

## 7. Suggested follow-ups (for a future planning round, not now)

If the next thing to build is "make this architecture more complete," the
highest-leverage moves in roughly this order:

1. **Wire reconnect** in `kyoso_sync::client` — re-issue `Hello { since:
   last_acked }` on `Disconnected`, drain `backend.pending` on reconnect.
   Snapshot recovery path is already complete server-side; just needs the
   client to ask.
2. **Add `EdgeAppeared` / `EdgeRemoved` emission** — round out the
   `AppEvent` surface so external observers see edges, not just nodes.
3. **Add `ConnectToolPlugin`** — completes the Tool trio that already
   ships `Tool::Connect` in the strum enum. Parallel pattern to
   `CreateToolPlugin`.
4. **Optional bus bridge** — a small system that mirrors `kyoso_sync`'s
   inbound stream into `AppCommand` on the duplex side, for agents that
   want to react to remote ops.
5. **Presence channel** — first protocol extension that doesn't go through
   the CRDT log. New `ClientMsg::Presence` / `ServerMsg::Presence` frames,
   ephemeral, broadcast without `GlobalSeq`. Useful for cursors and
   "user X is editing" UI.
