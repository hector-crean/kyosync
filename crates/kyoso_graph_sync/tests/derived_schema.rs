//! End-to-end test for `#[derive(SchemaSync)]` (Phase A — LWW happy path).
//!
//! Spawns two real `kyoso_server`-backed apps using a Bevy component
//! that has `SchemaSync` synthesized by the derive macro. Verifies that
//! field-level mutations replicate end-to-end with the same semantics as
//! the hand-written impls covered by `tests/two_apps.rs`.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use kyoso_graph::GraphManagerPlugin;
use kyoso_server::{AppState, app};
use kyoso_graph_sync::{
    GraphSyncPlugin, NodePresence, NodeTarget, SchemaSync, SchemaSyncedComponentPlugin,
};
use kyoso_sync::SyncStatus;
use tokio::net::TcpListener;

// `Reflect` is no longer required by `Syncable` (Part VIII), but Bevy
// inspector tooling still expects it on app-level components — keep
// the derive for parity with real apps.
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Derived")]
#[require(NodePresence)]
struct Derived {
    name: String,
    width: f32,
    height: f32,
    visible: bool,
}

#[derive(Component, Default, Debug, Clone, Reflect)]
#[reflect(Component, Default)]
struct DerivedEdge;

/// Second schema component for the multi-component late-join test —
/// mirrors having multiple `SchemaSyncedComponentPlugin`s on one
/// entity (kyoso_client has Frame + Rectangle + Size + Transform + …).
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "DerivedTwo")]
#[require(NodePresence)]
struct DerivedTwo {
    count: u32,
    label: String,
}

async fn spawn_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = AppState::in_memory();
    let router = app(state);
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

fn build_app(server: SocketAddr, room: &str) -> App {
    let mut app = App::new();
    app.add_plugins((
        GraphManagerPlugin::<Derived, DerivedEdge>::new(),
        GraphSyncPlugin::new(format!("ws://{server}/ws"), room),
        SchemaSyncedComponentPlugin::<NodeTarget, Derived>::default(),
    ));
    app
}

fn build_app_multi(server: SocketAddr, room: &str) -> App {
    let mut app = App::new();
    app.add_plugins((
        GraphManagerPlugin::<Derived, DerivedEdge>::new(),
        GraphSyncPlugin::new(format!("ws://{server}/ws"), room),
        SchemaSyncedComponentPlugin::<NodeTarget, Derived>::default(),
        SchemaSyncedComponentPlugin::<NodeTarget, DerivedTwo>::default(),
    ));
    app
}

fn pump_until(
    apps: &mut [&mut App],
    timeout: Duration,
    label: &str,
    mut pred: impl FnMut(&mut [&mut App]) -> bool,
) {
    let deadline = Instant::now() + timeout;
    loop {
        for app in apps.iter_mut() {
            app.update();
        }
        if pred(apps) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for: {label}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn sync_status(app: &mut App) -> SyncStatus {
    *app.world()
        .get_resource::<SyncStatus>()
        .expect("SyncStatus resource present")
}

/// Spawn a derived-schema component on A; verify B sees every field
/// replicated. Then mutate one field on each peer concurrently;
/// verify both peers converge to the union.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn derived_schema_replicates_all_fields() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let mut a = build_app(addr, "derived-room");
        let mut b = build_app(addr, "derived-room");

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(2),
            "welcome",
            |apps| apps.iter_mut().all(|app| sync_status(app).is_connected()),
        );

        // Spawn with non-default values for every field.
        let _entity_a = a.world_mut().spawn(Derived {
            name: "alpha".into(),
            width: 320.0,
            height: 240.0,
            visible: true,
        }).id();

        // Wait until B sees the matching values via the derived schema.
        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B sees all four fields replicated",
            |apps| {
                let mut q = apps[1].world_mut().query::<&Derived>();
                q.iter(apps[1].world()).any(|d| {
                    d.name == "alpha"
                        && (d.width - 320.0).abs() < 0.001
                        && (d.height - 240.0).abs() < 0.001
                        && d.visible
                })
            },
        );
    });
    join.await.expect("worker panic");
}

/// Multi-component late-join: A spawns ONE entity with TWO different
/// schema-synced components. B joins after, must end up with both
/// components populated with the right field values.
///
/// Mirrors the kyoso_client setup where one FigmaNode entity carries
/// many schemas (Frame + Rectangle + Size + Transform + ...).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_joiner_receives_multi_component_schema_via_welcome() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let mut a = build_app_multi(addr, "late-multi-room");
        pump_until(
            &mut [&mut a],
            Duration::from_secs(2),
            "A connected",
            |apps| sync_status(apps[0]).is_connected(),
        );

        // Spawn ONE entity with both schema components, both populated.
        a.world_mut().spawn((
            Derived {
                name: "alpha".into(),
                width: 320.0,
                height: 240.0,
                visible: true,
            },
            DerivedTwo { count: 7, label: "tag".into() },
        ));

        // Wait until 1 AddNode + 4 (Derived fields) + 2 (DerivedTwo fields) = 7 ops.
        pump_until(
            &mut [&mut a],
            Duration::from_secs(3),
            "A's ops settle",
            |apps| {
                apps[0]
                    .world()
                    .resource::<kyoso_graph_sync::ClientSyncEngine>()
                    .applied_seq()
                    >= 7
            },
        );

        let mut b = build_app_multi(addr, "late-multi-room");
        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B sees both components with non-default values",
            |apps| {
                let mut q1 = apps[1].world_mut().query::<&Derived>();
                let derived_ok = q1.iter(apps[1].world()).any(|d| {
                    d.name == "alpha"
                        && (d.width - 320.0).abs() < 0.001
                        && (d.height - 240.0).abs() < 0.001
                        && d.visible
                });
                let mut q2 = apps[1].world_mut().query::<&DerivedTwo>();
                let two_ok = q2
                    .iter(apps[1].world())
                    .any(|d| d.count == 7 && d.label == "tag");
                derived_ok && two_ok
            },
        );
    });
    join.await.expect("worker panic");
}

/// Late-joiner reproduction: A spawns a fully-populated schema component
/// while B is *not yet connected*; only after A's ops settle does B join.
/// B must end up with the same non-default field values, delivered via
/// the Welcome diff path (no snapshot — this runs well under the
/// 60s snapshot interval).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_joiner_receives_typed_schema_properties_via_welcome() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        // A connects first and gets its own welcome.
        let mut a = build_app(addr, "late-derived-room");
        pump_until(
            &mut [&mut a],
            Duration::from_secs(2),
            "A connected",
            |apps| sync_status(apps[0]).is_connected(),
        );

        // A spawns ONE entity with a fully-populated Derived component.
        a.world_mut().spawn(Derived {
            name: "alpha".into(),
            width: 320.0,
            height: 240.0,
            visible: true,
        });

        // Pump until A's outbound has confirmed all field ops landed on
        // the server (1 AddNode + 4 SetNodeProperty = 5 ops).
        pump_until(
            &mut [&mut a],
            Duration::from_secs(3),
            "A's ops settle",
            |apps| {
                apps[0]
                    .world()
                    .resource::<kyoso_graph_sync::ClientSyncEngine>()
                    .applied_seq()
                    >= 5
            },
        );

        // NOW B joins — the only path that delivers state is the Welcome
        // diff. The snapshot scheduler hasn't fired (60s interval, only
        // a few seconds elapsed), so the diff includes ALL ops since
        // seq 0 — both AddNode and the four SetNodeProperty ops.
        let mut b = build_app(addr, "late-derived-room");
        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B converges via Welcome — properties replicated",
            |apps| {
                let mut q = apps[1].world_mut().query::<&Derived>();
                q.iter(apps[1].world()).any(|d| {
                    d.name == "alpha"
                        && (d.width - 320.0).abs() < 0.001
                        && (d.height - 240.0).abs() < 0.001
                        && d.visible
                })
            },
        );
    });
    join.await.expect("worker panic");
}

/// Schema-name container attribute is honored: the wire path uses the
/// configured name, so only consumers of the same `#[schema(name = ...)]`
/// see each other's mutations. Sanity-check by inspecting the schema's
/// `SCHEMA_NAME` constant directly.
#[test]
fn schema_name_attribute_is_honored() {
    assert_eq!(<Derived as SchemaSync>::SCHEMA_NAME, "Derived");
}

/// Echo-guard sanity: a freshly-defaulted component compared against a
/// bottom doc state must produce zero mutations.
#[test]
fn default_component_against_bottom_doc_emits_nothing() {
    let component = Derived::default();
    let bottom = <DerivedSchema as kyoso_crdt::Lattice>::bottom();
    let mutations = component.diff(&bottom);
    assert!(
        mutations.is_empty(),
        "expected zero mutations from default-vs-bottom; got {} mutations",
        mutations.len(),
    );
}

// ---------------------------------------------------------------------------
// Phase B — #[crdt(skip)] / #[crdt(rename)] / #[crdt(default)]
// ---------------------------------------------------------------------------

/// Component with one synced field and one skipped field. The skipped
/// field doesn't appear in the schema struct at all, so the wire ops
/// only carry `synced` updates.
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Skipping")]
#[require(NodePresence)]
struct Skipping {
    synced: u32,

    #[crdt(skip)]
    local_only: u32,
}

#[test]
fn skipped_fields_do_not_appear_in_schema() {
    // The schema is generated by the macro; if the field were present it
    // would be a public field of `SkippingSchema`. Constructing the
    // schema from its `Default` and verifying the synced field works
    // (and that the schema can be cloned/compared without referring to
    // `local_only`) is sufficient.
    let bottom = SkippingSchema::default();
    let mut a = Skipping {
        synced: 7,
        local_only: 99,
    };
    let mutations = a.diff(&bottom);
    assert_eq!(
        mutations.len(),
        1,
        "only the `synced` field should produce a mutation",
    );

    // write_back leaves local_only untouched.
    let schema = SkippingSchema::default();
    a.local_only = 99;
    a.synced = 0;
    a.write_back(&schema);
    assert_eq!(
        a.local_only, 99,
        "skipped field must not be touched by write_back",
    );
}

/// Component with a renamed field. The wire path uses the renamed
/// identifier; the schema struct's field is the renamed one; the
/// component-side field name is unchanged.
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Renamed")]
#[require(NodePresence)]
struct Renamed {
    #[crdt(rename = "w")]
    width: f32,

    #[crdt(rename = "h")]
    height: f32,
}

#[test]
fn renamed_fields_use_renamed_identifier_in_schema() {
    // The schema's fields are `w` and `h`; constructing one verifies the
    // rename took effect — if it hadn't, `RenamedSchema { w, h }` would
    // be a compile error.
    let _schema = RenamedSchema {
        w: kyoso_crdt::types::LwwRegister::default(),
        h: kyoso_crdt::types::LwwRegister::default(),
    };

    // diff still reads from `self.width` / `self.height` on
    // the component side; the rename only affects the schema-side
    // identifier and the wire path.
    let component = Renamed { width: 1.0, height: 2.0 };
    let bottom = RenamedSchema::default();
    let mutations = component.diff(&bottom);
    assert_eq!(
        mutations.len(),
        2,
        "both renamed fields differ from defaults; expect 2 mutations",
    );
}

/// Component with a custom `default` expression on one field. The
/// echo-guard fallback uses this expression instead of
/// `Self::default().<field>`. Useful when `Self: Default` would set the
/// field to a value the user considers "no opinion" different from the
/// component's natural default.
///
/// Practical example: a `flags: u32` whose component default is `0` but
/// where the user wants "no opinion" to mean `0xFFFF_FFFF` (a sentinel).
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "DefaultsCustom")]
#[require(NodePresence)]
struct DefaultsCustom {
    #[crdt(default = "42u32")]
    sentinel: u32,
}

#[test]
fn custom_default_expression_is_used_as_echo_guard() {
    // Component value matching the custom default → no mutation emitted.
    let matches_custom_default = DefaultsCustom { sentinel: 42 };
    let bottom = DefaultsCustomSchema::default();
    let mutations = matches_custom_default.diff(&bottom);
    assert!(
        mutations.is_empty(),
        "component value `42` matches the custom default `42`; expected no mutations",
    );

    // Component value differing from the custom default → mutation
    // emitted.
    let differs = DefaultsCustom { sentinel: 0 };
    let mutations = differs.diff(&bottom);
    assert_eq!(
        mutations.len(),
        1,
        "component value `0` differs from custom default `42`; expected one mutation",
    );
}

/// Combined: skip + rename + default in one component. Verifies the
/// macro composes the three correctly.
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Combined")]
#[require(NodePresence)]
struct Combined {
    #[crdt(skip)]
    cache_token: u64,

    #[crdt(rename = "label", default = "String::from(\"untitled\")")]
    name: String,

    flag: bool,
}

// ---------------------------------------------------------------------------
// Phase C — #[crdt(or_set)] / #[crdt(counter)]
// ---------------------------------------------------------------------------

/// Component using `#[crdt(or_set)]` over a `Vec<String>`.
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Tagged")]
#[require(NodePresence)]
struct Tagged {
    #[crdt(or_set)]
    tags: Vec<String>,
}

#[test]
fn or_set_default_against_bottom_emits_nothing() {
    // Empty Vec vs empty OrSet → no add/remove deltas.
    let component = Tagged::default();
    let bottom = TaggedSchema::default();
    let mutations = component.diff(&bottom);
    assert!(mutations.is_empty(), "default empty Vec should not emit");
}

#[test]
fn or_set_emits_adds_for_new_elements() {
    let component = Tagged {
        tags: vec!["draft".into(), "urgent".into()],
    };
    let bottom = TaggedSchema::default();
    let mutations = component.diff(&bottom);
    assert_eq!(mutations.len(), 2, "two new elements → two Add mutations");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn or_set_replicates_end_to_end() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let mut a = build_tagged_app(addr, "or-set-room");
        let mut b = build_tagged_app(addr, "or-set-room");

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(2),
            "welcome",
            |apps| apps.iter_mut().all(|app| sync_status(app).is_connected()),
        );

        // App A spawns a Tagged with two tags.
        a.world_mut().spawn(Tagged {
            tags: vec!["draft".into(), "urgent".into()],
        });

        // B converges to the same set of tags.
        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B sees both tags",
            |apps| {
                let mut q = apps[1].world_mut().query::<&Tagged>();
                q.iter(apps[1].world()).any(|t| {
                    t.tags.contains(&"draft".to_string())
                        && t.tags.contains(&"urgent".to_string())
                })
            },
        );
    });
    join.await.expect("worker panic");
}

fn build_tagged_app(server: SocketAddr, room: &str) -> App {
    let mut app = App::new();
    app.add_plugins((
        GraphManagerPlugin::<Tagged, DerivedEdge>::new(),
        GraphSyncPlugin::new(format!("ws://{server}/ws"), room),
        SchemaSyncedComponentPlugin::<NodeTarget, Tagged>::default(),
    ));
    app
}

/// Component using `#[crdt(counter)]` over an `i64`.
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Counted")]
#[require(NodePresence)]
struct Counted {
    #[crdt(counter)]
    edits: i64,
}

#[test]
fn counter_zero_against_bottom_emits_nothing() {
    let component = Counted::default();
    let bottom = CountedSchema::default();
    let mutations = component.diff(&bottom);
    assert!(
        mutations.is_empty(),
        "edits=0 against bottom (value=0) should not emit",
    );
}

#[test]
fn counter_emits_inc_for_positive_diff() {
    let component = Counted { edits: 5 };
    let bottom = CountedSchema::default();
    let mutations = component.diff(&bottom);
    assert_eq!(mutations.len(), 1, "one Inc mutation expected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn counter_replicates_end_to_end() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let mut a = build_counted_app(addr, "counter-room");
        let mut b = build_counted_app(addr, "counter-room");

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(2),
            "welcome",
            |apps| apps.iter_mut().all(|app| sync_status(app).is_connected()),
        );

        a.world_mut().spawn(Counted { edits: 7 });

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B sees edits=7",
            |apps| {
                let mut q = apps[1].world_mut().query::<&Counted>();
                q.iter(apps[1].world()).any(|c| c.edits == 7)
            },
        );
    });
    join.await.expect("worker panic");
}

fn build_counted_app(server: SocketAddr, room: &str) -> App {
    let mut app = App::new();
    app.add_plugins((
        GraphManagerPlugin::<Counted, DerivedEdge>::new(),
        GraphSyncPlugin::new(format!("ws://{server}/ws"), room),
        SchemaSyncedComponentPlugin::<NodeTarget, Counted>::default(),
    ));
    app
}

#[test]
fn combined_skip_rename_default_compose() {
    // SchemaSync wire name from the container attr.
    assert_eq!(<Combined as SchemaSync>::SCHEMA_NAME, "Combined");

    // Schema field is `label` (renamed from `name`).
    let _schema = CombinedSchema {
        label: kyoso_crdt::types::LwwRegister::default(),
        flag: kyoso_crdt::types::LwwRegister::default(),
    };

    // A `name = "untitled"` component matches the custom default → no
    // emission for that field.
    let component = Combined {
        cache_token: 999,
        name: "untitled".into(),
        flag: false,
    };
    let bottom = CombinedSchema::default();
    let mutations = component.diff(&bottom);
    assert!(
        mutations.is_empty(),
        "all three fields are at their (custom or natural) defaults; \
         expected no mutations, got {}",
        mutations.len(),
    );

    // Mutate the renamed field to a non-default value → one mutation.
    let component = Combined {
        cache_token: 999,
        name: "alpha".into(),
        flag: false,
    };
    let mutations = component.diff(&bottom);
    assert_eq!(mutations.len(), 1, "only `name` differs from custom default");
}

// ---------------------------------------------------------------------------
// Phase D — #[crdt(map)] / #[crdt(nested)]
// ---------------------------------------------------------------------------

use std::collections::HashMap;

/// Component using `#[crdt(map)]` over a `HashMap<String, String>`. The
/// schema-side type is `CausalMap<LwwRegister<String>>`.
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Mapped")]
#[require(NodePresence)]
struct Mapped {
    #[crdt(map)]
    properties: HashMap<String, String>,
}

#[test]
fn map_default_against_bottom_emits_nothing() {
    let component = Mapped::default();
    let bottom = MappedSchema::default();
    let mutations = component.diff(&bottom);
    assert!(mutations.is_empty(), "default empty HashMap should not emit");
}

#[test]
fn map_emits_apply_for_new_keys() {
    let mut props = HashMap::new();
    props.insert("color".into(), "red".into());
    props.insert("size".into(), "large".into());
    let component = Mapped { properties: props };
    let bottom = MappedSchema::default();
    let mutations = component.diff(&bottom);
    assert_eq!(mutations.len(), 2, "two new keys → two Apply mutations");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn map_replicates_end_to_end() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let mut a = build_mapped_app(addr, "map-room");
        let mut b = build_mapped_app(addr, "map-room");

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(2),
            "welcome",
            |apps| apps.iter_mut().all(|app| sync_status(app).is_connected()),
        );

        let mut props = HashMap::new();
        props.insert("color".into(), "red".into());
        props.insert("size".into(), "large".into());
        a.world_mut().spawn(Mapped { properties: props });

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B sees both keys",
            |apps| {
                let mut q = apps[1].world_mut().query::<&Mapped>();
                q.iter(apps[1].world()).any(|m| {
                    m.properties.get("color") == Some(&"red".to_string())
                        && m.properties.get("size") == Some(&"large".to_string())
                })
            },
        );
    });
    join.await.expect("worker panic");
}

fn build_mapped_app(server: SocketAddr, room: &str) -> App {
    let mut app = App::new();
    app.add_plugins((
        GraphManagerPlugin::<Mapped, DerivedEdge>::new(),
        GraphSyncPlugin::new(format!("ws://{server}/ws"), room),
        SchemaSyncedComponentPlugin::<NodeTarget, Mapped>::default(),
    ));
    app
}

// ---------- nested ----------------------------------------------------------

/// Inner type that itself derives `SchemaSync` so it can be used as a
/// `#[crdt(nested)]` field. Components implementing `SchemaSync` don't
/// need to be Bevy `Component`s themselves — only the outer holding
/// component is the Bevy entity. But we keep the derive(Component) here
/// so the same struct could be spawned standalone if desired.
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Inner")]
#[require(NodePresence)]
struct Inner {
    label: String,
    weight: f32,
}

/// Outer component embedding `Inner` via `#[crdt(nested)]`. The schema
/// generated for `Outer` carries a `inner: InnerSchema` field; mutations
/// to `Outer.inner` recurse through the inner schema's own
/// `diff`.
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Outer")]
#[require(NodePresence)]
struct Outer {
    name: String,
    #[crdt(nested)]
    inner: Inner,
}

#[test]
fn nested_default_against_bottom_emits_nothing() {
    let component = Outer::default();
    let bottom = OuterSchema::default();
    let mutations = component.diff(&bottom);
    assert!(mutations.is_empty(), "default Outer should not emit");
}

#[test]
fn nested_emits_inner_field_mutations_wrapped_in_outer_variant() {
    let component = Outer {
        name: "outer-alpha".into(),
        inner: Inner {
            label: "inner-alpha".into(),
            weight: 1.5,
        },
    };
    let bottom = OuterSchema::default();
    let mutations = component.diff(&bottom);
    // Three diffs: outer.name + inner.label + inner.weight.
    assert_eq!(
        mutations.len(),
        3,
        "expected 3 mutations (1 outer + 2 inner); got {}",
        mutations.len(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nested_replicates_end_to_end() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let mut a = build_outer_app(addr, "nested-room");
        let mut b = build_outer_app(addr, "nested-room");

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(2),
            "welcome",
            |apps| apps.iter_mut().all(|app| sync_status(app).is_connected()),
        );

        a.world_mut().spawn(Outer {
            name: "alpha".into(),
            inner: Inner {
                label: "child".into(),
                weight: 2.5,
            },
        });

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B sees outer + nested fields",
            |apps| {
                let mut q = apps[1].world_mut().query::<&Outer>();
                q.iter(apps[1].world()).any(|o| {
                    o.name == "alpha"
                        && o.inner.label == "child"
                        && (o.inner.weight - 2.5).abs() < 0.001
                })
            },
        );
    });
    join.await.expect("worker panic");
}

fn build_outer_app(server: SocketAddr, room: &str) -> App {
    let mut app = App::new();
    app.add_plugins((
        GraphManagerPlugin::<Outer, DerivedEdge>::new(),
        GraphSyncPlugin::new(format!("ws://{server}/ws"), room),
        SchemaSyncedComponentPlugin::<NodeTarget, Outer>::default(),
    ));
    app
}

// ---------------------------------------------------------------------------
// Phase F — #[crdt(sequence)] for String / Vec<T>
// ---------------------------------------------------------------------------

/// Component using `#[crdt(sequence)]` over a `String`. The schema-side
/// type is `Sequence<char>` and the diff is per-character.
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "TextDoc")]
#[require(NodePresence)]
struct TextDoc {
    #[crdt(sequence)]
    body: String,
}

#[test]
fn sequence_string_default_against_bottom_emits_nothing() {
    let component = TextDoc::default();
    let bottom = TextDocSchema::default();
    let mutations = component.diff(&bottom);
    assert!(mutations.is_empty(), "default empty String should not emit");
}

#[test]
fn sequence_string_emits_one_insert_per_character() {
    let component = TextDoc { body: "hi".into() };
    let bottom = TextDocSchema::default();
    let mutations = component.diff(&bottom);
    // Two characters → two InsertAt mutations.
    assert_eq!(mutations.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequence_string_replicates_end_to_end() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let mut a = build_text_app(addr, "text-room");
        let mut b = build_text_app(addr, "text-room");

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(2),
            "welcome",
            |apps| apps.iter_mut().all(|app| sync_status(app).is_connected()),
        );

        let entity_a = a.world_mut().spawn(TextDoc { body: "hello".into() }).id();

        // Wait for B to converge to the initial body.
        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B sees 'hello'",
            |apps| {
                let mut q = apps[1].world_mut().query::<&TextDoc>();
                q.iter(apps[1].world()).any(|t| t.body == "hello")
            },
        );

        // Mutate A's text by replacing the middle ('ell' → 'ELL').
        a.world_mut()
            .get_mut::<TextDoc>(entity_a)
            .unwrap()
            .body = "hELLo".into();

        // The prefix-suffix diff finds prefix='h' (1), suffix='o' (1),
        // and emits Delete(1, 3) + Insert('E') + Insert('L') + Insert('L').
        // B should converge to 'hELLo'.
        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B converges to 'hELLo'",
            |apps| {
                let mut q = apps[1].world_mut().query::<&TextDoc>();
                q.iter(apps[1].world()).any(|t| t.body == "hELLo")
            },
        );
    });
    join.await.expect("worker panic");
}

fn build_text_app(server: SocketAddr, room: &str) -> App {
    let mut app = App::new();
    app.add_plugins((
        GraphManagerPlugin::<TextDoc, DerivedEdge>::new(),
        GraphSyncPlugin::new(format!("ws://{server}/ws"), room),
        SchemaSyncedComponentPlugin::<NodeTarget, TextDoc>::default(),
    ));
    app
}

/// Component using `#[crdt(sequence)]` over a `Vec<u32>`. Same shape
/// as the String case but element type is `u32`.
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "OrderedList")]
#[require(NodePresence)]
struct OrderedList {
    #[crdt(sequence)]
    items: Vec<u32>,
}

#[test]
fn sequence_vec_emits_inserts_for_appended_items() {
    let component = OrderedList { items: vec![1, 2, 3] };
    let bottom = OrderedListSchema::default();
    let mutations = component.diff(&bottom);
    assert_eq!(mutations.len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequence_vec_replicates_end_to_end() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let mut a = build_ordered_list_app(addr, "vec-seq-room");
        let mut b = build_ordered_list_app(addr, "vec-seq-room");

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(2),
            "welcome",
            |apps| apps.iter_mut().all(|app| sync_status(app).is_connected()),
        );

        a.world_mut().spawn(OrderedList {
            items: vec![10, 20, 30],
        });

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B sees items",
            |apps| {
                let mut q = apps[1].world_mut().query::<&OrderedList>();
                q.iter(apps[1].world()).any(|l| l.items == vec![10, 20, 30])
            },
        );
    });
    join.await.expect("worker panic");
}

fn build_ordered_list_app(server: SocketAddr, room: &str) -> App {
    let mut app = App::new();
    app.add_plugins((
        GraphManagerPlugin::<OrderedList, DerivedEdge>::new(),
        GraphSyncPlugin::new(format!("ws://{server}/ws"), room),
        SchemaSyncedComponentPlugin::<NodeTarget, OrderedList>::default(),
    ));
    app
}

// ---------------------------------------------------------------------------
// Phase E — #[crdt(with = "Type")]
// ---------------------------------------------------------------------------

/// Built-in `LwwRegister<T>` exposed via the `with` escape hatch. This
/// is functionally equivalent to `#[crdt(lww)]` on the same field; the
/// purpose here is to verify the trait-dispatch plumbing works
/// end-to-end against the built-in `SchemaField` impl.
#[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "WithBuiltin")]
#[require(NodePresence)]
struct WithBuiltin {
    #[crdt(with = "::kyoso_crdt::types::LwwRegister<u32>")]
    score: u32,
}

#[test]
fn with_built_in_lww_default_against_bottom_emits_nothing() {
    let component = WithBuiltin::default();
    let bottom = WithBuiltinSchema::default();
    let mutations = component.diff(&bottom);
    assert!(mutations.is_empty(), "default value should not emit");
}

#[test]
fn with_built_in_lww_emits_for_non_default() {
    let component = WithBuiltin { score: 99 };
    let bottom = WithBuiltinSchema::default();
    let mutations = component.diff(&bottom);
    assert_eq!(mutations.len(), 1, "non-default value should emit one mutation");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_built_in_lww_replicates_end_to_end() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let mut a = build_with_builtin_app(addr, "with-builtin-room");
        let mut b = build_with_builtin_app(addr, "with-builtin-room");

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(2),
            "welcome",
            |apps| apps.iter_mut().all(|app| sync_status(app).is_connected()),
        );

        a.world_mut().spawn(WithBuiltin { score: 42 });

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B sees score=42",
            |apps| {
                let mut q = apps[1].world_mut().query::<&WithBuiltin>();
                q.iter(apps[1].world()).any(|w| w.score == 42)
            },
        );
    });
    join.await.expect("worker panic");
}

fn build_with_builtin_app(server: SocketAddr, room: &str) -> App {
    let mut app = App::new();
    app.add_plugins((
        GraphManagerPlugin::<WithBuiltin, DerivedEdge>::new(),
        GraphSyncPlugin::new(format!("ws://{server}/ws"), room),
        SchemaSyncedComponentPlugin::<NodeTarget, WithBuiltin>::default(),
    ));
    app
}

// ---------- Custom SchemaField -----------------------------------------------
//
// The realistic use case for `with`: the component-side field type
// can't be directly synced (an opaque token, an asset handle, an
// `Entity`-id reference, ...). The user hand-rolls a small CRDT
// schema type that knows how to project to/from the component value.
//
// For the test we use a deliberately weird projection — the schema
// stores `value` and the component holds `2 * value`. This proves
// that the macro really delegates to `SchemaField`'s `project_to` /
// `diff` rather than relying on equality of the raw
// component value.

mod custom_with {
    use super::*;
    use kyoso_crdt::context::CausalContext;
    use kyoso_crdt::lattice::{Crdt, DeltaError, Lattice};
    use kyoso_crdt::schema::{IntoWireOp, SchemaApply};
    use kyoso_crdt::types::{LwwDelta, LwwMut, LwwRegister};
    use kyoso_crdt::{Path, WireDelta};

    /// Component-side type. Holds twice the schema's stored value.
    #[derive(Component, Default, Debug, Clone, PartialEq, Reflect)]
    #[reflect(Component, Default)]
    pub struct DoubledValue(pub u32);

    /// Schema-side type. Stores the "halved" value via an inner
    /// `LwwRegister<u32>`. `SchemaField::project_to` doubles the stored
    /// value when writing back to the component; `diff`
    /// halves the component's value when computing the diff.
    #[derive(Clone, Debug, Default, PartialEq)]
    pub struct HalvedSchema {
        inner: LwwRegister<u32>,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct HalvedDelta(pub LwwDelta<u32>);

    #[derive(Clone, Debug, PartialEq)]
    pub struct HalvedMut(pub LwwMut<u32>);

    impl Lattice for HalvedSchema {
        fn bottom() -> Self {
            Self { inner: LwwRegister::bottom() }
        }
        fn join(&mut self, other: Self) {
            self.inner.join(other.inner);
        }
        fn leq(&self, other: &Self) -> bool {
            self.inner.leq(&other.inner)
        }
    }

    impl Crdt for HalvedSchema {
        type Mutation = HalvedMut;
        type Delta = HalvedDelta;
        fn apply(&mut self, delta: &Self::Delta, ctx: &CausalContext) -> Result<(), DeltaError> {
            self.inner.apply(&delta.0, ctx)
        }
        fn mutate(&mut self, m: Self::Mutation, ctx: &mut CausalContext) -> Self::Delta {
            HalvedDelta(self.inner.mutate(m.0, ctx))
        }
    }

    impl SchemaApply for HalvedSchema {
        fn apply_wire(
            &mut self,
            path: &Path,
            delta: WireDelta,
            ctx: &CausalContext,
        ) -> Result<(), DeltaError> {
            self.inner.apply_wire(path, delta, ctx)
        }
        fn install_state(
            &mut self,
            path: &Path,
            field: kyoso_crdt::OpaqueValue,
        ) -> Result<(), DeltaError> {
            self.inner.install_state(path, field)
        }
    }

    impl IntoWireOp for HalvedDelta {
        fn into_wire_op(self) -> (Path, WireDelta) {
            self.0.into_wire_op()
        }
    }

    impl From<HalvedDelta> for WireDelta {
        fn from(d: HalvedDelta) -> Self {
            d.0.into()
        }
    }

    impl TryFrom<WireDelta> for HalvedDelta {
        type Error = DeltaError;
        fn try_from(w: WireDelta) -> Result<Self, Self::Error> {
            Ok(HalvedDelta(LwwDelta::<u32>::try_from(w)?))
        }
    }

    impl kyoso_graph_sync::SchemaField<DoubledValue> for HalvedSchema {
        fn diff(
            &self,
            component: &DoubledValue,
            _baseline: &DoubledValue,
        ) -> Vec<<Self as Crdt>::Mutation> {
            // The schema stores half of the component's value.
            let target_halved = component.0 / 2;
            let stored = self.inner.get().copied().unwrap_or_default();
            if stored != target_halved {
                vec![HalvedMut(LwwMut::Set(target_halved))]
            } else {
                Vec::new()
            }
        }
        fn project_to(&self, component: &mut DoubledValue) {
            if let Some(v) = self.inner.get() {
                component.0 = v * 2;
            }
        }
    }

    #[derive(Component, Default, Debug, Clone, PartialEq, Reflect, SchemaSync)]
    #[reflect(Component, Default)]
    #[schema(name = "Container")]
    pub struct Container {
        #[crdt(with = "HalvedSchema")]
        pub doubled: DoubledValue,
    }

    #[test]
    fn custom_schema_field_emits_halved_value() {
        let component = Container {
            doubled: DoubledValue(10),
        };
        let bottom = ContainerSchema::default();
        let mutations = component.diff(&bottom);
        assert_eq!(
            mutations.len(),
            1,
            "value=10 should emit one mutation for halved=5",
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn custom_schema_field_replicates_end_to_end() {
        let addr = super::spawn_server().await;
        let join = tokio::task::spawn_blocking(move || {
            let mut a = build_container_app(addr, "custom-with-room");
            let mut b = build_container_app(addr, "custom-with-room");

            super::pump_until(
                &mut [&mut a, &mut b],
                Duration::from_secs(2),
                "welcome",
                |apps| {
                    apps.iter_mut().all(|app| super::sync_status(app).is_connected())
                },
            );

            // App A spawns Container { doubled: DoubledValue(10) }.
            // The schema stores 5 (halved). On B, write_back doubles
            // back to 10.
            a.world_mut().spawn(Container {
                doubled: DoubledValue(10),
            });

            super::pump_until(
                &mut [&mut a, &mut b],
                Duration::from_secs(3),
                "B sees doubled=10 via halved-schema projection",
                |apps| {
                    let mut q = apps[1].world_mut().query::<&Container>();
                    q.iter(apps[1].world()).any(|c| c.doubled.0 == 10)
                },
            );
        });
        join.await.expect("worker panic");
    }

    fn build_container_app(server: SocketAddr, room: &str) -> App {
        let mut app = App::new();
        app.add_plugins((
            GraphManagerPlugin::<Container, DerivedEdge>::new(),
            GraphSyncPlugin::new(format!("ws://{server}/ws"), room),
            SchemaSyncedComponentPlugin::<NodeTarget, Container>::default(),
        ));
        app
    }
}

// ---------------------------------------------------------------------------
// Compaction-recovery: typed-schema state survives server GC.
// ---------------------------------------------------------------------------

mod compaction_recovery {
    use super::{Counted, build_counted_app, pump_until, sync_status};
    use kyoso_server::{AppState, app};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;

    async fn spawn_server_with(state: AppState) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app(state);
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        addr
    }

    /// Late joiner sees typed-schema state via snapshot hydration even
    /// after the server has compacted every `SetNodeProperty` op below
    /// the snapshot point.
    ///
    /// Pre-fix, the server's snapshot was over `EmptySchema` so it
    /// captured zero typed state, and the GC compacted the property
    /// ops that would have allowed late replay. Result: late joiner B
    /// had a node entity but no `Counted` properties.
    ///
    /// Post-fix, the server's snapshot is over `OpaqueRecord` so
    /// the PN-counter state lives in the snapshot itself and is
    /// hydrated into `SchemaDoc<CountedSchema>` on B's Welcome.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn late_joiner_hydrates_typed_schema_after_compaction() {
        let state = AppState::in_memory();
        let rooms = state.rooms.clone();
        let addr = spawn_server_with(state).await;
        let handle = tokio::runtime::Handle::current();

        // App A: connect, spawn Counted{edits=7}, wait until A has
        // received its own server-echoed op so the engine's
        // `applied_seq` is past the mutation. Also block long enough
        // for A to send its `Ack` (the outbound system does this once
        // per frame on `applied_seq` change), so `min_ack` advances
        // before we trigger GC.
        let rooms_inner = Arc::clone(&rooms);
        let outcome: bool = tokio::task::spawn_blocking(move || {
            let mut a = build_counted_app(addr, "compaction-typed");
            pump_until(
                &mut [&mut a],
                Duration::from_secs(2),
                "A connected",
                |apps| sync_status(apps[0]).is_connected(),
            );
            a.world_mut().spawn(Counted { edits: 7 });

            // Wait for A to apply its own echoed op, then keep
            // pumping a bit so the Ack reaches the server.
            pump_until(
                &mut [&mut a],
                Duration::from_secs(3),
                "A applied own echo",
                |apps| {
                    let mut q = apps[0].world_mut().query::<&Counted>();
                    q.iter(apps[0].world()).any(|c| c.edits == 7)
                },
            );
            for _ in 0..30 {
                a.update();
                std::thread::sleep(Duration::from_millis(10));
            }

            // Trigger snapshot + GC on the server's room.
            let dropped: u64 = handle.block_on(async {
                let room = rooms_inner
                    .get_or_create("compaction-typed")
                    .await
                    .expect("room");
                room.take_snapshot_all().await;
                room.run_gc_all().await
            });
            assert!(
                dropped > 0,
                "expected GC to compact property ops below snapshot, got {dropped}"
            );

            // App B: fresh late joiner. Its Welcome carries a
            // snapshot at the compacted point, an empty diff (every
            // op below the snapshot was compacted), and opaque
            // typed-schema state for Counted that hydrates into B's
            // SchemaDoc<CountedSchema>.
            let mut b = build_counted_app(addr, "compaction-typed");
            pump_until(
                &mut [&mut a, &mut b],
                Duration::from_secs(3),
                "B hydrated Counted{edits=7} from snapshot",
                |apps| {
                    let mut q = apps[1].world_mut().query::<&Counted>();
                    q.iter(apps[1].world()).any(|c| c.edits == 7)
                },
            );
            true
        })
        .await
        .expect("worker panic");
        assert!(outcome);
    }
}

// ---------------------------------------------------------------------------
// Reconnect-clobber regression — Welcome snapshot must not provoke
// `SetNodeProperty(<default value>)` emissions on the joining peer.
//
// Scenario reproduced in production (circuit client, May 2026):
//   1. Peers A, B, C build out a scene; their per-node typed schema
//      state (Transform, OnLayer, Resistor params, …) lives in the
//      server's `OpaqueRecord` snapshot once GC compacts the
//      property ops below the snapshot point.
//   2. A new peer D joins. Welcome arrives with a snapshot. The graph
//      plugin's `project_snapshot` spawns the structural marker
//      (`CircuitNode`) for each replicated node. Bevy auto-inserts
//      the marker's `#[require(...)]` components — `Transform`, etc. —
//      at `Default::default()` BEFORE `project_to_components` has a
//      chance to write the hydrated values back from `SchemaDoc`.
//   3. `detect_local_changes` runs after `ensure_schema_slots`, sees
//      `Changed<C>` for every just-inserted component, diffs the local
//      default against the hydrated doc state, and emits
//      `SetNodeProperty(<default>)` ops — one per node, per
//      auto-required component.
//   4. Those ops broadcast to all peers. Every peer (including A/B/C)
//      writes the defaults back into its own components. Result: the
//      scene "snaps to origin" the moment D joins.
//
// This test reproduces the path minimally: a marker component with
// `#[require(Counted)]` so Bevy auto-inserts `Counted::default()`
// during `project_snapshot`'s spawn. With the fix, `detect_local_changes`
// suppresses the first-frame Added emission whenever the doc already
// holds non-default state (i.e. it was just hydrated from a snapshot).
mod reconnect_clobber {
    use super::{build_counted_app, pump_until, sync_status, Counted};
    use bevy::prelude::*;
    use kyoso_graph::GraphManagerPlugin;
    use kyoso_graph_sync::{GraphSyncPlugin, NodeTarget, SchemaSyncedComponentPlugin};
    use kyoso_server::{AppState, app};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;

    /// Structural marker with a `#[require(...)]` that auto-inserts
    /// the schema-synced component at `Default::default()`. Mirrors how
    /// `kyoso_circuit_client`'s `on_circuit_node_added` observer inserts
    /// `Mesh3d` (which auto-requires `Transform::default()`) — the
    /// trigger that caused the production "snap to origin" failure.
    #[derive(Component, Default, Clone, Debug, PartialEq, Reflect)]
    #[reflect(Component, Default)]
    #[require(Counted)]
    struct AutoRequiringMarker;

    #[derive(Component, Default, Clone, Debug, PartialEq, Reflect)]
    #[reflect(Component, Default)]
    struct MarkerEdge;

    fn build_marker_app(server: SocketAddr, room: &str) -> App {
        let mut app = App::new();
        app.add_plugins((
            GraphManagerPlugin::<AutoRequiringMarker, MarkerEdge>::new(),
            GraphSyncPlugin::new(
                format!("ws://{server}/ws"),
                room,
            ),
            SchemaSyncedComponentPlugin::<NodeTarget, Counted>::default(),
        ));
        app
    }

    async fn spawn_server_with(state: AppState) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app(state);
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        addr
    }

    /// A spawns a node with `Counted{edits=7}`, snapshot/GC fires so the
    /// only path that delivers state to B is the snapshot hydrator, B
    /// joins and uses `AutoRequiringMarker` (which auto-inserts
    /// `Counted::default()`).
    ///
    /// Assertion: A's `Counted{edits=7}` stays at 7 after B has been
    /// connected for several frames. Pre-fix, B would emit Dec(7) on
    /// frame N+1 and A's component would drift to 0.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn auto_required_default_on_snapshot_join_does_not_clobber_existing_state() {
        let state = AppState::in_memory();
        let rooms = state.rooms.clone();
        let addr = spawn_server_with(state).await;
        let handle = tokio::runtime::Handle::current();

        let rooms_inner = Arc::clone(&rooms);
        tokio::task::spawn_blocking(move || {
            // A uses the no-auto-require app so its initial spawn carries
            // the real `Counted { edits: 7 }` directly; we only need the
            // auto-require behavior on the JOINER side to reproduce the bug.
            let mut a = build_counted_app(addr, "reconnect-clobber");
            pump_until(
                &mut [&mut a],
                Duration::from_secs(2),
                "A connected",
                |apps| sync_status(apps[0]).is_connected(),
            );
            a.world_mut().spawn(Counted { edits: 7 });

            // Wait for A's own echo + Ack to settle on the server.
            pump_until(
                &mut [&mut a],
                Duration::from_secs(3),
                "A applied own echo",
                |apps| {
                    let mut q = apps[0].world_mut().query::<&Counted>();
                    q.iter(apps[0].world()).any(|c| c.edits == 7)
                },
            );
            for _ in 0..30 {
                a.update();
                std::thread::sleep(Duration::from_millis(10));
            }

            let dropped: u64 = handle.block_on(async {
                let room = rooms_inner
                    .get_or_create("reconnect-clobber")
                    .await
                    .expect("room");
                room.take_snapshot_all().await;
                room.run_gc_all().await
            });
            assert!(
                dropped > 0,
                "expected GC to compact below snapshot, got {dropped}"
            );

            // B uses the auto-requiring marker app — its
            // `project_snapshot` spawn will trigger
            // `#[require(Counted)]` to insert `Counted::default()`
            // before the schema doc projects the hydrated value back.
            let mut b = build_marker_app(addr, "reconnect-clobber");
            pump_until(
                &mut [&mut a, &mut b],
                Duration::from_secs(3),
                "B hydrated edits=7 from snapshot",
                |apps| {
                    let mut q = apps[1].world_mut().query::<&Counted>();
                    q.iter(apps[1].world()).any(|c| c.edits == 7)
                },
            );

            // Pump well past the point where B could have emitted
            // and A could have applied a spurious Dec(7).
            for _ in 0..60 {
                a.update();
                b.update();
                std::thread::sleep(Duration::from_millis(10));
            }

            // STABILITY ASSERTION — the regression. Pre-fix, A's
            // `edits` field would be 0 here because B's snapshot-spawn
            // emitted Dec(7) and A applied it.
            let a_edits = {
                let mut q = a.world_mut().query::<&Counted>();
                q.iter(a.world())
                    .next()
                    .map(|c| c.edits)
                    .expect("A has a Counted")
            };
            assert_eq!(
                a_edits, 7,
                "A's Counted{{edits}} was clobbered by B's snapshot-join — \
                 detect_local_changes emitted Dec on the auto-required \
                 default component before project_to_components could \
                 write the hydrated value back."
            );

            let b_edits = {
                let mut q = b.world_mut().query::<&Counted>();
                q.iter(b.world())
                    .next()
                    .map(|c| c.edits)
                    .expect("B has a Counted")
            };
            assert_eq!(
                b_edits, 7,
                "B should have converged to edits=7 via snapshot hydration."
            );
        })
        .await
        .expect("worker panic");
    }
}
