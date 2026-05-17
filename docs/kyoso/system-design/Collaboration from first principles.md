

Collaboration? 


Kyoso aims to be a substrate which can bear the weight of different domain-specific use cases. CRDTs are key piece of the infrastructure, and need to be able to adapt flexibly to different settings.

- state of the art? How do we validate this?
- transport / prevesence vs. storage, branching, composition,


Centre of gravity? ECS / CRDT: how are these conjoined?

Graph CRDT (and specifically, trees)
- figma-shaped document: graph is essentially a tree with a few cross-refferences 
- nodes? edges


Composition
 key idea is **lattice composition**: if every embedded value is itself a CRDT (a join-semilattice), a `Map<key, CRDT>` is also a CRDT — merges propagate up

A **join-semilattice** is a set `S` with a binary operation `⊔` (called **join**) satisfying:

- **Associativity**: `(a ⊔ b) ⊔ c = a ⊔ (b ⊔ c)`
- **Commutativity**: `a ⊔ b = b ⊔ a`
- **Idempotency**: `a ⊔ a = a`

These three together induce a **partial order** `≤` defined by `a ≤ b ⟺ a ⊔ b = b`. The join is the **least upper bound** under that order.

A CRDT is a join-semilattice plus a "bottom" element `⊥` (initial state) and a set of **inflation operations** (mutations) that move you *up* the lattice (`mut(s) ≥ s`). Convergence is then a theorem: any two replicas, after exchanging their states and joining, reach the same state. The three semilattice axioms are exactly the safety net you need:

- Commutativity → order of message arrival doesn't matter.
- Associativity → grouping of joins doesn't matter.
- Idempotency → re-delivering the same message doesn't matter.



Base CRDT primitives:

Each impl provides `Crdt` + `Lattice` + `SchemaApply` + `From<TypedDelta> for WireDelta` + `TryFrom<WireDelta> for TypedDelta`, all property-tested for the lattice axioms.






Intermediate representation





















Three flavours of CRDTs and their relationship to the lattice





1. **Edge typology** — the idea that not all edges in the graph want the same CRDT semantics. Tree edges (parent-child structure), reference edges (component instance → main, prototype links, mentions), and derived edges (selection, hover, focus chains) each have different invariants, different concurrency behavior, and different performance/storage profiles. The current kyoso `AddEdge` op treats all edges uniformly; this section explores what's gained by typing them.
2. **Composition** — kyoso has a multi-layer document: a graph of nodes connected by edges; each node has properties (some scalar, some structured); each edge can also have properties; the whole thing must converge. This is the recurring CRDT-composition problem: how do you build a system where every layer is independently a CRDT, where you can register new CRDT *types* per field, where causal context is shared coherently, and where the algebra of "compose two CRDTs to get a third" is well-defined and uniform.




synchronisation architecture: convergece, latency, offline, auth, perisstence, scaling
- server-mediated vs full P2P

Hybrid? Stoage vs. presence

Sync points


Haven't looked at offline? Buffer? 
no auto-reconnect, no offline buffer, no backpressure on outbound mpsc, no presence heartbeat/timeout, `ConnectToolPlugin` stubbed.


Presencs vs. stoage


Presence transport?


How do we project the CRDT state to ECS?


Presence vs. Storage:
- presence: selection set; cursor









