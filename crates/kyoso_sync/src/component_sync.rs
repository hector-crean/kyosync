//! Model-agnostic typed-component sync pipeline.
//!
//! Given a Bevy component `C: SchemaSync` and a *target provider*
//! `X: SchemaTarget`, this module replicates `C`'s fields across peers:
//!
//! - **Outbound** ([`detect_local_changes`]): on `Changed<C>`, diff the
//!   component against the replicated [`SchemaDoc`] state and enqueue one
//!   wire op per changed field.
//! - **Inbound** ([`apply_remote_changes`]): apply server-confirmed
//!   property ops back into the [`SchemaDoc`].
//! - **Projection** ([`project_to_components`]): write replicated state
//!   back onto the Bevy component.
//!
//! Nothing here names a graph type. `X: SchemaTarget` is the seam: it
//! supplies the identity index (`Entity ↔ CrdtId`), the outbound op
//! queue, and the inbound op stream — all as associated types — and
//! bridges them to the universal `(CrdtId, Path, WireDelta)` property-op
//! shape. The graph crate (`kyoso_graph_sync`) provides `NodeTarget` /
//! `EdgeTarget`; a non-graph model would provide its own `SchemaTarget`
//! and reuse this pipeline unchanged.

use std::collections::HashMap;
use std::marker::PhantomData;

use bevy::prelude::*;
use kyoso_crdt::{
    CausalContext, CausalState, Crdt, CrdtId, DeltaError, GlobalSeq, IntoWireOp, OpaqueValue, Path,
    PathSegment, SchemaApply, WireDelta,
};

use crate::schema::SchemaSync;

// ---------------------------------------------------------------------------
// System scheduling
// ---------------------------------------------------------------------------

/// Pipeline phases shared by every sync model.
///
/// A model plugin places its structural detection (which establishes the
/// `Entity ↔ CrdtId` identity binding) in [`SyncSet::Structural`] and its
/// outbound drain in [`SyncSet::Outbound`]. [`SchemaSyncedComponentPlugin`]
/// schedules itself strictly between the two — so it never names the
/// model's own systems, and carries no model type parameters.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyncSet {
    /// Inbound ops applied + local identity/topology changes detected.
    /// The identity index is fully populated by the end of this set.
    Structural,
    /// Locally-queued ops drained to the transport.
    Outbound,
}

// ---------------------------------------------------------------------------
// SchemaTarget — the model seam
// ---------------------------------------------------------------------------

/// Runtime discriminant for [`SchemaTarget`]. Keys per-kind hydrators in
/// [`SchemaHydrators`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TargetKind {
    Node,
    Edge,
}

/// One inbound property op, extracted from a model's applied-op message.
pub struct InboundProperty<'a> {
    /// Op id — feeds the causal context for the apply.
    pub op_id: CrdtId,
    /// Server-assigned sequence, if confirmed.
    pub seq: Option<GlobalSeq>,
    /// Entity id the property belongs to.
    pub target: CrdtId,
    /// Schema-namespaced path (head segment = `SCHEMA_NAME`).
    pub path: &'a Path,
    /// The wire delta to apply.
    pub delta: &'a WireDelta,
}

/// The seam between the generic component-sync pipeline and a concrete
/// CRDT model.
///
/// An implementor binds the pipeline to a model by naming three
/// resources/messages and bridging them to the universal
/// `(CrdtId, Path, WireDelta)` property-op shape:
///
/// - [`Index`](Self::Index) — resource holding the `Entity ↔ CrdtId` map.
/// - [`Outbound`](Self::Outbound) — resource the pipeline queues ops into.
/// - [`Inbound`](Self::Inbound) — message carrying server-confirmed ops.
///
/// `kyoso_graph_sync` implements this for graph nodes and reference
/// edges. A non-graph model (singleton, flat object-set) would add its
/// own implementor and reuse the pipeline unchanged.
pub trait SchemaTarget: 'static + Send + Sync {
    /// Runtime discriminant — disambiguates hydrators when one
    /// `SCHEMA_NAME` could attach to multiple target kinds.
    const KIND: TargetKind;

    /// Resource holding this target's `Entity ↔ CrdtId` bindings.
    type Index: Resource;
    /// Resource the outbound property ops are queued into.
    type Outbound: Resource;
    /// Message carrying inbound, server-confirmed ops.
    type Inbound: Message;

    /// `CrdtId` bound to `entity`, if any.
    fn id_for(index: &Self::Index, entity: Entity) -> Option<CrdtId>;

    /// All `(entity, id)` pairs — projection iterates these.
    fn pairs(index: &Self::Index) -> &HashMap<Entity, CrdtId>;

    /// Mint a fresh op id from the outbound queue's id source.
    fn mint_id(out: &mut Self::Outbound) -> CrdtId;

    /// Queue a property op `(target, path, delta)` for transmission.
    fn enqueue(
        out: &mut Self::Outbound,
        op_id: CrdtId,
        target: CrdtId,
        path: Path,
        delta: WireDelta,
    );

    /// Extract a property op from an inbound message, or `None` if the
    /// message isn't a property op for this target kind.
    fn match_inbound(event: &Self::Inbound) -> Option<InboundProperty<'_>>;
}

// ---------------------------------------------------------------------------
// SchemaDoc — replicated per-id schema state
// ---------------------------------------------------------------------------

/// Per-entity replicated schema state for one component type.
///
/// A plain `CrdtId → schema` map plus the causal-context state embedded
/// CRDTs (OR-Set, PN-Counter) allocate SubDots from. Updated only by
/// server-confirmed ops via [`SchemaDoc::apply_property`] — local edits
/// reach it on the server echo, never pre-applied.
#[derive(Resource)]
pub struct SchemaDoc<S>
where
    S: Crdt + SchemaApply + Default + Send + Sync + 'static,
{
    schemas: HashMap<CrdtId, S>,
    causal: CausalState,
}

impl<S> Default for SchemaDoc<S>
where
    S: Crdt + SchemaApply + Default + Send + Sync + 'static,
{
    fn default() -> Self {
        Self {
            schemas: HashMap::new(),
            causal: CausalState::new(),
        }
    }
}

impl<S> SchemaDoc<S>
where
    S: Crdt + SchemaApply + Default + Send + Sync + 'static,
{
    /// Schema state for `id`, if a slot exists.
    pub fn schema(&self, id: CrdtId) -> Option<&S> {
        self.schemas.get(&id)
    }

    /// Mutable schema state for `id`, if a slot exists.
    pub fn schema_mut(&mut self, id: CrdtId) -> Option<&mut S> {
        self.schemas.get_mut(&id)
    }

    /// Create a default schema slot for `id` if absent.
    pub fn ensure_schema(&mut self, id: CrdtId) {
        self.schemas.entry(id).or_insert_with(S::default);
    }

    /// Apply an inbound property delta to the schema at `target`,
    /// creating the slot if absent.
    ///
    /// Takes the `(target, path, delta)` triple directly — no
    /// model-specific op-envelope round-trip.
    pub fn apply_property(
        &mut self,
        op_id: CrdtId,
        seq: Option<GlobalSeq>,
        target: CrdtId,
        path: &Path,
        delta: WireDelta,
    ) -> Result<(), DeltaError> {
        let ctx = CausalContext::new(op_id, seq, &mut self.causal);
        let schema = self.schemas.entry(target).or_insert_with(S::default);
        schema.apply_wire(path, delta, &ctx)
    }
}

// ---------------------------------------------------------------------------
// SchemaHydrators — registry for snapshot-driven typed-schema installation
// ---------------------------------------------------------------------------

/// One hydrator: installs opaque per-primitive state into a [`SchemaDoc`].
/// Takes `&mut World` so it can resolve the right `SchemaDoc<S>` resource
/// without statically knowing `S`.
pub type HydratorFn = fn(&mut World, target: CrdtId, path: Path, field: OpaqueValue);

/// Registry of per-`(target-kind, schema-name)` hydrators.
///
/// Populated by [`SchemaSyncedComponentPlugin`] at build time; consulted
/// by a model's snapshot handler when a server snapshot arrives.
#[derive(Resource, Default)]
pub struct SchemaHydrators {
    by_key: HashMap<(TargetKind, String), HydratorFn>,
}

impl SchemaHydrators {
    /// Register the hydrator for component `C` under target `X`'s kind.
    /// Called once per [`SchemaSyncedComponentPlugin`] at app build time.
    pub fn register<X, C>(&mut self)
    where
        X: SchemaTarget,
        C: SchemaSync,
        <C::Schema as Crdt>::Delta: IntoWireOp,
        <C::Schema as Crdt>::Mutation: Send + Sync + 'static,
    {
        self.by_key.insert(
            (X::KIND, C::SCHEMA_NAME.to_string()),
            hydrate_schema_doc::<C> as HydratorFn,
        );
    }

    /// `true` if no hydrators are registered.
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// A clone of the full `(kind, schema-name) → hydrator` table.
    ///
    /// A snapshot handler clones the table so it can look hydrators up
    /// and then invoke them with `&mut World` — the invocation needs
    /// exclusive world access, which a borrow of this resource would
    /// block.
    pub fn all(&self) -> HashMap<(TargetKind, String), HydratorFn> {
        self.by_key.clone()
    }
}

/// Hydrator monomorphised for component `C`. Resolves the matching
/// [`SchemaDoc<C::Schema>`] resource and installs `field` at `path`.
///
/// `ensure_schema` runs first because the slot may not exist yet on the
/// receiving replica — the snapshot is authoritative for "what state
/// exists for which target."
fn hydrate_schema_doc<C>(world: &mut World, target: CrdtId, path: Path, field: OpaqueValue)
where
    C: SchemaSync,
    <C::Schema as Crdt>::Delta: IntoWireOp,
    <C::Schema as Crdt>::Mutation: Send + Sync + 'static,
{
    let mut doc = world.resource_mut::<SchemaDoc<C::Schema>>();
    doc.ensure_schema(target);
    if let Some(state) = doc.schema_mut(target) {
        if let Err(e) = state.install_state(&path, field) {
            tracing::warn!(?e, schema = %C::SCHEMA_NAME, "schema install_state failed");
        }
    }
}

// ---------------------------------------------------------------------------
// Pipeline systems — generic over `X: SchemaTarget` + `C: SchemaSync`
// ---------------------------------------------------------------------------

fn ensure_schema_slots<X, C>(
    mut doc: ResMut<SchemaDoc<C::Schema>>,
    index: Res<X::Index>,
    has_c: Query<Entity, With<C>>,
) where
    X: SchemaTarget,
    C: SchemaSync,
    <C::Schema as Crdt>::Delta: IntoWireOp,
    <C::Schema as Crdt>::Mutation: Send + Sync + 'static,
{
    // Bypass change detection while creating placeholder slots —
    // otherwise every frame's deref_mut would flip `doc.is_changed()`
    // to true, forcing [`project_to_components`] to write back to
    // every bound entity (and the resulting `Mut<C>` mark trips a
    // redundant Changed-without-real-change in the next frame's
    // [`detect_local_changes`]). Genuine apply_property mutations
    // still flip `is_changed` the normal way.
    let raw = doc.bypass_change_detection();
    for entity in has_c.iter() {
        if let Some(id) = X::id_for(&index, entity) {
            raw.ensure_schema(id);
        }
    }
}

fn detect_local_changes<X, C>(
    mut out: ResMut<X::Outbound>,
    doc: Res<SchemaDoc<C::Schema>>,
    index: Res<X::Index>,
    components: Query<(Entity, Ref<C>), Changed<C>>,
) where
    X: SchemaTarget,
    C: SchemaSync,
    <C::Schema as Crdt>::Delta: IntoWireOp,
    <C::Schema as Crdt>::Mutation: Send + Sync + 'static,
{
    // Lattice bottom for this schema — used below to discriminate
    // snapshot-hydration spawns from genuine user mutations.
    let empty_schema = C::Schema::default();
    for (entity, component) in components.iter() {
        let Some(id) = X::id_for(&index, entity) else {
            continue;
        };
        let Some(current) = doc.schema(id) else {
            continue;
        };
        // Suppress the "reconnect clobber" failure mode: a snapshot-
        // hydrated entity is spawned with only its structural marker,
        // and on-add observers auto-insert default components (e.g.
        // `Transform::default()`) before projection writes the hydrated
        // values back. Those defaults trip `Changed<C>`; a naive diff
        // would emit ops resetting every field to its local default and
        // clobber the canonical state held by other peers.
        //
        // Discriminator: snapshot-hydrated entities are `is_added()` AND
        // the doc already holds non-default state. User-spawned entities
        // are also `is_added()` but the doc is still at lattice bottom,
        // so their initial values do replicate.
        let same = *current == empty_schema;
        if component.is_added() && !same {
            continue;
        }
        let mutations = component.diff(current);
        if mutations.is_empty() {
            continue;
        }
        eprintln!(
            "[detect_local_changes schema={}] entity={entity:?} id={id:?} is_added={} doc_is_empty_default={} mutations={}",
            C::SCHEMA_NAME,
            component.is_added(),
            same,
            mutations.len()
        );
        let mut throwaway = current.clone();
        for mutation in mutations {
            let op_id = X::mint_id(&mut out);
            let mut state = CausalState::new();
            let mut ctx = CausalContext::new(op_id, None, &mut state);
            let typed_delta = throwaway.mutate(mutation, &mut ctx);
            let (inner_path, wire) = typed_delta.into_wire_op();
            let mut path = Path::field(C::SCHEMA_NAME);
            for seg in inner_path.0 {
                path.0.push(seg);
            }
            X::enqueue(&mut out, op_id, id, path, wire);
        }
    }
}

fn apply_remote_changes<X, C>(
    mut doc: ResMut<SchemaDoc<C::Schema>>,
    mut events: MessageReader<X::Inbound>,
) where
    X: SchemaTarget,
    C: SchemaSync,
    <C::Schema as Crdt>::Delta: IntoWireOp,
    <C::Schema as Crdt>::Mutation: Send + Sync + 'static,
{
    for event in events.read() {
        let Some(prop) = X::match_inbound(event) else {
            continue;
        };
        let Some(PathSegment::Field(head)) = prop.path.0.first() else {
            continue;
        };
        if head != C::SCHEMA_NAME {
            continue;
        }
        let inner_path = Path(prop.path.0[1..].to_vec());
        let result = doc.apply_property(
            prop.op_id,
            prop.seq,
            prop.target,
            &inner_path,
            prop.delta.clone(),
        );
        eprintln!(
            "[apply_remote_changes schema={}] target={:?} inner_path={:?} ok={} doc_changed={}",
            C::SCHEMA_NAME,
            prop.target,
            inner_path,
            result.is_ok(),
            doc.is_changed(),
        );
        if let Err(e) = result {
            tracing::warn!(?e, schema = %C::SCHEMA_NAME, "schema apply rejected op");
        }
    }
}

fn project_to_components<X, C>(
    mut commands: Commands,
    doc: Res<SchemaDoc<C::Schema>>,
    index: Res<X::Index>,
    mut components: Query<&mut C>,
) where
    X: SchemaTarget,
    C: SchemaSync,
    <C::Schema as Crdt>::Delta: IntoWireOp,
    <C::Schema as Crdt>::Mutation: Send + Sync + 'static,
{
    let pairs = X::pairs(&index);
    let pairs_len = pairs.len();
    let changed = doc.is_changed();
    eprintln!(
        "[project_to_components_entry schema={}] doc_changed={} pairs_len={} index_ptr={:p} pairs_ptr={:p}",
        C::SCHEMA_NAME,
        changed,
        pairs_len,
        &*index,
        pairs
    );
    if !changed {
        return;
    }
    for (entity, id) in X::pairs(&index).iter() {
        let Some(schema) = doc.schema(*id) else {
            continue;
        };
        eprintln!(
            "[project_to_components schema={}] entity={entity:?} id={id:?}",
            C::SCHEMA_NAME
        );
        match components.get_mut(*entity) {
            Ok(mut component) => component.write_back(schema),
            Err(_) => {
                let schema = schema.clone();
                commands.queue(InsertSchemaProjected::<C> {
                    entity: *entity,
                    schema,
                    _ph: PhantomData,
                });
            }
        }
    }
}

struct InsertSchemaProjected<C: SchemaSync> {
    entity: Entity,
    schema: C::Schema,
    _ph: PhantomData<fn() -> C>,
}

impl<C> Command for InsertSchemaProjected<C>
where
    C: SchemaSync,
{
    type Out = ();
    fn apply(self, world: &mut World) {
        let Ok(mut entity_mut) = world.get_entity_mut(self.entity) else {
            return;
        };
        if entity_mut.get::<C>().is_none() {
            entity_mut.insert(C::default());
        }
        if let Some(mut component) = entity_mut.get_mut::<C>() {
            component.write_back(&self.schema);
        }
    }
}

// ---------------------------------------------------------------------------
// Public plugin — drives the generic pipeline for one (target, component)
// ---------------------------------------------------------------------------

/// Syncs the typed-schema fields of component `C` for entities that the
/// target provider `X` binds to CRDT ids.
///
/// This plugin and its pipeline are model-agnostic — they name no graph
/// types. `X: SchemaTarget` is the only model-aware part: it supplies the
/// identity index, outbound queue and inbound stream. The graph crate
/// provides `NodeTarget` / `EdgeTarget`; a non-graph model provides its
/// own `X` and reuses this plugin unchanged.
///
/// Add one per `(target, component)` pair, after the model's own sync
/// plugin (which must place its detection in [`SyncSet::Structural`] and
/// its drain in [`SyncSet::Outbound`]):
///
/// ```ignore
/// app.add_plugins((
///     SchemaSyncedComponentPlugin::<NodeTarget, Transform>::default(),
///     SchemaSyncedComponentPlugin::<EdgeTarget, EdgeLabel>::default(),
/// ));
/// ```
pub struct SchemaSyncedComponentPlugin<X, C> {
    _phantom: PhantomData<fn() -> (X, C)>,
}

impl<X, C> Default for SchemaSyncedComponentPlugin<X, C> {
    fn default() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<X, C> Plugin for SchemaSyncedComponentPlugin<X, C>
where
    X: SchemaTarget,
    C: SchemaSync,
    <C::Schema as Crdt>::Delta: IntoWireOp,
    <C::Schema as Crdt>::Mutation: Send + Sync + 'static,
{
    fn build(&self, app: &mut App) {
        app.init_resource::<SchemaDoc<C::Schema>>();
        app.init_resource::<SchemaHydrators>();
        app.world_mut()
            .resource_mut::<SchemaHydrators>()
            .register::<X, C>();
        // Slots strictly between the model's structural detection (which
        // populates `X::Index`) and its outbound drain. Ordering against
        // the `SyncSet` phases — not the model's own systems — is what
        // keeps this plugin free of model type parameters.
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
}
