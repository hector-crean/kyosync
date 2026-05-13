

substrate? 
CRDTs are again an infrastructure, which require some flexibility
Also: most systems require a layering of different CRDT structures (i.e tree, but also comments etc.)



- peer to peer? Server authoritative?

- brancing?
- snapshot / comrpession of history, while allowing undo/redo? Garbage collection
- Message encoding / transport? postcard-encoded binary frames over a single axum WebSocket.
- **Server topology**: single central axum hub, one `tokio::sync::broadcast` per room (capacity 256), Postgres tables `rooms / ops / snapshots / peer_acks` (sqlx). Append lock serializes seq assignment across concurrent peers
- **Client → ECS bridge**: `kyoso_sync::CrdtSyncPlugin` runs `inbound_system` (drains WS, applies remote ops to backend, projects into ECS), detection systems for local Bevy `Added`/`Changed` (echo-suppressed via `GraphEntityIndex`), and `outbound_system` (drains pending ops, sends WS) 


Graph (specialised to tree)

Compositions of CRDT
- Node properties?
- Edge properties?
- Supersturcutre of graph connectivity?






**For figma-shaped documents the graph is essentially a tree with a few cross-references** (component instances → main components, prototype links between frames, constraints). It's worth considering **typed edges with separate CRDT semantics**:
- `tree` edges: Kleppmann move + OrderKey (parent-child structure).
- `reference` edges: 2P2P-graph add/remove (component instance → main, prototype links).
- `derived` edges: not synced — recomputed from other state (selection, hover).







sychronisation archtecture
- convergence, latency, offline, auth/permissions, persistence, scaling


Haven't looked at offline? Buffer? 
no auto-reconnect, no offline buffer, no backpressure on outbound mpsc, no presence heartbeat/timeout, `ConnectToolPlugin` stubbed.



How do we project the CRDT state to ECS?


Presence vs. Storage:
- presence: selection set; cursor


Edge kinds -- what may these be for?






