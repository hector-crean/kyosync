//! Per-category reference-edge schemas — validates that [`RefEdgeCrdt`]
//! impls can compose with the derive macro to build typed property
//! schemas per category.

use kyoso_crdt::types::LwwRegister;
use kyoso_crdt::{CausalContext, CausalState, Crdt, CrdtId, DeriveCrdt, Lattice};
use kyoso_graph_crdt::{DanglePolicy, RefEdgeCrdt, RefEdgePolicy};

/// Per-edge property schema for `prototype_link`.
#[derive(Clone, Debug, Default, PartialEq, DeriveCrdt)]
pub struct PrototypeTransition {
    pub kind: LwwRegister<String>,
    pub easing: LwwRegister<String>,
    pub duration_ms: LwwRegister<u32>,
}

#[derive(Default)]
pub struct PrototypeLinkEdge;

impl RefEdgeCrdt for PrototypeLinkEdge {
    const POLICY: RefEdgePolicy = RefEdgePolicy::OrSet;
    const DANGLE: DanglePolicy = DanglePolicy::Tolerate;
    type Properties = PrototypeTransition;
}

/// Per-edge property schema for `instance_of`.
#[derive(Clone, Debug, Default, PartialEq, DeriveCrdt)]
pub struct InstanceOverrides {
    pub label: LwwRegister<String>,
}

#[derive(Default)]
pub struct InstanceOfEdge;

impl RefEdgeCrdt for InstanceOfEdge {
    const POLICY: RefEdgePolicy = RefEdgePolicy::OrSet;
    const DANGLE: DanglePolicy = DanglePolicy::Tolerate;
    type Properties = InstanceOverrides;
}

/// `mask_of` keeps a permanent removal contract — once removed, never
/// re-added via concurrent ops.
#[derive(Default)]
pub struct MaskOfEdge;

impl RefEdgeCrdt for MaskOfEdge {
    const POLICY: RefEdgePolicy = RefEdgePolicy::TwoPSet;
    const DANGLE: DanglePolicy = DanglePolicy::Cascade;
    type Properties = (); // No per-edge properties.
}

fn ctx_at(state: &mut CausalState, peer: u32, seq: u64) -> CausalContext<'_> {
    CausalContext::new(CrdtId::new(peer, seq), Some(seq), state)
}

#[test]
fn prototype_link_categories_have_distinct_policies() {
    assert_eq!(PrototypeLinkEdge::POLICY, RefEdgePolicy::OrSet);
    assert_eq!(MaskOfEdge::POLICY, RefEdgePolicy::TwoPSet);
    assert_eq!(MaskOfEdge::DANGLE, DanglePolicy::Cascade);
    assert_eq!(InstanceOfEdge::DANGLE, DanglePolicy::Tolerate);
}

#[test]
fn prototype_transition_schema_is_a_crdt() {
    use kyoso_crdt::types::LwwMut;

    let mut p = PrototypeTransition::bottom();
    let mut s = CausalState::new();

    p.mutate(
        PrototypeTransitionMut::Kind(LwwMut::Set("dissolve".to_string())),
        &mut ctx_at(&mut s, 1, 1),
    );
    p.mutate(
        PrototypeTransitionMut::DurationMs(LwwMut::Set(300)),
        &mut ctx_at(&mut s, 1, 2),
    );

    assert_eq!(p.kind.get(), Some(&"dissolve".to_string()));
    assert_eq!(p.duration_ms.get(), Some(&300));
}

#[test]
fn prototype_transition_converges_under_join() {
    use kyoso_crdt::types::LwwMut;

    let mut a = PrototypeTransition::bottom();
    let mut b = PrototypeTransition::bottom();
    let mut sa = CausalState::new();
    let mut sb = CausalState::new();

    a.mutate(
        PrototypeTransitionMut::Kind(LwwMut::Set("instant".to_string())),
        &mut ctx_at(&mut sa, 1, 1),
    );
    b.mutate(
        PrototypeTransitionMut::Easing(LwwMut::Set("linear".to_string())),
        &mut ctx_at(&mut sb, 2, 1),
    );

    a.join(b);
    assert_eq!(a.kind.get(), Some(&"instant".to_string()));
    assert_eq!(a.easing.get(), Some(&"linear".to_string()));
}
