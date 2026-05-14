//! Place tool — spawn new circuit components in 3D.
//!
//! In 3D the place tool drops a new circuit-component entity (Resistor
//! / Capacitor / …) at the world position where the cursor ray
//! intersects the active layer's xz-plane. The component kind comes
//! from [`PlaceKind`]; the target board layer comes from the shared
//! [`LayerManager`](crate::LayerManager) so the same "currently active
//! layer" notion is read by place / connect / grid alike.
//!
//! Active commands:
//! - `SpawnAt { position, kind, layer }` — programmatic spawn at a
//!   given world coord on a specific board layer. The intent path
//!   agents / MCP / FFI take.
//! - `SpawnAtCursor { kind, layer }` — UI-driven spawn. The handler
//!   resolves cursor → world via the camera's `RayMap` against the
//!   layer's xz-plane and forwards as a `SpawnAt`.

use bevy::picking::backend::ray::RayMap;
use bevy::picking::pointer::PointerId;
use bevy::prelude::*;
use kyoso_camera::markers::MainCamera;
use kyoso_camera::raycast::RayMapExt;
use kyoso_circuit::{CircuitLayer, CircuitNode, ComponentKind, OnLayer};
use serde::{Deserialize, Serialize};

use crate::msg::{AppCommand, AppEvent, Pos3};
use crate::tool::{Tool, ToolCommand};

/// The active component kind for the place tool. Set by the toolbar UI
/// when the user clicks one of the Place-X buttons.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct PlaceKind(pub ComponentKind);

#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "op", content = "args")]
pub enum PlaceCommand {
    SpawnAt {
        position: Pos3,
        kind: ComponentKind,
        layer: CircuitLayer,
    },
    SpawnAtCursor {
        kind: ComponentKind,
        layer: CircuitLayer,
    },
}

#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum PlaceEvent {
    /// A component was spawned. The Bevy entity bits are exposed for
    /// debug; external observers should rely on `AppEvent::Graph` for
    /// CrdtId-keyed identity.
    ComponentSpawned {
        entity: u64,
        kind: ComponentKind,
        layer: CircuitLayer,
    },
}

pub struct PlaceToolPlugin;

impl Plugin for PlaceToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PlaceCommand>();
        app.add_message::<PlaceEvent>();
        app.init_resource::<PlaceKind>();
        app.add_systems(
            Update,
            (
                handle_place_commands.run_if(in_state(Tool::Place)),
                handle_canvas_clicks.run_if(in_state(Tool::Place)),
            ),
        );
    }
}

fn handle_place_commands(
    mut commands: Commands,
    mut reader: MessageReader<PlaceCommand>,
    mut events: MessageWriter<AppEvent>,
    mut place_events: MessageWriter<PlaceEvent>,
    // `RayMap` is added by Bevy's picking plugin (part of `DefaultPlugins`
    // / `MeshPickingPlugin`); it isn't present in the headless test
    // setup that uses `MinimalPlugins`. Wrap in `Option` so programmatic
    // `SpawnAt` (no cursor needed) keeps working in those contexts.
    ray_map: Option<Res<RayMap>>,
    cameras: Query<Entity, With<MainCamera>>,
    grid_manager: Option<Res<crate::GridManager>>,
) {
    for cmd in reader.read() {
        let grid_layout = grid_manager.as_deref().map(|g| *g.layout(match cmd {
            PlaceCommand::SpawnAt { layer, .. } | PlaceCommand::SpawnAtCursor { layer, .. } => *layer,
        }));
        match cmd {
            PlaceCommand::SpawnAt {
                position,
                kind,
                layer,
            } => {
                let entity = spawn_component_with_grid(
                    &mut commands,
                    *position,
                    *kind,
                    *layer,
                    grid_layout.as_ref(),
                );
                place_events.write(PlaceEvent::ComponentSpawned {
                    entity: entity.to_bits(),
                    kind: *kind,
                    layer: *layer,
                });
            }
            PlaceCommand::SpawnAtCursor { kind, layer } => {
                let Some(ray_map) = ray_map.as_deref() else {
                    events.write(AppEvent::CommandError {
                        message:
                            "Place::SpawnAtCursor: picking pipeline not present (headless?)"
                                .into(),
                    });
                    continue;
                };
                let Some(world) = cursor_to_layer_world(ray_map, &cameras, *layer) else {
                    events.write(AppEvent::CommandError {
                        message: "Place::SpawnAtCursor: no cursor / camera ray / layer plane intersection".into(),
                    });
                    continue;
                };
                let entity = spawn_component_with_grid(
                    &mut commands,
                    world.into(),
                    *kind,
                    *layer,
                    grid_layout.as_ref(),
                );
                place_events.write(PlaceEvent::ComponentSpawned {
                    entity: entity.to_bits(),
                    kind: *kind,
                    layer: *layer,
                });
            }
        }
    }
}

/// Spawn a circuit-component entity carrying:
/// - the structural marker [`CircuitNode`]
/// - the per-kind schema component (with non-default preset values so
///   `SchemaSync` actually emits replication ops — see the comment in
///   the body)
/// - an [`OnLayer`] tagging the board layer (also schema-synced)
/// - a `Transform` snapped to the layer's y-offset
///
/// Promoted to a shared helper so [`crate::handlers::dispatch_app_commands`]
/// can call it without going through the place tool's gated systems.
///
/// The non-default presets (1 kΩ resistor, 1 µF capacitor, 1 mH
/// inductor, 5 V source, "GND" ground, layer = `layer.id()` ≠ 0) are
/// what makes `SchemaSync` see a non-default value and emit ops; with
/// `Default::default()` for every field, no replication would happen
/// and remote peers would only receive the structural `CircuitNode`
/// without the kind-specific schema or the layer assignment.
pub fn spawn_component(
    commands: &mut Commands,
    position: Pos3,
    kind: ComponentKind,
    layer: CircuitLayer,
) -> Entity {
    spawn_component_with_grid(commands, position, kind, layer, None)
}

/// Same as [`spawn_component`] but with an optional grid layout used
/// to snap the spawn position to a cell centre on the target layer's
/// xz-plane. The default-grid path (`grid = None`) preserves the
/// existing behaviour for programmatic / agent callers that don't
/// hold the [`GridManager`] resource handle.
pub fn spawn_component_with_grid(
    commands: &mut Commands,
    position: Pos3,
    kind: ComponentKind,
    layer: CircuitLayer,
    grid: Option<&crate::GridLayout3d>,
) -> Entity {
    // Snap y to the layer's offset regardless of the supplied
    // position.y — the cursor raycast already does this for click
    // spawns; programmatic SpawnAt callers don't have to bother.
    let raw_world = Vec3::new(position.x, layer.y_offset(), position.z);
    let world = match grid {
        Some(layout) => layout.snap(raw_world),
        None => raw_world,
    };
    let mut e = commands.spawn((
        CircuitNode,
        Transform::from_translation(world),
        Visibility::default(),
        OnLayer::new(layer),
    ));
    match kind {
        ComponentKind::Resistor => {
            e.insert(kyoso_circuit::Resistor {
                resistance_ohms: 1_000.0,
            });
        }
        ComponentKind::Capacitor => {
            e.insert(kyoso_circuit::Capacitor {
                capacitance_farads: 1.0e-6,
            });
        }
        ComponentKind::Inductor => {
            e.insert(kyoso_circuit::Inductor {
                inductance_henries: 1.0e-3,
            });
        }
        ComponentKind::VoltageSource => {
            e.insert(kyoso_circuit::VoltageSource {
                voltage_volts: 5.0,
            });
        }
        ComponentKind::Ground => {
            e.insert(kyoso_circuit::Ground {
                label: "GND".into(),
            });
        }
    }
    e.id()
}

/// Resolve the mouse pointer to a 3D world point on the active layer's
/// xz-plane (Y = `layer.y_offset()`, normal = +Y). Returns `None` if
/// the picking pipeline hasn't computed a ray for the main camera /
/// pointer this frame, or the ray is parallel to the plane.
fn cursor_to_layer_world(
    ray_map: &RayMap,
    cameras: &Query<Entity, With<MainCamera>>,
    layer: CircuitLayer,
) -> Option<Vec3> {
    let camera = cameras.iter().next()?;
    ray_map.pointer_plane_intersection(
        camera,
        PointerId::Mouse,
        Vec3::new(0.0, layer.y_offset(), 0.0),
        Vec3::Y,
    )
}

/// While `Tool::Place` is active, a left mouse click on world space
/// writes `AppCommand::Tool(Place(SpawnAtCursor { kind, layer }))`.
/// Skips the click if any UI element is interactive.
fn handle_canvas_clicks(
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    interactions: Query<&Interaction>,
    place_kind: Res<PlaceKind>,
    layer_manager: Res<crate::LayerManager>,
    mut commands_w: MessageWriter<AppCommand>,
) {
    let Some(mouse) = mouse else {
        return;
    };
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    if interactions
        .iter()
        .any(|i| !matches!(i, Interaction::None))
    {
        return;
    }
    commands_w.write(AppCommand::Tool(ToolCommand::Place(
        PlaceCommand::SpawnAtCursor {
            kind: place_kind.0,
            layer: layer_manager.active(),
        },
    )));
}
