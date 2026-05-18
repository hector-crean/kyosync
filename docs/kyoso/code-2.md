This is a two-stage derive pipeline feeding a model-agnostic runtime plugin. The full stack is:

1. **`SchemaSync` derive** (in `kyoso_sync_derive`) — emits a *companion struct* `…Schema` alongside the Bevy component, stamps `#[derive(::kyoso_crdt::DeriveCrdt)]` onto it, and emits the `SchemaSync` + `SchemaField` impls that bridge Bevy ⇄ schema.
2. **`DeriveCrdt` derive** (`kyoso_crdt_derive`, re-exported from `kyoso_crdt` as `DeriveCrdt`; the underlying macro is `#[proc_macro_derive(Crdt)]`) — the compiler picks up the stamped attribute and emits the `…SchemaMut`/`…SchemaDelta` enums, plus `Lattice`/`Crdt`/`SchemaApply`/`IntoWireOp` impls on the companion struct.
3. **`SchemaSyncedComponentPlugin`** (in `kyoso_sync`) — a Bevy plugin generic over `(SchemaTarget, SchemaSync)` that schedules four systems to drive the diff → wire → apply → writeback cycle, with `SchemaTarget` as the seam that binds the pipeline to a particular CRDT model (graph nodes, graph edges, …).

So `SchemaSync` never calls `DeriveCrdt` directly — it *generates source text containing a derive attribute*, and the compiler's normal expansion loop picks it up. That's the "transitive" link. The runtime plugin in turn never names the macro-generated types directly — it works through the traits the two derives populate (`SchemaSync`, `Crdt`, `SchemaApply`, `IntoWireOp`).

---

## The running example

```rust
// crates/kyoso_figma/src/text.rs
#[derive(Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Text")]
pub struct Text {
    #[crdt(sequence)]  pub content: String,      // RGA sequence-of-char
    #[crdt(nested)]    pub style:   TypeStyle,   // schema-in-schema
    pub fills: Vec<Paint>,                       // no #[crdt(...)] ⇒ whole-list LWW
}
```

Three different `#[crdt(...)]` modes in one struct: `sequence`, `nested`, and the implicit LWW default. `TypeStyle` (`crates/kyoso_figma/src/typestyle.rs:12`) itself derives `SchemaSync` with four plain-LWW fields, so it slots into `Text` as a nested CRDT.

---

## Stage 1 — what `#[derive(SchemaSync)]` emits

The macro (`crates/kyoso_sync_derive/src/lib.rs:255-310`) produces three items.

### 1a. The companion schema struct

```rust
#[derive(
    ::core::clone::Clone,
    ::core::fmt::Debug,
    ::core::default::Default,
    ::core::cmp::PartialEq,
    ::kyoso_crdt::DeriveCrdt,        // ◀── THIS is the transitive trigger
)]
pub struct TextSchema {
    pub content: ::kyoso_crdt::types::Sequence<char>,
    pub style:   <TypeStyle as ::kyoso_sync::SchemaSync>::Schema,
    pub fills:   ::kyoso_crdt::types::LwwRegister<Vec<Paint>>,
}
```

Each Bevy field type `T` is wrapped per its CRDT kind:
- LWW → `LwwRegister<T>`
- `sequence` over `String` → `Sequence<char>`; over `Vec<T>` → `Sequence<T>`
- `nested` → `<T as SchemaSync>::Schema` (the field's own companion struct, recursively)

The field-kind table (`kyoso_sync_derive/src/lib.rs:182-227`) is the *only* place the `#[crdt(...)]` attribute matters; it picks the wrapper type and nothing else.

### 1b. `impl SchemaSync for Text`

```rust
impl ::kyoso_sync::SchemaSync for Text {
    type Schema = TextSchema;
    const SCHEMA_NAME: &'static str = "Text";   // from #[schema(name = "Text")]

    fn diff(&self, doc: &Self::Schema) -> ::kyoso_sync::SchemaMutations<Self> {
        #[allow(unused_variables)]
        let default = <Self as ::core::default::Default>::default();
        let mut out = ::std::vec::Vec::new();

        // One uniform arm per field — delegates to that wrapper type's
        // SchemaField impl. `&default.content` is the LWW echo-guard
        // baseline (Sequence ignores it).
        out.extend(
            <::kyoso_crdt::types::Sequence<char>
                as ::kyoso_sync::SchemaField<String>>::diff(
                &doc.content, &self.content, &default.content,
            )
            .into_iter()
            .map(TextSchemaMut::Content),     // ◀── variant from Stage 2's enum
        );
        out.extend(
            <<TypeStyle as ::kyoso_sync::SchemaSync>::Schema
                as ::kyoso_sync::SchemaField<TypeStyle>>::diff(
                &doc.style, &self.style, &default.style,
            )
            .into_iter()
            .map(TextSchemaMut::Style),
        );
        out.extend(
            <::kyoso_crdt::types::LwwRegister<Vec<Paint>>
                as ::kyoso_sync::SchemaField<Vec<Paint>>>::diff(
                &doc.fills, &self.fills, &default.fills,
            )
            .into_iter()
            .map(TextSchemaMut::Fills),
        );
        out
    }

    fn write_back(&mut self, schema: &Self::Schema) {
        <::kyoso_crdt::types::Sequence<char>
            as ::kyoso_sync::SchemaField<String>>::project_to(
            &schema.content, &mut self.content,
        );
        <<TypeStyle as ::kyoso_sync::SchemaSync>::Schema
            as ::kyoso_sync::SchemaField<TypeStyle>>::project_to(
            &schema.style, &mut self.style,
        );
        <::kyoso_crdt::types::LwwRegister<Vec<Paint>>
            as ::kyoso_sync::SchemaField<Vec<Paint>>>::project_to(
            &schema.fills, &mut self.fills,
        );
    }
}
```

The per-field diff/projection logic isn't inlined here — it lives in the `SchemaField` impls in `crates/kyoso_sync/src/schema.rs:93-260`. The macro just emits *one delegation per field*. The LWW echo-guard (`schema.rs:99-107`) — "an empty register resolves to `baseline`, so a freshly-defaulted component emits nothing" — is what `&default.fills` feeds.

### 1c. `impl SchemaField<Text> for TextSchema` — the nesting hook

```rust
impl ::kyoso_sync::SchemaField<Text> for TextSchema {
    fn diff(&self, component: &Text, _baseline: &Text)
        -> ::std::vec::Vec<<Self as ::kyoso_crdt::Crdt>::Mutation>
    {
        <Text as ::kyoso_sync::SchemaSync>::diff(component, self)
    }
    fn project_to(&self, component: &mut Text) {
        <Text as ::kyoso_sync::SchemaSync>::write_back(component, self)
    }
}
```

This is what makes `#[crdt(nested)]` work: `Text`'s `style: TypeStyle` field uses `TypeStyleSchema` as its wrapper, and `TypeStyleSchema` (emitted by `derive(SchemaSync)` on `TypeStyle`) has exactly this impl. So in 1b's diff arms, `<TypeStyleSchema as SchemaField<TypeStyle>>::diff` resolves to a recursive call into `TypeStyle`'s own `SchemaSync::diff` — `TypeStyle` looks like just another CRDT field to `Text`.

---

## Stage 2 — what `DeriveCrdt` emits on `TextSchema`

Now the compiler processes the `#[derive(::kyoso_crdt::DeriveCrdt)]` that Stage 1 stamped on `TextSchema`. `crates/kyoso_crdt_derive/src/lib.rs:33-213` walks `TextSchema`'s three fields and emits **five items**.

### 2a. The mutation enum (referenced by Stage 1b's `.map(TextSchemaMut::Content)`)

```rust
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::pub_enum_variant_names, dead_code)]
pub enum TextSchemaMut {
    Content(<::kyoso_crdt::types::Sequence<char> as ::kyoso_crdt::Crdt>::Mutation),
    Style(<<TypeStyle as ::kyoso_sync::SchemaSync>::Schema as ::kyoso_crdt::Crdt>::Mutation),
    Fills(<::kyoso_crdt::types::LwwRegister<Vec<Paint>> as ::kyoso_crdt::Crdt>::Mutation),
}
// e.g. <LwwRegister<Vec<Paint>> as Crdt>::Mutation resolves to LwwMut<Vec<Paint>> (= Set(value))
//      <Sequence<char>      as Crdt>::Mutation resolves to SequenceMut<char> (Insert/Delete)
```

Variant names are PascalCase'd field idents (`stroke_weight` → `StrokeWeight`; `font_family` → `FontFamily`). This is the bridge: Stage 1b emits `TextSchemaMut::Content` *forward-referencing* an enum Stage 2 hasn't created yet — both passes finish before type-checking, so it resolves.

### 2b. The delta enum (the on-wire shape)

```rust
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::pub_enum_variant_names, dead_code)]
pub enum TextSchemaDelta {
    Content(<::kyoso_crdt::types::Sequence<char> as ::kyoso_crdt::Crdt>::Delta),
    Style(<<TypeStyle as ::kyoso_sync::SchemaSync>::Schema as ::kyoso_crdt::Crdt>::Delta),
    Fills(<::kyoso_crdt::types::LwwRegister<Vec<Paint>> as ::kyoso_crdt::Crdt>::Delta),
}
```

### 2c. `impl Lattice` — pointwise join

```rust
impl ::kyoso_crdt::Lattice for TextSchema {
    fn bottom() -> Self {
        Self {
            content: <::kyoso_crdt::types::Sequence<char>            as ::kyoso_crdt::Lattice>::bottom(),
            style:   <<TypeStyle as ::kyoso_sync::SchemaSync>::Schema as ::kyoso_crdt::Lattice>::bottom(),
            fills:   <::kyoso_crdt::types::LwwRegister<Vec<Paint>>    as ::kyoso_crdt::Lattice>::bottom(),
        }
    }
    fn join(&mut self, other: Self) {
        ::kyoso_crdt::Lattice::join(&mut self.content, other.content);
        ::kyoso_crdt::Lattice::join(&mut self.style,   other.style);
        ::kyoso_crdt::Lattice::join(&mut self.fills,   other.fills);
    }
}
```

### 2d. `impl Crdt` — typed apply / mutate dispatch

```rust
impl ::kyoso_crdt::Crdt for TextSchema {
    type Mutation = TextSchemaMut;
    type Delta    = TextSchemaDelta;

    fn apply(&mut self, delta: &Self::Delta, ctx: &::kyoso_crdt::CausalContext)
        -> ::core::result::Result<(), ::kyoso_crdt::DeltaError>
    {
        match delta {
            TextSchemaDelta::Content(d) => ::kyoso_crdt::Crdt::apply(&mut self.content, d, ctx),
            TextSchemaDelta::Style(d)   => ::kyoso_crdt::Crdt::apply(&mut self.style,   d, ctx),
            TextSchemaDelta::Fills(d)   => ::kyoso_crdt::Crdt::apply(&mut self.fills,   d, ctx),
        }
    }

    fn mutate(&mut self, m: Self::Mutation, ctx: &mut ::kyoso_crdt::CausalContext) -> Self::Delta {
        match m {
            TextSchemaMut::Content(m) =>
                TextSchemaDelta::Content(::kyoso_crdt::Crdt::mutate(&mut self.content, m, ctx)),
            TextSchemaMut::Style(m) =>
                TextSchemaDelta::Style(::kyoso_crdt::Crdt::mutate(&mut self.style, m, ctx)),
            TextSchemaMut::Fills(m) =>
                TextSchemaDelta::Fills(::kyoso_crdt::Crdt::mutate(&mut self.fills, m, ctx)),
        }
    }
}
```

### 2e. `impl SchemaApply` — wire-driven, path-string dispatch (inbound)

```rust
impl ::kyoso_crdt::SchemaApply for TextSchema {
    fn apply_wire(&mut self, path: &::kyoso_crdt::Path, delta: ::kyoso_crdt::WireDelta,
                  ctx: &::kyoso_crdt::CausalContext)
        -> ::core::result::Result<(), ::kyoso_crdt::DeltaError>
    {
        let (head, tail) = ::kyoso_crdt::schema::split_field_head(path)?;
        match head {
            "content" => ::kyoso_crdt::SchemaApply::apply_wire(&mut self.content, &tail, delta, ctx),
            "style"   => ::kyoso_crdt::SchemaApply::apply_wire(&mut self.style,   &tail, delta, ctx),
            "fills"   => ::kyoso_crdt::SchemaApply::apply_wire(&mut self.fills,   &tail, delta, ctx),
            other => Err(::kyoso_crdt::DeltaError::UnknownPath { segment: other.to_string() }),
        }
    }
    fn install_state(&mut self, path: &::kyoso_crdt::Path, field: ::kyoso_crdt::OpaqueValue)
        -> ::core::result::Result<(), ::kyoso_crdt::DeltaError>
    { /* same head/tail dispatch, snapshot-hydration path */ }
}
```

Note `apply_wire` matches the **field name string** (`"content"`), whereas `apply` (2d) matches the **typed enum variant**. `SchemaApply` is the untyped wire entry point; `Crdt::apply` is the typed one. For leaf CRDTs (`Sequence`, `LwwRegister`) the `tail` is empty and they consume the delta directly; for the nested `style` field the tail is non-empty and recursion continues into `TypeStyleSchema::apply_wire`.

### 2f. `impl IntoWireOp` — delta → `(Path, WireDelta)` (outbound)

```rust
impl ::kyoso_crdt::IntoWireOp for TextSchemaDelta {
    fn into_wire_op(self) -> (::kyoso_crdt::Path, ::kyoso_crdt::WireDelta) {
        match self {
            TextSchemaDelta::Content(d) => {
                let (inner, wire) = ::kyoso_crdt::IntoWireOp::into_wire_op(d);
                let mut path = ::kyoso_crdt::Path::field("content");   // prepend our field name
                for seg in inner.0 { path.0.push(seg); }                // recurse for nested
                (path, wire)
            }
            TextSchemaDelta::Style(d) => { /* same shape, prepends "style" */ }
            TextSchemaDelta::Fills(d) => { /* same shape, prepends "fills" */ }
        }
    }
}
```

For a leaf like `LwwRegister<Vec<Paint>>` or `Sequence<char>`, `inner` is empty so the path is just `["fills"]` / `["content"]`. For the `nested` `style` field, the inner path is itself non-empty (e.g. `["font_size"]`) so the final path is `["style", "font_size"]` — multi-segment.

---

## Picking the wrapper: how `#[crdt(...)]` annotations map

Only Stage **1a** (the wrapper type) changes per annotation — everything downstream is uniform because it all goes through `SchemaField` / `Crdt`:

| `#[crdt(...)]` on a field `T` | Stage 1a wrapper type | Stage 2 `::Mutation` |
|---|---|---|
| *(none)* / `lww` | `LwwRegister<T>` | `LwwMut<T>` |
| `or_set` (T = `Vec<U>`/`HashSet<U>`/`BTreeSet<U>`) | `OrSet<U>` | `OrSetMut<U>` |
| `counter` (T = integer) | `PnCounter` | `PnMut` |
| `map` (T = `HashMap<String,V>`) | `CausalMap<LwwRegister<V>>` | `MapMut<LwwMut<V>>` |
| `sequence` (T = `String`/`Vec<U>`) | `Sequence<char>` / `Sequence<U>` | `SequenceMut<…>` |
| `nested` (T: `SchemaSync`) | `<T as SchemaSync>::Schema` | that schema's `…SchemaMut` |
| `with = "Type"` | `Type` (hand-written `SchemaField` impl) | `<Type as Crdt>::Mutation` |
| `skip` | *(field omitted entirely)* | — |

The full table is also documented at `kyoso_sync_derive/src/lib.rs:43-51`. Worked through `Text` field-by-field:

| Field | Attribute | Wrapper | Mutation | Convergence semantics |
|---|---|---|---|---|
| `content` | `#[crdt(sequence)]` | `Sequence<char>` | `SequenceMut<char>` | RGA — concurrent inserts interleave deterministically |
| `style` | `#[crdt(nested)]` | `TypeStyleSchema` | `TypeStyleSchemaMut` | per-field merge of the inner struct |
| `fills` | *(none)* | `LwwRegister<Vec<Paint>>` | `LwwMut<Vec<Paint>>` | whole-list LWW |

`nested` is the interesting case because it bottoms out at another `SchemaSync` derive — which means the wrapper type, the mutation type, *and* the recursive wire-path stitching all just fall out of the same machinery. There is no special code path for nesting; it composes.

---

## The runtime — `SchemaSyncedComponentPlugin`

Macro output gives you the types and the per-field bridges. The plugin gives them a runtime: it owns the replicated state, schedules the systems that drive the cycle, and abstracts over the CRDT model via a trait seam.

`crates/kyoso_sync/src/component_sync.rs:496`. Generic over two type params:

- `C: SchemaSync` — the component to sync (e.g. `Text`).
- `X: SchemaTarget` — the **model seam** (more on this below). Today: `NodeTarget` or `EdgeTarget` from `kyoso_graph_sync`.

`Plugin::build`:

```rust
fn build(&self, app: &mut App) {
    app.init_resource::<SchemaDoc<C::Schema>>();
    app.init_resource::<SchemaHydrators>();
    app.world_mut()
        .resource_mut::<SchemaHydrators>()
        .register::<X, C>();
    app.add_systems(
        Update,
        (
            ensure_schema_slots::<X, C>,
            detect_local_changes::<X, C>,
            apply_remote_changes::<X, C>,
            project_to_components::<X, C>,
        )
            .chain()
            .after(SyncSet::Structural)
            .before(SyncSet::Outbound),
    );
}
```

Two resources, four systems, one chain, one ordering constraint.

### What it owns

- **`SchemaDoc<C::Schema>`** (`component_sync.rs:138`) — a `HashMap<CrdtId, C::Schema>` plus a `CausalState`. The per-id replicated schema state. Only mutated by server-confirmed property ops (via `apply_property` at `component_sync.rs:183`); never pre-applied from local edits. Local edits reach it on the server echo.
- **`SchemaHydrators`** (`component_sync.rs:211`) — a `(TargetKind, schema_name) → HydratorFn` registry for snapshot install. Each `SchemaSyncedComponentPlugin<X, C>` registers one entry at app build.

### Scheduling

```
SyncSet::Structural          ← model populates X::Index (Entity ↔ CrdtId)
   │
   ▼
ensure_schema_slots          ── default SchemaDoc slot per bound entity
detect_local_changes         ── outbound: Changed<C> → diff → wire ops
apply_remote_changes         ── inbound:  X::Inbound message → SchemaDoc
project_to_components        ── writeback: SchemaDoc → Bevy Mut<C>
   │
   ▼
SyncSet::Outbound            ← model drains queued ops to the transport
```

The plugin doesn't name any model-specific systems — only the two `SyncSet` phases. That's what makes it model-agnostic.

### The model seam: `SchemaTarget`

`crates/kyoso_sync/src/component_sync.rs:93`. A trait with three associated resources/messages and a few accessors:

```rust
pub trait SchemaTarget: 'static + Send + Sync {
    const KIND: TargetKind;
    type Index: Resource;        // Entity ↔ CrdtId map
    type Outbound: Resource;     // local op queue
    type Inbound: Message;       // inbound server-confirmed op

    fn id_for(index: &Self::Index, entity: Entity) -> Option<CrdtId>;
    fn pairs(index: &Self::Index) -> &HashMap<Entity, CrdtId>;
    fn mint_id(out: &mut Self::Outbound) -> CrdtId;
    fn enqueue(out: &mut Self::Outbound, op_id: CrdtId, target: CrdtId,
               path: Path, delta: WireDelta);
    fn match_inbound(event: &Self::Inbound) -> Option<InboundProperty<'_>>;
}
```

`NodeTarget` and `EdgeTarget` (in `kyoso_graph_sync`) bridge the graph model's `NodeId`/`EdgeId`-flavoured op envelopes to the universal `(CrdtId, Path, WireDelta)` triple. A non-graph model (e.g. a flat object-set, a singleton) would add a third implementor and reuse the plugin unchanged.

---

## The four systems

### `ensure_schema_slots`

`component_sync.rs:272`. For every entity that has both component `C` *and* an `X::Index` binding, ensures a default schema slot exists in `SchemaDoc`. Uses `doc.bypass_change_detection()` — otherwise the deref-mut would flip `is_changed()` every frame and force `project_to_components` to redundantly write back to every bound entity, which would trip `Changed<C>` and create a frame-loop.

### `detect_local_changes` — outbound encoder

`component_sync.rs:297`. Query: `Query<(Entity, Ref<C>), Changed<C>>`. For each changed component:

```rust
let mutations = component.diff(current);                // SchemaSync::diff (stage 1b)
for mutation in mutations {
    let op_id = X::mint_id(&mut out);
    let mut ctx = CausalContext::new(op_id, None, &mut state);
    let typed_delta = throwaway.mutate(mutation, &mut ctx);   // Crdt::mutate (stage 2d)
    let (inner_path, wire) = typed_delta.into_wire_op();      // IntoWireOp (stage 2f)
    let mut path = Path::field(C::SCHEMA_NAME);               // prepend "Text"
    for seg in inner_path.0 { path.0.push(seg); }
    X::enqueue(&mut out, op_id, id, path, wire);              // model-side enqueue
}
```

Note the **reconnect echo-guard** at lines 318-333: a snapshot-hydrated entity is `is_added()` AND the doc already holds non-default state — those are skipped. A user-spawned entity is *also* `is_added()`, but the doc is still at lattice bottom, so its initial values do replicate. Without this guard, on-add observers that auto-insert default components (`Transform::default()` etc.) would trip a diff that resets every field to the local default and clobber canonical state on other peers.

The `throwaway` clone is so the local `mutate` walks the CRDT state forward to produce the wire delta without committing to the live `SchemaDoc` — that update happens only on the server echo.

### `apply_remote_changes` — inbound router

`component_sync.rs:361`. Filters the inbound stream:

```rust
for event in events.read() {
    let Some(prop) = X::match_inbound(event) else { continue };
    let Some(PathSegment::Field(head)) = prop.path.0.first() else { continue };
    if head != C::SCHEMA_NAME { continue }                  // skip other components
    let inner_path = Path(prop.path.0[1..].to_vec());        // strip schema name
    doc.apply_property(prop.op_id, prop.seq, prop.target,
                       &inner_path, prop.delta.clone())?;
}
```

`SchemaDoc::apply_property` (`component_sync.rs:183`) wraps the call in a `CausalContext` and dispatches to the generated `SchemaApply::apply_wire` (stage 2e), which walks the path head-by-head until it lands on a leaf CRDT and applies the typed delta.

### `project_to_components` — writeback

`component_sync.rs:402`. Bails fast when `doc.is_changed() == false`. Otherwise iterates `X::pairs()` and calls `component.write_back(schema)` (stage 1b `SchemaSync::write_back`) per entity. If the component doesn't exist on the entity yet — typical of snapshot hydration: the structural marker spawns the entity but the component hasn't been inserted yet — falls back to queueing an `InsertSchemaProjected` command that inserts `C::default()` and then writes back.

---

## End-to-end: editing `text.content` on peer A → peer B

Tracing one local character insert all the way through both halves of the pipeline:

```
Peer A                                              Peer B
──────                                              ──────
1. text.content.push('!')
   (some Bevy system mutates the component)

2. detect_local_changes (component_sync.rs:297):
     Text::diff(&doc.schema) returns
       vec![ TextSchemaMut::Content(SequenceMut::Insert{..}) ]
     (via SchemaField<String> for Sequence<char>,
      schema.rs:237, which calls sequence_diff)

3. TextSchema::mutate(mut, &mut ctx)              ← Stage 2d
     → TextSchemaDelta::Content(<sequence delta>) ← Stage 2b

4. .into_wire_op()                                 ← Stage 2f
     → (Path["content"],
        WireDelta::SequenceInsert { predecessor, value })
     (RGA position lives in `predecessor`, not the path)

5. Prepend SCHEMA_NAME (component_sync.rs:352):
     Path["Text", "content"]

6. NodeTarget::enqueue(out, op_id, id,
                       Path["Text","content"], wire)
     → pushed onto the graph model's outbound queue

7. SyncSet::Outbound drains:
     queue → graph Op<OpKind::SetNodeProperty{..}>
     → EnvelopeClientMsg::Submit { model, payload }
     (postcard-encoded by WsClient)

8.   ───────── WebSocket frame ─────────►
                                                    9. SyncTransportPlugin (PreUpdate)
                                                       drains the WsClient channel,
                                                       emits WsInbound event

                                                    10. GraphSyncPlugin matches the
                                                        model id, decodes Op<OpKind>,
                                                        re-emits NodeTarget::Inbound

                                                    11. apply_remote_changes
                                                        (component_sync.rs:361):
                                                          match_inbound → InboundProperty
                                                          head == "Text" → strip
                                                          SchemaDoc::apply_property
                                                          → SchemaApply::apply_wire
                                                            (Stage 2e):
                                                              match "content"
                                                              → self.content.apply_wire(
                                                                  &[], delta, ctx)
                                                              → Sequence::apply
                                                            → text_schema.content updated

                                                    12. project_to_components
                                                        (component_sync.rs:402):
                                                          doc.is_changed() == true
                                                          for (entity, id) in pairs:
                                                            Text::write_back(&schema)  ← Stage 1b
                                                            → component.content =
                                                              schema.content.iter()
                                                              .collect::<String>()
```

For the `#[crdt(nested)]` `style.font_size` case only steps 2-5 differ: `Text::diff` recurses through `SchemaField<TypeStyle> for TypeStyleSchema` (stage 1c) and yields `TextSchemaMut::Style(TypeStyleSchemaMut::FontSize(LwwMut::Set(...)))`; `into_wire_op` recursively prepends two path segments; step 5 prepends `"Text"`; the final path is `Path["Text", "style", "font_size"]`. Stage 2e dispatch on peer B walks two heads instead of one. Everything else — transport, scheduling, writeback — is identical.

For the implicit-LWW `fills` case the path is just `Path["Text", "fills"]` and the wire delta is `WireDelta::LwwReplace { value }`. Same shape, same systems.

---

## Snapshot / hydration

`detect_local_changes` and `apply_remote_changes` handle the *steady-state* delta cycle. When a peer first joins a room, the server replays current state as snapshots — one `OpaqueValue` per primitive CRDT slot (`crates/kyoso_crdt/src/opaque.rs:60`). The graph crate's snapshot handler clones the `SchemaHydrators` table (`component_sync.rs:236`) and, for each snapshot field, looks up the hydrator by `(TargetKind, schema_name)` and calls it with `&mut World`. The hydrator (`hydrate_schema_doc` at `component_sync.rs:253`) resolves `SchemaDoc<C::Schema>`, ensures the slot for `target`, and calls `SchemaApply::install_state` — the snapshot variant of stage 2e. The very next frame's `project_to_components` writes those hydrated values onto Bevy components (creating them via `InsertSchemaProjected` if necessary).

The `Welcome` path (for joiners arriving fresh enough to be served deltas rather than a full snapshot) is just `apply_remote_changes` again — the server ships the recent op tail and the live delta cycle handles it. The integration test `derived_schema_replicates_all_fields` (`crates/kyoso_graph_sync/tests/derived_schema.rs:113`) is the canonical end-to-end exercise; `late_joiner_receives_typed_schema_properties_via_welcome` (line 226) covers the welcome path specifically.

---

## Plugin assembly in a host app

`crates/kyoso_circuit/src/plugin.rs:28`:

```rust
impl Plugin for KyosoCircuitPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            SyncTransportPlugin::new(self.server_url.clone(), self.room.clone()),
            GraphSyncPlugin::default(),
            SchemaSyncedComponentPlugin::<NodeTarget, Resistor>::default(),
            SchemaSyncedComponentPlugin::<NodeTarget, Capacitor>::default(),
            SchemaSyncedComponentPlugin::<NodeTarget, Inductor>::default(),
            SchemaSyncedComponentPlugin::<NodeTarget, VoltageSource>::default(),
            SchemaSyncedComponentPlugin::<NodeTarget, Ground>::default(),
        ));
        app.add_plugins((
            SchemaSyncedComponentPlugin::<NodeTarget, Transform>::default(),
            SchemaSyncedComponentPlugin::<NodeTarget, OnLayer>::default(),
        ));
    }
}
```

One `SchemaSyncedComponentPlugin<NodeTarget, C>` per component type. The plugin re-export comes through `kyoso_graph_sync` for convenience, but the type itself is defined in `kyoso_sync`. To sync a field of an edge-attached component instead, swap `NodeTarget` for `EdgeTarget`. To sync against a non-graph model, supply a third `SchemaTarget` impl — the plugin code itself doesn't change.

---

## Responsibility split

| Layer | Owns | Lives in |
|---|---|---|
| `SchemaSync` derive | Bevy-component ⇄ schema-struct bridge (`diff`/`write_back`), the `SchemaField<Comp>` nesting hook, **and emitting the `DeriveCrdt` attribute** | `kyoso_sync_derive` |
| `DeriveCrdt` (`Crdt`) derive | Pure CRDT machinery on the schema struct: `Mut`/`Delta` enums, `Lattice`, `Crdt`, `SchemaApply`, `IntoWireOp` | `kyoso_crdt_derive` |
| `SchemaField<T>` impls | Per-CRDT-primitive diff/projection (incl. the LWW echo-guard) | `kyoso_sync` (`schema.rs`) |
| `SchemaSyncedComponentPlugin<X, C>` | Per-`(target, component)` runtime pipeline + 4 systems + `SchemaDoc` + `SchemaHydrators` | `kyoso_sync` (`component_sync.rs`) |
| `SchemaTarget` (model seam) | Binds the pipeline to a model: identity, outbound queue, inbound stream | `kyoso_graph_sync` (`NodeTarget`/`EdgeTarget`) and future model crates |
| `SyncTransportPlugin` / `WsClient` | Postcard-encoded ops over WebSocket | `kyoso_sync` (`transport.rs`, `client.rs`) |

`SchemaSync` knows nothing about lattices or wire format; `DeriveCrdt` knows nothing about Bevy; `SchemaSyncedComponentPlugin` knows nothing about graphs; `SchemaTarget` knows nothing about CRDTs. The generated `TextSchema` struct is the only thing every layer touches.
