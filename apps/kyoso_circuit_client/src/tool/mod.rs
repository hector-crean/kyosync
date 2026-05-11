//! Tools — orthogonal interaction modes a la Figma.
//!
//! Each [`Tool`] variant is a top-level [`States`] entry. Per-tool
//! interaction logic lives in its own plugin (`SelectToolPlugin`,
//! `PlaceToolPlugin`, `ConnectToolPlugin`) with systems gated by
//! `.run_if(in_state(Tool::X))`. Each tool also defines its own
//! `Command` enum that flows through the same Duplex bridge as the
//! top-level [`AppCommand`](crate::msg::AppCommand).

pub mod connect;
pub mod place;
pub mod select;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use strum::{EnumIter, EnumProperty, IntoEnumIterator};

pub use connect::{ConnectCommand, ConnectEvent, ConnectKind, ConnectState, ConnectToolPlugin};
pub use place::{PlaceCommand, PlaceEvent, PlaceKind, PlaceLayer, PlaceToolPlugin};
pub use select::{SelectCommand, SelectEvent, SelectToolPlugin};

/// Top-level interaction mode. Exactly one is active at a time.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    Hash,
    States,
    Serialize,
    Deserialize,
    EnumIter,
    EnumProperty,
)]
pub enum Tool {
    /// Click to select, drag to move (default).
    #[default]
    #[strum(props(label = "Select", shortcut = "V"))]
    Select,

    /// Click empty canvas to spawn a new component of the active kind.
    #[strum(props(label = "Place", shortcut = "P"))]
    Place,

    /// Click two components in succession to add an edge between them.
    #[strum(props(label = "Connect", shortcut = "E"))]
    Connect,
}

impl Tool {
    pub fn label(&self) -> &'static str {
        self.get_str("label").unwrap_or("?")
    }

    pub fn shortcut(&self) -> Option<&'static str> {
        self.get_str("shortcut")
    }

    pub fn shortcut_keycode(&self) -> Option<KeyCode> {
        let s = self.shortcut()?;
        keycode_from_letter(s)
    }

    pub fn all() -> impl Iterator<Item = Self> {
        Self::iter()
    }
}

fn keycode_from_letter(s: &str) -> Option<KeyCode> {
    let mut chars = s.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(match c.to_ascii_uppercase() {
        'A' => KeyCode::KeyA,
        'B' => KeyCode::KeyB,
        'C' => KeyCode::KeyC,
        'D' => KeyCode::KeyD,
        'E' => KeyCode::KeyE,
        'F' => KeyCode::KeyF,
        'G' => KeyCode::KeyG,
        'H' => KeyCode::KeyH,
        'I' => KeyCode::KeyI,
        'J' => KeyCode::KeyJ,
        'K' => KeyCode::KeyK,
        'L' => KeyCode::KeyL,
        'M' => KeyCode::KeyM,
        'N' => KeyCode::KeyN,
        'O' => KeyCode::KeyO,
        'P' => KeyCode::KeyP,
        'Q' => KeyCode::KeyQ,
        'R' => KeyCode::KeyR,
        'S' => KeyCode::KeyS,
        'T' => KeyCode::KeyT,
        'U' => KeyCode::KeyU,
        'V' => KeyCode::KeyV,
        'W' => KeyCode::KeyW,
        'X' => KeyCode::KeyX,
        'Y' => KeyCode::KeyY,
        'Z' => KeyCode::KeyZ,
        _ => return None,
    })
}

/// Aggregator for every per-tool command. Lives on the umbrella
/// [`AppCommand::Tool`](crate::msg::AppCommand::Tool) variant so the
/// external API surface has one slot per plugin instead of one per tool.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "tool", content = "command")]
pub enum ToolCommand {
    Select(SelectCommand),
    Place(PlaceCommand),
    Connect(ConnectCommand),
}

/// Aggregator for every per-tool event.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum ToolEvent {
    Switched { tool: Tool },
    Select(SelectEvent),
    Place(PlaceEvent),
    Connect(ConnectEvent),
}

/// Plugin that registers the `Tool` state and adds the per-tool plugins.
pub struct ToolsPlugin;

impl Plugin for ToolsPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<bevy::state::app::StatesPlugin>() {
            app.add_plugins(bevy::state::app::StatesPlugin);
        }
        app.init_state::<Tool>();
        app.add_message::<ToolCommand>();
        app.add_message::<ToolEvent>();
        app.add_plugins((SelectToolPlugin, PlaceToolPlugin, ConnectToolPlugin));
    }
}
