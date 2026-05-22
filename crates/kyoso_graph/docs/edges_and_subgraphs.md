# Edges and reusable subgraphs

A graph's expressive power doesn't come from "nodes and edges" — it comes
from what *kinds* of edges exist and what *patterns* of subgraph are
treated as first-class. This document is a design-level overview of:

1. The two structural classes of edge in kyoso.
2. The reference-edge **categories** (use cases) the system recognises,
   each of which carries different semantics, conflict policies, and
   agent affordances.
3. The idea of a **reusable subgraph** ("component" in the Figma sense)
   as a graph-layer concept, and how it connects back to the edge
   taxonomy and to [`crate::subgraph::SubgraphMatches`].

The CRDT-level encoding of edge categories already lives in
[`kyoso_graph_crdt::edge_category`]; this document is the graph-layer
companion — what edges *mean* to traversal, queries, and agents.

[`kyoso_graph_crdt::edge_category`]: ../../kyoso_graph_crdt/src/edge_category.rs

---

## 1. Two structural classes of edge

kyoso documents have two structurally distinct kinds of edge. Both are
represented in the ECS, but they're shaped differently because they
answer different questions.

### Tree edges — the containment scaffold

A node's parent + sibling-ordering. Identified not by a separate edge
entity but by the node's own components:

- [`crate::tree::TreeParent`] — `Option<Entity>` parent.
- [`crate::tree::OrderKey`] — fractional sibling ordering (LSEQ-like).

Tree edges form a forest: every node has at most one parent, no cycles.
Replicated through a single [`Move`][move-op] op in the CRDT layer.

[move-op]: ../../kyoso_graph_crdt/src/op.rs

**Use cases**: containment (frame → child frames), document outline,
layer panel, transform hierarchy.

**Why a separate class**: tree mutations need stronger invariants
(uniqueness of parent, no cycles, deterministic sibling order) than
reference edges. Encoding them as node-level state instead of edge
entities makes those invariants representable.

### Reference edges — many-to-many cross-references

Edge entities carrying [`EdgeFrom`] / [`EdgeTo`]
([`crate::components`]). Replicated through `AddRefEdge` /
`RemoveRefEdge` ops, each tagged with an [`EdgeCategory`][category]
that says what kind of relationship the edge expresses.

[category]: ../../kyoso_graph_crdt/src/edge_category.rs
[`EdgeFrom`]: ../../kyoso_graph/src/components.rs
[`crate::components`]: ../../kyoso_graph/src/components.rs

Unlike tree edges these are:
- Many-to-many (a node can be the target of many `InstanceOf` edges, the
  source of many `MaskOf` edges, etc.).
- Cyclic-allowed (the graph layer doesn't forbid them; specific
  categories may, e.g. `InstanceOf` forbids self-containment).
- Independently tombstone-able (the dangling-endpoint policy is
  per-category — see [`DanglePolicy`][danglepolicy]).

[danglepolicy]: ../../kyoso_graph_crdt/src/edge_category.rs

---

## 2. Reference-edge use cases

The categories enumerated by [`EdgeCategory`][category] aren't arbitrary
— each one corresponds to a distinct *semantic role* that the editor,
the solver, and the agent treat differently. They cluster into a few
families.

### Identity / authorship

- **`InstanceOf`** — "this node is an instance of that component
  definition." Drives propagation: changes to the def flow to instances
  unless overridden. The subject of §3 below.

- **`StyleRef`** — "this node uses that shared style/variable." A weaker
  version of `InstanceOf` — propagates property values, but the
  consuming node still owns its own structural identity.

### Behaviour wiring

- **`PrototypeLink`** — "interacting with this node transitions to
  that node" (state-machine-like; Figma prototyping). The graph
  induced by `PrototypeLink` edges is a separate logical graph that the
  prototyping runtime walks at preview time, independent of containment
  or layout.

### Layout / constraint

- **`ConstraintPin`** — "this node's position/size is anchored to that
  node." Consumed by the solver ([`crate::solver`]). The graph of
  constraint edges is the solver's dependency graph; cycles here are an
  over-constraint error.

### Annotation / collaboration

- **`CommentAnchor`** — "this comment thread is anchored to this node."
  The edge is the link between collaboration metadata and document
  state. Survives unrelated edits to the node.

- **`Mention`** — "this thread mentions this user/node." Reverse-indexed
  for notifications.

### Rendering modification

- **`MaskOf`** — "this node masks that node's pixels." The mask graph
  is consulted by the rasteriser; deletions cascade
  ([`DanglePolicy::Cascade`][danglepolicy]) because a mask without a
  target is meaningless.

### Untyped fallbacks

- **`Reference`** — generic. Used when the caller doesn't pick a
  category. Acceptable for prototyping but loses the per-category
  semantics; production code should pick a concrete category.

- **`Custom(String)`** — app-level extension. Opaque to the kernel.

### Why the categories matter for traversal

Most queries are scoped to one category. "What does this node depend on
for layout?" walks `ConstraintPin` edges only. "What are the live
instances of this component?" walks `InstanceOf` only. The current
`GraphQuery` is category-agnostic; a future `CategoryQuery<C>` would
filter at the index level. (Today's filtering is post-hoc via edge
predicates in pattern matching — see [`crate::pattern`].)

---

## 3. Reusable subgraphs

A *reusable subgraph* is the graph-layer name for what design tools call
a **component** — a named, addressable chunk of structure that can be
instantiated repeatedly, with both invariant ("the shape stays a
button") and variable ("this button's label can change") parts.

The word "component" is heavily overloaded — Bevy components, React
components, Figma components — so this document uses **definition**,
**instance**, and **slot** instead.

### The three things a "component" actually is

A Figma-style component compresses three orthogonal concerns into one
concept. Keeping them apart is what makes the design tractable:

1. **A named, addressable subgraph.** The definition. A specific
   subgraph in the document, marked as "this is a thing called Button."
2. **A contract.** A schema declaring which parts of that subgraph are
   meant to vary in instances (the *slots*) and which are invariant
   (everything else).
3. **An instantiation mechanism.** Some way to place a copy that
   *stays linked* to the source — so updates to the definition flow
   into instances, and per-instance overrides survive definition
   changes.

Each of those is a separate design choice with separate consequences.

### Choice 1: where do instances live?

Three flavours, very different consequences:

| Model | Storage | Query cost | Propagation | Fits today |
|---|---|---|---|---|
| **Eager / materialised** | O(n × instances) — full clone in ECS | Native — works with existing `GraphQuery` | Walk instances on def change | Yes |
| **Lazy / virtual** | O(defs + overrides) | Every query needs an expansion view | Automatic (read expands fresh) | Requires new view type |
| **Hybrid (Figma's)** | Materialise children with stable links back to def | Native, with link-aware queries | Walk linked children, skip overridden ones | Yes, with one new component |

The recommended starting point is **hybrid**. It's the only one where
CRDT sync, agent queries, and the existing traversal infrastructure all
keep working without special-casing — instances are real graph topology
that carries a back-reference to the definition.

### Choice 2: how is identity preserved across def ↔ instance?

This is the linchpin. You need a stable map from a node in the
definition to its mirror in each instance, so:

- A change to def-child *X* knows which instance entities to update.
- An override ("made this rectangle red in this instance") survives
  reorderings and insertions in the definition.

Two ways to encode it:

- **Structural path** (`[0, 1, 2]` = "first child of first child of
  root"): fragile under reordering, but storage-free.
- **Stable id per def-child**: each cloned instance-child carries
  a `LinkedToDef(def_child_entity)` component. Robust; one extra
  component per cloned node. This is what Figma does and what's
  recommended here.

### Choice 3: the override taxonomy

Overrides come in a few flavours that look different but reduce to a
small set of mechanisms:

| Kind | Example | Mechanism |
|---|---|---|
| Property override | text content, fill colour | component on instance-child |
| Visibility override | hide this def-child in this instance | bit flag component |
| Structural insertion | add a sibling not in the def | normal child + `AddedByInstance` marker |
| Sub-instance swap | replace nested Button with IconButton | swap the `InstanceOf` target |
| Reorder | move child to different sibling position | property override on `OrderKey` |
| Detach | break the link entirely | remove the link components |

Deletion of a def-child within an instance is best modelled as a
visibility override, not a structural removal — that way restoring the
def-child (an undo elsewhere) makes it reappear.

### Choice 4: the contract — slots vs invariants

This is the interesting axis for an *agent*. A definition publishes a
manifest:

```text
Button:
  variant slots:
    label   : text content of #/root/text
    style   : enum { primary, secondary }
    onClick : … behaviour wiring
  invariant:
    everything else (rectangle dimensions, layout, padding, …)
```

The manifest tells:

- The editor UI what to expose for direct editing.
- The agent what's safe to mutate (slots) versus what would break the
  contract (invariants).
- The propagation system which slots to leave alone when the def
  changes (overrides at slots are sticky; non-slot changes propagate).

This slot manifest is structurally similar to a [`crate::pattern::Pattern`]
— a small graph with per-node and per-edge constraints. That's not a
coincidence (see below).

### Choice 5: propagation semantics

When the definition changes, what happens to instances?

- **Live propagation** — changes flow into all instances at
  non-overridden slots, immediately. Figma's behaviour. Best for
  design fidelity.
- **Versioned** — the instance pins to a def revision; users opt into
  upgrades. Better for library cases where surprise diffs are
  unwelcome.
- **Mixed by slot** — invariant changes propagate live; slot changes
  become defaults for new instances only. Closest to user expectation
  because the contract already distinguishes the two kinds.

**Mixed by slot** is the recommended default: it uses the manifest you
already need for the editor and the agent.

### Cycles and nesting

A definition cannot transitively contain an instance of itself. The
check is one [`crate::subgraph::SubgraphMatches`] call at insertion
time, plus reachability via `InstanceOf` edges.

Nested instances (Button inside Card) are fine. Overrides at depth walk
the `InstanceOf` chain to find which definition owns each slot.

---

## 4. Components *are* patterns

A `Definition` and a [`crate::pattern::Pattern`] are the same shape —
a small graph with per-node and per-edge constraints. The difference is
interpretation:

- A `Pattern` says: *find subgraphs that look like this.*
- A `Definition` says: *this is a subgraph that looks like this; keep
  instances looking like it.*

Exploiting that symmetry:

- "Find all Button instances" is `graph.instances_of(button_def)` —
  indexed by the `InstanceOf` component, O(matches).
- "Find Button-shaped subgraphs that *aren't* instances yet" (candidates
  for componentisation) is `graph.subgraph_matches(button_def.as_pattern())`
  minus the indexed instances. Useful agent operation.
- "Find Button instances that have drifted from the contract" is
  `subgraph_matches` over the definition's invariant slots only, filtered
  to instance roots.

So the two systems converge: **components are named, indexed patterns
with a contract.**

---

## 5. Cross-cutting design themes

These themes show up in multiple categories above; collecting them in
one place keeps the per-category descriptions short.

### CRDT policies are per-category

Each [`EdgeCategory`][category] picks its own
[`RefEdgePolicy`][refedgepolicy] (add/remove conflict resolution) and
[`DanglePolicy`][danglepolicy] (what happens when an endpoint
tombstones). `InstanceOf` might want `Tolerate` (Figma keeps the broken
link); `MaskOf` wants `Cascade` (a mask without a target is
meaningless).

[refedgepolicy]: ../../kyoso_graph_crdt/src/edge_category.rs

### Per-category property schemas

Reference edges can carry properties (a `PrototypeLink` has an easing
curve and trigger condition; an `InstanceOf` has its override delta).
The schema lives next to the category, via
[`RefEdgeCrdt::Properties`][refedgecrdt] — one CRDT struct per
category, derived through `kyoso_crdt_derive`.

[refedgecrdt]: ../../kyoso_graph_crdt/src/edge_category.rs

### Pattern matching is category-aware

[`crate::subgraph::SubgraphMatches`] currently treats all edges
uniformly. The next useful extension is per-pattern-edge category
constraints — "this pattern edge must be an `InstanceOf` edge" — so
that a single pattern can express "an instance of Button whose
constraint-pin target is inside the same frame."

### Agent affordances

Categories give agents *semantic verbs*:

- `instances_of(def)` — indexed lookup, not a pattern walk.
- `dependencies_of(node, ConstraintPin)` — solver-shaped query.
- `comments_on(node)` — collaboration-shaped query.
- `prototype_reachable_from(node)` — behaviour-shaped query.

These are cheaper and more legible than the same query expressed as a
generic pattern — and they let the agent reason at the level the user
thinks at, instead of at the level of edge entities.

---

## 6. Open questions

Things still to decide, recorded so they don't get lost:

1. **Materialisation model.** Hybrid (clone + link) is the
   recommended default, but committing to it locks in the most code.
2. **Slot manifest authoring.** Declarative (def author publishes the
   schema) or inferred (from "what was overridden on the most recent
   instance")? Declarative is recommended — inferred is confusing in
   practice.
3. **Cross-category constraints on patterns.** Should
   [`crate::pattern::PatternEdge`] grow an optional
   [`EdgeCategory`][category] filter, or should categories be expressed
   via edge predicates? Adding a first-class field is more
   index-friendly.
4. **Variants (in the Figma sense).** A "variant group" of
   definitions (Primary/Secondary, Small/Medium/Large) — modelled as
   sibling definitions with a shared manifest, or as one definition
   with a discriminated property? Figma does the former; the latter is
   smaller but less flexible.
