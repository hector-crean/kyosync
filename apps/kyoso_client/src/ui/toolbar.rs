//! Bottom-centered Figma-style toolbar.
//!
//! Reads tool metadata off the [`Tool`](crate::tool::Tool) enum
//! (label + shortcut, via `strum` properties) and spawns one button
//! per variant. Click → `AppCommand::SetTool(variant)` written into
//! the duplex bus — same code path as JS / MCP / agent calls. Active
//! tool gets a brighter background; the system reads `State<Tool>`
//! every frame and updates colours.
//!
//! Intentionally text-based for v1 — no asset-server dependency, no
//! icon files to bundle. Swap text for `ImageNode` when you want
//! Figma-grade icons.

use bevy::prelude::*;

use crate::msg::AppCommand;
use crate::tool::{ConnectKind, Tool};
use crate::weave::WeaveEdgeKind;

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

/// Marker on a toolbar button. Carries the `Tool` it represents so the
/// click handler can write the right `AppCommand`.
#[derive(Component, Clone, Copy)]
struct ToolbarButton(Tool);

/// Marker on a per-weave-kind Connect button. Click → set
/// `Tool::Connect` AND `ConnectKind(kind)`.
#[derive(Component, Clone, Copy)]
struct WeaveKindButton(WeaveEdgeKind);

const WEAVE_KINDS: &[WeaveEdgeKind] = &[
    WeaveEdgeKind::Reference,
    WeaveEdgeKind::Dependency,
    WeaveEdgeKind::Comment,
    WeaveEdgeKind::Annotation,
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
                handle_weave_kind_clicks,
                sync_button_visuals,
                sync_weave_kind_visuals,
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
                    // Visual divider before the weave-kind cluster.
                    pill.spawn((
                        Node {
                            width: Val::Px(1.0),
                            height: Val::Px(BUTTON_HEIGHT - 6.0),
                            margin: UiRect::axes(Val::Px(4.0), Val::Px(0.0)),
                            ..default()
                        },
                        BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.18)),
                    ));
                    for &kind in WEAVE_KINDS {
                        spawn_weave_kind_button(pill, kind);
                    }
                });
        });
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

/// Translate a button click into an `AppCommand::SetTool` written into
/// the duplex bus. Routing through the bus (instead of mutating
/// `NextState<Tool>` directly) means the toolbar uses the same code
/// path as JS / MCP / agent callers — observable in one place.
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

/// A weave-kind button click sets `Tool::Connect` *and* the active
/// `ConnectKind`. Both writes go through the duplex bus.
fn handle_weave_kind_clicks(
    mut commands_w: MessageWriter<AppCommand>,
    buttons: Query<(&Interaction, &WeaveKindButton), Changed<Interaction>>,
) {
    for (interaction, button) in &buttons {
        if matches!(interaction, Interaction::Pressed) {
            commands_w.write(AppCommand::SetTool(Tool::Connect));
            commands_w.write(AppCommand::SetConnectKind(button.0));
        }
    }
}

fn spawn_weave_kind_button(
    parent: &mut bevy::ecs::relationship::RelatedSpawnerCommands<ChildOf>,
    kind: WeaveEdgeKind,
) {
    parent
        .spawn((
            Button,
            WeaveKindButton(kind),
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
            // A tiny coloured swatch matching the edge kind's render
            // colour, so the toolbar reads at a glance.
            btn.spawn((
                Node {
                    width: Val::Px(10.0),
                    height: Val::Px(10.0),
                    border_radius: BorderRadius::all(Val::Px(2.0)),
                    ..default()
                },
                BackgroundColor(kind.color()),
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

fn sync_weave_kind_visuals(
    state: Res<State<Tool>>,
    connect_kind: Res<ConnectKind>,
    mut buttons: Query<(&Interaction, &WeaveKindButton, &mut BackgroundColor)>,
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

/// Per-frame: paint each button's background based on (a) is it the
/// currently-active tool, (b) is the cursor hovering it. Active beats
/// hover beats normal.
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
