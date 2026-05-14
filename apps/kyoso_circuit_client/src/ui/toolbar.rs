//! Bottom-centered Figma-style toolbar.
//!
//! Reads tool metadata off the [`Tool`](crate::tool::Tool) enum
//! (label + shortcut) and spawns one button per variant. Clicks emit
//! `AppCommand::SetTool(variant)` — same code path as FFI / MCP / agent
//! calls. Component-kind buttons (Resistor / Capacitor / …) and
//! wire-kind buttons (Wire / Same-Net / Diff-Pair) live next to the
//! tool buttons; clicking either also switches into the matching tool.

use bevy::prelude::*;
use kyoso_circuit::{CircuitEdgeKind, CircuitLayer, ComponentKind};

use crate::msg::AppCommand;
use crate::tool::{ConnectKind, PlaceKind, Tool};
use crate::LayerManager;

// --- Design tokens --------------------------------------------------------

const TOOLBAR_BG: Color = Color::srgba(0.118, 0.118, 0.118, 0.92);
const TOOLBAR_BOTTOM_MARGIN: f32 = 16.0;
const TOOLBAR_PADDING: f32 = 6.0;
const TOOLBAR_RADIUS: f32 = 10.0;

const BUTTON_HEIGHT: f32 = 34.0;
const BUTTON_PAD_X: f32 = 12.0;
const BUTTON_GAP: f32 = 4.0;
const BUTTON_RADIUS: f32 = 6.0;

const BUTTON_NORMAL: Color = Color::NONE;
const BUTTON_HOVER: Color = Color::srgba(1.0, 1.0, 1.0, 0.08);
const BUTTON_ACTIVE: Color = Color::srgba(0.30, 0.55, 0.95, 0.40);

const LABEL_SIZE: f32 = 13.0;
const SHORTCUT_SIZE: f32 = 10.0;
const SHORTCUT_COLOR: Color = Color::srgba(1.0, 1.0, 1.0, 0.5);

// --- Components -----------------------------------------------------------

#[derive(Component)]
struct ToolbarRoot;

#[derive(Component, Clone, Copy)]
struct ToolbarButton(Tool);

#[derive(Component, Clone, Copy)]
struct ComponentKindButton(ComponentKind);

#[derive(Component, Clone, Copy)]
struct WireKindButton(CircuitEdgeKind);

/// Layer-selector button. Clicking it sets the active board layer for
/// future place-tool spawns. Independent of `Tool` — the layer applies
/// regardless of which tool is active so the user can pre-pick the
/// layer before switching to Place.
#[derive(Component, Clone, Copy)]
struct LayerButton(CircuitLayer);

const WIRE_KINDS: &[CircuitEdgeKind] = &[
    CircuitEdgeKind::Wire,
    CircuitEdgeKind::SameNet,
    CircuitEdgeKind::DifferentialPair,
];

// --- Plugin ----------------------------------------------------------------

pub struct ToolbarPlugin;

impl Plugin for ToolbarPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_toolbar);
        app.add_systems(
            Update,
            (
                handle_button_clicks,
                handle_component_kind_clicks,
                handle_wire_kind_clicks,
                handle_layer_clicks,
                sync_button_visuals,
                sync_component_kind_visuals,
                sync_wire_kind_visuals,
                sync_layer_visuals,
            ),
        );
    }
}

fn spawn_toolbar(mut commands: Commands) {
    commands
        .spawn((
            ToolbarRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Auto,
                bottom: Val::Px(TOOLBAR_BOTTOM_MARGIN),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::FlexEnd,
                ..default()
            },
        ))
        .with_children(|outer| {
            outer
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(BUTTON_GAP),
                        padding: UiRect::all(Val::Px(TOOLBAR_PADDING)),
                        border_radius: BorderRadius::all(Val::Px(TOOLBAR_RADIUS)),
                        ..default()
                    },
                    BackgroundColor(TOOLBAR_BG),
                ))
                .with_children(|pill| {
                    for tool in Tool::all() {
                        spawn_tool_button(pill, tool);
                    }
                    spawn_divider(pill);
                    for kind in ComponentKind::all() {
                        spawn_component_kind_button(pill, kind);
                    }
                    spawn_divider(pill);
                    for &kind in WIRE_KINDS {
                        spawn_wire_kind_button(pill, kind);
                    }
                    spawn_divider(pill);
                    for layer in CircuitLayer::all() {
                        spawn_layer_button(pill, layer);
                    }
                });
        });
}

fn spawn_divider(parent: &mut bevy::ecs::relationship::RelatedSpawnerCommands<ChildOf>) {
    parent.spawn((
        Node {
            width: Val::Px(1.0),
            height: Val::Px(BUTTON_HEIGHT - 6.0),
            margin: UiRect::axes(Val::Px(4.0), Val::Px(0.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.18)),
    ));
}

fn spawn_tool_button(
    parent: &mut bevy::ecs::relationship::RelatedSpawnerCommands<ChildOf>,
    tool: Tool,
) {
    parent
        .spawn((
            Button,
            ToolbarButton(tool),
            Node {
                height: Val::Px(BUTTON_HEIGHT),
                padding: UiRect::axes(Val::Px(BUTTON_PAD_X), Val::Px(0.0)),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                border_radius: BorderRadius::all(Val::Px(BUTTON_RADIUS)),
                ..default()
            },
            BackgroundColor(BUTTON_NORMAL),
        ))
        .with_children(|btn| {
            btn.spawn((
                Text::new(tool.label()),
                TextFont {
                    font_size: FontSize::Px(LABEL_SIZE),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            if let Some(sc) = tool.shortcut() {
                btn.spawn((
                    Text::new(sc),
                    TextFont {
                        font_size: FontSize::Px(SHORTCUT_SIZE),
                        ..default()
                    },
                    TextColor(SHORTCUT_COLOR),
                ));
            }
        });
}

fn spawn_component_kind_button(
    parent: &mut bevy::ecs::relationship::RelatedSpawnerCommands<ChildOf>,
    kind: ComponentKind,
) {
    parent
        .spawn((
            Button,
            ComponentKindButton(kind),
            Node {
                height: Val::Px(BUTTON_HEIGHT),
                padding: UiRect::axes(Val::Px(BUTTON_PAD_X), Val::Px(0.0)),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                border_radius: BorderRadius::all(Val::Px(BUTTON_RADIUS)),
                ..default()
            },
            BackgroundColor(BUTTON_NORMAL),
        ))
        .with_children(|btn| {
            btn.spawn((
                Text::new(kind.label()),
                TextFont {
                    font_size: FontSize::Px(LABEL_SIZE),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
        });
}

fn spawn_wire_kind_button(
    parent: &mut bevy::ecs::relationship::RelatedSpawnerCommands<ChildOf>,
    kind: CircuitEdgeKind,
) {
    let c = kind.color_srgb();
    parent
        .spawn((
            Button,
            WireKindButton(kind),
            Node {
                height: Val::Px(BUTTON_HEIGHT),
                padding: UiRect::axes(Val::Px(BUTTON_PAD_X), Val::Px(0.0)),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                border_radius: BorderRadius::all(Val::Px(BUTTON_RADIUS)),
                ..default()
            },
            BackgroundColor(BUTTON_NORMAL),
        ))
        .with_children(|btn| {
            btn.spawn((
                Node {
                    width: Val::Px(10.0),
                    height: Val::Px(10.0),
                    border_radius: BorderRadius::all(Val::Px(2.0)),
                    ..default()
                },
                BackgroundColor(Color::srgb(c[0], c[1], c[2])),
            ));
            btn.spawn((
                Text::new(kind.label()),
                TextFont {
                    font_size: FontSize::Px(LABEL_SIZE),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
        });
}

fn handle_button_clicks(
    mut commands_w: MessageWriter<AppCommand>,
    buttons: Query<(&Interaction, &ToolbarButton), Changed<Interaction>>,
) {
    for (interaction, button) in &buttons {
        if matches!(interaction, Interaction::Pressed) {
            commands_w.write(AppCommand::SetTool(button.0));
        }
    }
}

/// Click on a component-kind button → switch into Place tool AND set
/// the active `ComponentKind`. Both writes go through the duplex bus.
fn handle_component_kind_clicks(
    mut commands_w: MessageWriter<AppCommand>,
    buttons: Query<(&Interaction, &ComponentKindButton), Changed<Interaction>>,
) {
    for (interaction, button) in &buttons {
        if matches!(interaction, Interaction::Pressed) {
            commands_w.write(AppCommand::SetTool(Tool::Place));
            commands_w.write(AppCommand::SetComponentKind(button.0));
        }
    }
}

/// Click on a wire-kind button → switch into Connect tool AND set the
/// active `CircuitEdgeKind`.
fn handle_wire_kind_clicks(
    mut commands_w: MessageWriter<AppCommand>,
    buttons: Query<(&Interaction, &WireKindButton), Changed<Interaction>>,
) {
    for (interaction, button) in &buttons {
        if matches!(interaction, Interaction::Pressed) {
            commands_w.write(AppCommand::SetTool(Tool::Connect));
            commands_w.write(AppCommand::SetWireKind(button.0));
        }
    }
}

fn sync_button_visuals(
    state: Res<State<Tool>>,
    mut buttons: Query<(&Interaction, &ToolbarButton, &mut BackgroundColor)>,
) {
    let active = *state.get();
    for (interaction, button, mut bg) in &mut buttons {
        let is_active = button.0 == active;
        *bg = match (is_active, interaction) {
            (true, _) => BUTTON_ACTIVE.into(),
            (false, Interaction::Hovered | Interaction::Pressed) => BUTTON_HOVER.into(),
            (false, Interaction::None) => BUTTON_NORMAL.into(),
        };
    }
}

fn sync_component_kind_visuals(
    state: Res<State<Tool>>,
    place_kind: Res<PlaceKind>,
    mut buttons: Query<(&Interaction, &ComponentKindButton, &mut BackgroundColor)>,
) {
    let place_active = matches!(*state.get(), Tool::Place);
    for (interaction, button, mut bg) in &mut buttons {
        let is_active = place_active && button.0 == place_kind.0;
        *bg = match (is_active, interaction) {
            (true, _) => BUTTON_ACTIVE.into(),
            (false, Interaction::Hovered | Interaction::Pressed) => BUTTON_HOVER.into(),
            (false, Interaction::None) => BUTTON_NORMAL.into(),
        };
    }
}

fn spawn_layer_button(
    parent: &mut bevy::ecs::relationship::RelatedSpawnerCommands<ChildOf>,
    layer: CircuitLayer,
) {
    let c = layer.color_srgb();
    parent
        .spawn((
            Button,
            LayerButton(layer),
            Node {
                height: Val::Px(BUTTON_HEIGHT),
                padding: UiRect::axes(Val::Px(BUTTON_PAD_X), Val::Px(0.0)),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                border_radius: BorderRadius::all(Val::Px(BUTTON_RADIUS)),
                ..default()
            },
            BackgroundColor(BUTTON_NORMAL),
        ))
        .with_children(|btn| {
            btn.spawn((
                Node {
                    width: Val::Px(10.0),
                    height: Val::Px(10.0),
                    border_radius: BorderRadius::all(Val::Px(2.0)),
                    ..default()
                },
                BackgroundColor(Color::srgb(c[0], c[1], c[2])),
            ));
            btn.spawn((
                Text::new(layer.label()),
                TextFont {
                    font_size: FontSize::Px(LABEL_SIZE),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
        });
}

/// Click on a layer button → set the active board layer. Does NOT
/// switch tools; the same layer applies whether the user is placing,
/// connecting, or selecting.
fn handle_layer_clicks(
    mut commands_w: MessageWriter<AppCommand>,
    buttons: Query<(&Interaction, &LayerButton), Changed<Interaction>>,
) {
    for (interaction, button) in &buttons {
        if matches!(interaction, Interaction::Pressed) {
            commands_w.write(AppCommand::SetActiveLayer(button.0));
        }
    }
}

fn sync_layer_visuals(
    layer_manager: Res<LayerManager>,
    mut buttons: Query<(&Interaction, &LayerButton, &mut BackgroundColor)>,
) {
    for (interaction, button, mut bg) in &mut buttons {
        let is_active = button.0 == layer_manager.active();
        *bg = match (is_active, interaction) {
            (true, _) => BUTTON_ACTIVE.into(),
            (false, Interaction::Hovered | Interaction::Pressed) => BUTTON_HOVER.into(),
            (false, Interaction::None) => BUTTON_NORMAL.into(),
        };
    }
}

fn sync_wire_kind_visuals(
    state: Res<State<Tool>>,
    connect_kind: Res<ConnectKind>,
    mut buttons: Query<(&Interaction, &WireKindButton, &mut BackgroundColor)>,
) {
    let connect_active = matches!(*state.get(), Tool::Connect);
    for (interaction, button, mut bg) in &mut buttons {
        let is_active = connect_active && button.0 == connect_kind.0;
        *bg = match (is_active, interaction) {
            (true, _) => BUTTON_ACTIVE.into(),
            (false, Interaction::Hovered | Interaction::Pressed) => BUTTON_HOVER.into(),
            (false, Interaction::None) => BUTTON_NORMAL.into(),
        };
    }
}
