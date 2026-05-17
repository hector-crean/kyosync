

# Worked example: a `Card` component end to end

I'll use one component that touches every attribute, then peel the three layers: what **you write** → what **`derive(SchemaSync)`** generates → what **`derive(Crdt)`** generates from *that* → how the **plugins** drive it.

> Heads-up on status: `derive(Crdt)` exists today (`kyoso_crdt_derive`). `derive(SchemaSync)` is the Part IX plan — it isn't built yet, so Layer 1 below is *what the macro will emit*, matching the hand-written impls in `builtin_schemas.rs`.

---

## Layer 0 — what you write

```rust
use bevy::prelude::*;
use std::collections::HashMap;
use kyoso_sync::SchemaSync;

#[derive(Component, Clone, Default, PartialEq, SchemaSync)]
#[schema(name = "CardGeometry")]
pub struct CardGeometry {
    pub x: f32,            // implicit lww
    pub y: f32,            // implicit lww
    pub width: f32,        // implicit lww
    pub height: f32,       // implicit lww
}

#[derive(Component, Clone, Default, PartialEq, SchemaSync)]
#[schema(name = "Card")]
pub struct Card {
    pub title: String,                              // implicit lww

    #[crdt(or_set)]
    pub labels: Vec<String>,                        // OrSet<String>

    #[crdt(sequence)]
    pub body: String,                               // Sequence<char> — collaborative text

    #[crdt(map)]
    pub field_values: HashMap<String, String>,      // CausalMap<LwwRegister<String>>

    #[crdt(counter)]
    pub upvotes: i64,                               // PnCounter

    #[crdt(nested)]
    pub geometry: CardGeometry,                     // recurse into CardGeometry's schema

    #[crdt(skip)]
    pub hover: HoverState,                          // never synced — local UI state
}
```

Every field maps to one CRDT decision. `title` takes the scalar default (LWW). The rest are explicit.

---

## Layer 1 — what `derive(SchemaSync)` generates

For **each** annotated component it emits two things: a **schema struct** (the parallel type with CRDT fields) and an **`impl SchemaSync`**. Nested types are generated independently — `CardGeometry` first, since `Card`'s schema embeds it.

### 1a. The schema structs

```rust
// from CardGeometry
#[derive(Clone, Debug, Default, PartialEq, ::kyoso_crdt::DeriveCrdt)]
pub struct CardGeometrySchema {
    pub x:      ::kyoso_crdt::types::LwwRegister<f32>,
    pub y:      ::kyoso_crdt::types::LwwRegister<f32>,
    pub width:  ::kyoso_crdt::types::LwwRegister<f32>,
    pub height: ::kyoso_crdt::types::LwwRegister<f32>,
}

// from Card
#[derive(Clone, Debug, Default, PartialEq, ::kyoso_crdt::DeriveCrdt)]
pub struct CardSchema {
    pub title:        ::kyoso_crdt::types::LwwRegister<String>,
    pub labels:       ::kyoso_crdt::types::OrSet<String>,
    pub body:         ::kyoso_crdt::types::Sequence<char>,
    pub field_values: ::kyoso_crdt::types::CausalMap<::kyoso_crdt::types::LwwRegister<String>>,
    pub upvotes:      ::kyoso_crdt::types::PnCounter,
    pub geometry:     CardGeometrySchema,    // ← nested: the *other* type's Schema, embedded
    // `hover` is absent — #[crdt(skip)] omits it entirely
}
```

Two attribute mechanics worth pinning down:

- **`nested`** doesn't embed `CardGeometry`; it embeds `CardGeometrySchema` — the *other component's schema type*. The CRDT machinery composes because `CardGeometrySchema` is itself a `Crdt` (Layer 2).
- **`skip`** removes the field from the schema struct entirely. It cannot be addressed, replicated, or hydrated.
- **`map`**: `HashMap<String, String>` becomes `CausalMap<LwwRegister<String>>`. `CausalMap` keys are always `String`; the value must itself be a CRDT, so the bare `String` value is wrapped in `LwwRegister`.

Note `#[derive(... DeriveCrdt)]` on the schema struct — Layer 1 *plants* the Layer 2 derive. That's how the two macros chain.

### 1b. The `SchemaSync` impl — `changes_against`

This is where each attribute picks a different diff strategy. `current` is the doc's replicated state; `self` is the live component.

```rust
impl ::kyoso_sync::SchemaSync for Card {
    type Schema = CardSchema;
    const SCHEMA_NAME: &'static str = "Card";

    fn changes_against(&self, current: &CardSchema) -> Vec<CardSchemaMut> {
        let default = <Self as Default>::default();
        let mut out = Vec::new();

        // lww — echo-guarded scalar compare
        if *current.title.get().unwrap_or(&default.title) != self.title {
            out.push(CardSchemaMut::Title(LwwMut::Set(self.title.clone())));
        }

        // or_set — set-diff: Add what's new, Remove what's gone
        for e in &self.labels {
            if !current.labels.contains(e) {
                out.push(CardSchemaMut::Labels(OrSetMut::Add(e.clone())));
            }
        }
        for e in current.labels.iter() {
            if !self.labels.contains(e) {
                out.push(CardSchemaMut::Labels(OrSetMut::Remove(e.clone())));
            }
        }

        // sequence — prefix/suffix diff → InsertAt / DeleteAt spans
        let cur: String = current.body.iter().collect();
        if cur != self.body {
            let pre = common_prefix_len(&cur, &self.body);
            let suf = common_suffix_len(&cur, &self.body, pre);
            if cur.len() > pre + suf {
                out.push(CardSchemaMut::Body(SequenceMut::DeleteAt {
                    pos: pre, len: cur.len() - pre - suf,
                }));
            }
            for (i, ch) in self.body[pre..self.body.len() - suf].chars().enumerate() {
                out.push(CardSchemaMut::Body(SequenceMut::InsertAt { pos: pre + i, value: ch }));
            }
        }

        // map — per-key: Apply on changed/new keys, Remove on dropped keys
        for (k, v) in &self.field_values {
            let cur_v = current.field_values.get(k).and_then(|r| r.get());
            if cur_v != Some(v) {
                out.push(CardSchemaMut::FieldValues(MapMut::Apply {
                    key: k.clone(),
                    mutation: LwwMut::Set(v.clone()),
                }));
            }
        }
        for k in current.field_values.keys() {
            if !self.field_values.contains_key(k) {
                out.push(CardSchemaMut::FieldValues(MapMut::Remove { key: k.clone() }));
            }
        }

        // counter — diff against the resolved value, emit Inc/Dec
        let diff = self.upvotes - current.upvotes.value();
        if diff > 0 {
            out.push(CardSchemaMut::Upvotes(PnMut::Inc(diff as u64)));
        } else if diff < 0 {
            out.push(CardSchemaMut::Upvotes(PnMut::Dec((-diff) as u64)));
        }

        // nested — recurse: child's own changes_against, each mutation re-wrapped
        for m in self.geometry.changes_against(&current.geometry) {
            out.push(CardSchemaMut::Geometry(m));
        }

        // skip — `hover` produces nothing
        out
    }
```

The echo-guard generalises per CRDT: LWW uses `unwrap_or(&default)`; OrSet/Map compare membership (an empty collection emits nothing); the counter diffs against `value()` (bottom resolves to `0`); `nested` inherits its child's guard. In every case, *component-equals-default vs doc-at-bottom produces no op* — that's what stops reconnect echo.

### 1c. The `SchemaSync` impl — `write_back`

The reverse projection — unwrap each CRDT back into a plain field:

```rust
    fn write_back(&mut self, schema: &CardSchema) {
        if let Some(v) = schema.title.get() { self.title = v.clone(); }   // lww
        self.labels = schema.labels.iter().cloned().collect();            // or_set  → Vec
        self.body   = schema.body.iter().collect();                      // sequence → String
        self.field_values = schema.field_values.iter()                   // map → HashMap
            .filter_map(|(k, r)| r.get().map(|v| (k.clone(), v.clone())))
            .collect();
        self.upvotes = schema.upvotes.value();                           // counter → i64
        self.geometry.write_back(&schema.geometry);                      // nested → recurse
        // `hover` untouched — #[crdt(skip)]
    }
}
```

`get()` returning `None` (bottom) means "skip the write" for LWW — never clobber with a default. `skip` fields are simply never named, so local UI state survives a projection.

---

## Layer 2 — what `derive(Crdt)` generates from `CardSchema`

Now `derive(Crdt)` runs on the struct Layer 1 produced (`crates/kyoso_crdt_derive/src/lib.rs`). It makes `CardSchema` itself a CRDT — six items. **`CardSchemaMut` is the type `changes_against` returns**; that's the direct link between the layers.

```rust
// 1. CardSchemaMut — one variant per field, field name PascalCased,
//    each carrying that field type's Crdt::Mutation.
pub enum CardSchemaMut {
    Title(LwwMut<String>),
    Labels(OrSetMut<String>),
    Body(SequenceMut<char>),
    FieldValues(MapMut<LwwMut<String>>),
    Upvotes(PnMut),
    Geometry(CardGeometrySchemaMut),   // ← nested field's own generated Mut enum
}

// 2. CardSchemaDelta — same shape over Crdt::Delta
pub enum CardSchemaDelta {
    Title(LwwDelta<String>),
    Labels(OrSetDelta<String>),
    Body(SequenceDelta<char>),
    FieldValues(MapDelta<LwwDelta<String>>),
    Upvotes(PnDelta),
    Geometry(CardGeometrySchemaDelta),
}

// 3. impl Lattice for CardSchema — pointwise bottom() and join()
// 4. impl Crdt for CardSchema:
//      type Mutation = CardSchemaMut;  type Delta = CardSchemaDelta;
//      apply(delta)  — match variant, dispatch to that field's Crdt::apply
//      mutate(m)     — match variant, dispatch to that field's Crdt::mutate → Delta
// 5. impl SchemaApply for CardSchema:
//      apply_wire(path, wire)   — match path head ("upvotes", "geometry", …),
//                                 recurse into the field with the tail
//      install_state(path, val) — same dispatch, for snapshot hydration
// 6. impl IntoWireOp for CardSchemaDelta:
//      into_wire_op() — recurse into the leaf delta, prepend this field's name
```

The recursion is the whole trick. `CardSchemaMut::Geometry` carries `CardGeometrySchemaMut` — produced by `derive(Crdt)` running *separately* on `CardGeometrySchema`. So `apply_wire(["geometry","x"], …)` on `CardSchema` matches `"geometry"`, hands `["x"]` to `CardGeometrySchema`'s generated `apply_wire`, which matches `"x"` and hands `[]` to `LwwRegister<f32>`'s hand-written `SchemaApply`. Leaf CRDTs bottom out the recursion; nested schemas extend it. `IntoWireOp` builds the path back up the same way.

After Layer 2, `CardSchema` satisfies `Crdt + SchemaApply` — exactly the `SchemaSync::Schema` bound (`schema_sync.rs:106`).

---

## Layer 3 — how the plugins use it

You register **only the top-level component**. `CardGeometry` is synced *through* `Card`'s schema, so it gets no plugin of its own:

```rust
app.add_plugins(SchemaSyncedNodeComponentPlugin::<MyNode, MyEdge, Card>::default());
```

That plugin (`schema_sync.rs:487`) inits `SchemaDoc<CardSchema>`, registers a hydrator, and chains four systems. Here is what each does with the generated code, traced through one edit — the user runs `card.upvotes += 1`:

**1. `detect_typed_changes::<NodeTarget, Card>`** (`schema_sync.rs:323`) — `Changed<Card>` fires.
- Calls `card.changes_against(current)` → `[CardSchemaMut::Upvotes(PnMut::Inc(1))]` *(Layer 1c logic, Layer 2 enum)*.
- Reconnect guard: `card.is_added() && *current != empty_schema` — `PartialEq` here is the user-derived one on `CardSchema`.
- For the mutation: `throwaway.mutate(m, &mut ctx)` → generated `Crdt::mutate` dispatches to `PnCounter::mutate` → `CardSchemaDelta::Upvotes(PnDelta { by: 1 })`.
- `delta.into_wire_op()` → generated `IntoWireOp` → `(["upvotes"], WireDelta)`. The driver prepends `SCHEMA_NAME` → path `["Card", "upvotes"]`, wraps in `OpKind::SetNodeProperty`, enqueues on the engine.

**2. `route_typed_inbound::<NodeTarget, Card>`** (`schema_sync.rs:385`) — a remote op arrives, path `["Card", "upvotes"]`.
- Head segment matches `Card::SCHEMA_NAME`; strips it to `["upvotes"]`.
- `doc.apply_property_op(stripped)` → generated `SchemaApply::apply_wire(["upvotes"], wire)` → matches the `upvotes` arm → `PnCounter::apply`. State merges via the generated `Lattice`.

**3. `project_typed_to_bevy::<NodeTarget, Card>`** (`schema_sync.rs:417`) — `doc.is_changed()` is true.
- For each replicated entity: `card.write_back(schema)` *(Layer 1c)* → `card.upvotes = schema.upvotes.value()`, and `card.geometry.write_back(&schema.geometry)` recurses.
- If the component isn't on the entity yet, `InsertSchemaProjected` inserts `Card::default()` then writes back.

**4. Snapshot hydration** — at `Welcome`, `hydrate_schema_doc::<Card>` (`schema_sync.rs:287`) calls generated `SchemaApply::install_state(path, OpaqueValue)` to install post-merge state field by field — same head-dispatch as `apply_wire`, no delta replay.

A nested edit (`card.geometry.x = 10.0`) flows identically, the path just one segment deeper: `changes_against` recurses → `CardSchemaMut::Geometry(CardGeometrySchemaMut::X(LwwMut::Set(10.0)))` → `into_wire_op` builds `["geometry","x"]` → driver prepends → `["Card","geometry","x"]`.
