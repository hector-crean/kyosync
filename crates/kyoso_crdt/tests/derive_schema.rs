//! End-to-end validation of the `derive(Crdt)` proc-macro against the
//! base CRDT primitives — Lattice axioms, typed Crdt apply/mutate, and
//! the wire-driven SchemaApply path.

use kyoso_crdt::types::{LwwMut, LwwRegister, OrSet, OrSetMut, PnCounter, PnMut};
use kyoso_crdt::{
    CausalContext, CausalState, Crdt, CrdtId, DeltaError, DeriveCrdt, Lattice, Path, PathSegment,
    SchemaApply, WireDelta,
};

/// A figma-shaped node properties bag with mixed CRDT types.
#[derive(Clone, Debug, PartialEq, DeriveCrdt)]
pub struct NodeProperties {
    pub name: LwwRegister<String>,
    pub tags: OrSet<String>,
    pub counter: PnCounter,
}

fn ctx_at(state: &mut CausalState, peer: u32, seq: u64) -> CausalContext<'_> {
    CausalContext::new(CrdtId::new(peer, seq), Some(seq), state)
}

#[test]
fn derive_lattice_bottom_is_per_field_bottom() {
    let p = NodeProperties::bottom();
    assert!(p.name.get().is_none());
    assert!(p.tags.is_empty());
    assert_eq!(p.counter.value(), 0);
}

#[test]
fn derive_lattice_join_is_pointwise() {
    let mut a = NodeProperties::bottom();
    let mut b = NodeProperties::bottom();
    let mut sa = CausalState::new();
    let mut sb = CausalState::new();

    a.mutate(
        NodePropertiesMut::Name(LwwMut::Set("alice".to_string())),
        &mut ctx_at(&mut sa, 1, 1),
    );
    b.mutate(
        NodePropertiesMut::Tags(OrSetMut::Add("draft".to_string())),
        &mut ctx_at(&mut sb, 2, 1),
    );
    b.mutate(
        NodePropertiesMut::Counter(PnMut::Inc(5)),
        &mut ctx_at(&mut sb, 2, 2),
    );

    a.join(b);
    assert_eq!(a.name.get(), Some(&"alice".to_string()));
    assert!(a.tags.contains(&"draft".to_string()));
    assert_eq!(a.counter.value(), 5);
}

#[test]
fn derive_typed_apply_routes_to_field() {
    let mut p = NodeProperties::bottom();
    let mut state = CausalState::new();

    let delta = p.mutate(
        NodePropertiesMut::Name(LwwMut::Set("bob".to_string())),
        &mut ctx_at(&mut state, 1, 1),
    );
    assert_eq!(p.name.get(), Some(&"bob".to_string()));

    // Apply the same delta on a fresh replica — should reach the same state.
    let mut q = NodeProperties::bottom();
    let mut qs = CausalState::new();
    q.apply(&delta, &ctx_at(&mut qs, 1, 1)).unwrap();
    assert_eq!(q.name.get(), Some(&"bob".to_string()));
}

#[test]
fn derive_schema_apply_dispatches_via_field_name() {
    let mut p = NodeProperties::bottom();
    let mut state = CausalState::new();

    // Wire delta arriving from "name" field.
    let path = Path(vec![PathSegment::Field("name".to_string())]);
    let wire = WireDelta::LwwReplace {
        value: postcard::to_allocvec(&"alice".to_string()).unwrap(),
    };
    p.apply_wire(&path, wire, &ctx_at(&mut state, 1, 1)).unwrap();
    assert_eq!(p.name.get(), Some(&"alice".to_string()));
}

#[test]
fn derive_schema_apply_dispatches_to_orset() {
    let mut p = NodeProperties::bottom();
    let mut state = CausalState::new();

    let path = Path(vec![PathSegment::Field("tags".to_string())]);
    let wire = WireDelta::OrSetAdd {
        value: postcard::to_allocvec(&"draft".to_string()).unwrap(),
    };
    p.apply_wire(&path, wire, &ctx_at(&mut state, 1, 1)).unwrap();
    assert!(p.tags.contains(&"draft".to_string()));
}

#[test]
fn derive_schema_apply_dispatches_to_pn_counter() {
    let mut p = NodeProperties::bottom();
    let mut state = CausalState::new();

    let path = Path(vec![PathSegment::Field("counter".to_string())]);
    let wire = WireDelta::PnCounterDelta { by: 3 };
    p.apply_wire(&path, wire, &ctx_at(&mut state, 1, 1)).unwrap();
    assert_eq!(p.counter.value(), 3);
}

#[test]
fn derive_schema_apply_unknown_field_errors() {
    let mut p = NodeProperties::bottom();
    let mut state = CausalState::new();

    let path = Path(vec![PathSegment::Field("nonexistent".to_string())]);
    let wire = WireDelta::LwwReplace {
        value: postcard::to_allocvec(&"x".to_string()).unwrap(),
    };
    let err = p
        .apply_wire(&path, wire, &ctx_at(&mut state, 1, 1))
        .unwrap_err();
    assert!(matches!(err, DeltaError::UnknownPath { .. }));
}

#[test]
fn derive_schema_apply_type_mismatch_errors() {
    let mut p = NodeProperties::bottom();
    let mut state = CausalState::new();

    // Send an OrSet delta to the LWW `name` field.
    let path = Path(vec![PathSegment::Field("name".to_string())]);
    let wire = WireDelta::OrSetAdd {
        value: postcard::to_allocvec(&"alice".to_string()).unwrap(),
    };
    let err = p
        .apply_wire(&path, wire, &ctx_at(&mut state, 1, 1))
        .unwrap_err();
    assert!(matches!(err, DeltaError::TypeMismatch { .. }));
}

/// A nested schema: `Style` is itself a derive(Crdt) struct used as a
/// field of a parent `RichNode` schema.
#[derive(Clone, Debug, Default, PartialEq, DeriveCrdt)]
pub struct Style {
    pub fill: LwwRegister<String>,
    pub opacity: LwwRegister<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveCrdt)]
pub struct RichNode {
    pub name: LwwRegister<String>,
    pub style: Style,
}

#[test]
fn nested_schema_dispatches_via_multi_segment_path() {
    use kyoso_crdt::types::CausalMap;

    let mut node = RichNode::bottom();
    let mut state = CausalState::new();

    // path = ["style", "fill"]
    let path = Path(vec![
        PathSegment::Field("style".to_string()),
        PathSegment::Field("fill".to_string()),
    ]);
    let wire = WireDelta::LwwReplace {
        value: postcard::to_allocvec(&"#ff00ff".to_string()).unwrap(),
    };
    node.apply_wire(&path, wire, &ctx_at(&mut state, 1, 1)).unwrap();
    assert_eq!(
        node.style.fill.get(),
        Some(&"#ff00ff".to_string()),
        "nested struct field set via multi-segment path"
    );

    // The other style field should be untouched.
    assert_eq!(node.style.opacity.get(), None);

    // And the parent's name field also untouched.
    assert_eq!(node.name.get(), None);
    let _ = CausalMap::<LwwRegister<u32>>::new(); // ensure import is used
}

#[test]
fn nested_schema_unknown_inner_segment_errors() {
    let mut node = RichNode::bottom();
    let mut state = CausalState::new();

    let path = Path(vec![
        PathSegment::Field("style".to_string()),
        PathSegment::Field("nope".to_string()),
    ]);
    let wire = WireDelta::LwwReplace {
        value: postcard::to_allocvec(&"x".to_string()).unwrap(),
    };
    let err = node
        .apply_wire(&path, wire, &ctx_at(&mut state, 1, 1))
        .unwrap_err();
    assert!(matches!(err, DeltaError::UnknownPath { .. }));
}

#[test]
fn leaf_rejects_extra_path_tail() {
    let mut node = RichNode::bottom();
    let mut state = CausalState::new();

    // "name" is a leaf LwwRegister; an extra segment is invalid.
    let path = Path(vec![
        PathSegment::Field("name".to_string()),
        PathSegment::Field("garbage".to_string()),
    ]);
    let wire = WireDelta::LwwReplace {
        value: postcard::to_allocvec(&"x".to_string()).unwrap(),
    };
    let err = node
        .apply_wire(&path, wire, &ctx_at(&mut state, 1, 1))
        .unwrap_err();
    assert!(matches!(err, DeltaError::Invalid { .. }));
}

#[test]
fn derive_idempotent_typed_apply() {
    let mut p = NodeProperties::bottom();
    let mut state = CausalState::new();

    let delta = p.mutate(
        NodePropertiesMut::Name(LwwMut::Set("alice".to_string())),
        &mut ctx_at(&mut state, 1, 1),
    );

    let mut q = NodeProperties::bottom();
    let mut qs = CausalState::new();
    q.apply(&delta, &ctx_at(&mut qs, 1, 1)).unwrap();
    q.apply(&delta, &ctx_at(&mut qs, 1, 1)).unwrap();
    assert_eq!(q.name.get(), Some(&"alice".to_string()));
}
