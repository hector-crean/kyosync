//! Create tool — spawn new figma frames at the cursor.
//!
//! In the figma+weave hybrid the Create tool drops a new `Frame` at
//! the click position. The frame is sized 180×100 by default and
//! filled with the colour passed in the `Rgb` argument. Subsequent
//! drag tools move it around; the Connect-X tools attach typed edges
//! between two frames.
//!
//! Active commands:
//! - `SpawnNodeAt { position, color }` — programmatic spawn at a given
//!   world coord. The intent path agents / MCP / JS take.
//! - `SpawnNodeAtCursor { color }` — UI-driven spawn at the current
//!   pointer position. The handler resolves cursor → world via the
//!   active 2D camera and forwards as a `SpawnNodeAt`.

use bevy::prelude::*;
use kyoso_core::paint::Paint;
use kyoso_core::{Frame, SceneNode, Size};
use serde::{Deserialize, Serialize};

use crate::msg::{AppCommand, AppEvent, Pos2, Rgb};
use crate::tool::{Tool, ToolCommand};

#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "op", content = "args")]
pub enum CreateCommand {
    SpawnNodeAt { position: Pos2, color: Rgb },
    SpawnNodeAtCursor { color: Rgb },
}

#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum CreateEvent {
    NodeSpawned { entity: u64 },
}

pub struct CreateToolPlugin;

impl Plugin for CreateToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<CreateCommand>();
        app.add_message::<CreateEvent>();
        app.add_systems(
            Update,
            (
                handle_create_commands.run_if(in_state(Tool::Create)),
                handle_canvas_clicks.run_if(in_state(Tool::Create)),
            ),
        );
    }
}

fn handle_create_commands(
    mut commands: Commands,
    mut reader: MessageReader<CreateCommand>,
    mut events: MessageWriter<AppEvent>,
    mut create_events: MessageWriter<CreateEvent>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
) {
    for cmd in reader.read() {
        match cmd {
            CreateCommand::SpawnNodeAt { position, color } => {
                let entity = spawn_node(&mut commands, *position, *color);
                create_events.write(CreateEvent::NodeSpawned {
                    entity: entity.to_bits(),
                });
            }
            CreateCommand::SpawnNodeAtCursor { color } => {
                let Some(world) = cursor_to_world(&windows, &cameras) else {
                    events.write(AppEvent::CommandError {
                        message: "Create::SpawnNodeAtCursor: no cursor or no 2D camera".into(),
                    });
                    continue;
                };
                let entity = spawn_node(&mut commands, world.into(), *color);
                create_events.write(CreateEvent::NodeSpawned {
                    entity: entity.to_bits(),
                });
            }
        }
    }
}

fn spawn_node(commands: &mut Commands, position: Pos2, color: Rgb) -> Entity {
    commands
        .spawn((
            SceneNode,
            Frame {
                name: String::new(),
                clips_content: true,
                layout_mode: kyoso_core::LayoutMode::None,
                fills: vec![Paint::Solid {
                    color: [color.r, color.g, color.b, 1.0],
                }],
                strokes: vec![],
                stroke_weight: 0.0,
            },
            Size {
                width: 180.0,
                height: 100.0,
            },
            Transform::from_xyz(position.x, position.y, 0.0),
            // `kyoso_core::Frame` doesn't `#[require(Visibility)]` because
            // the kyoso_core crate uses bevy with `default-features =
            // false` (no bevy_render). Add it here so the mesh enters the
            // render / picking pipeline.
            Visibility::default(),
        ))
        .id()
}

fn cursor_to_world(
    windows: &Query<&Window>,
    cameras: &Query<(&Camera, &GlobalTransform), With<Camera2d>>,
) -> Option<Vec2> {
    let window = windows.iter().next()?;
    let cursor = window.cursor_position()?;
    let (camera, cam_t) = cameras.iter().next()?;
    camera.viewport_to_world_2d(cam_t, cursor).ok()
}

/// While `Tool::Create` is active, a left mouse click on world space
/// writes `AppCommand::Tool(Create(SpawnNodeAtCursor { color }))`.
///
/// Skips the click if any UI element is currently being interacted with
/// (otherwise a toolbar click would also spawn a node).
fn handle_canvas_clicks(
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    interactions: Query<&Interaction>,
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
    commands_w.write(AppCommand::Tool(ToolCommand::Create(
        CreateCommand::SpawnNodeAtCursor {
            color: Rgb {
                r: 0.4,
                g: 0.7,
                b: 1.0,
            },
        },
    )));
}
