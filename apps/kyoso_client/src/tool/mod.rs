//! Tools — orthogonal interaction modes a la Figma.
//!
//! Each [`Tool`] variant is a top-level [`States`] entry. Per-tool
//! interaction logic lives in its own plugin (`SelectToolPlugin`,
//! `CreateToolPlugin`, `ConnectToolPlugin`, …) with systems gated by
//! `.run_if(in_state(Tool::X))`. Each tool also defines its own
//! `Command` enum that flows through the same Duplex bridge as the
//! top-level [`AppCommand`](crate::msg::AppCommand) — so
//! agents/MCP/JS push
//! `AppCommand::Tool(ToolCommand::Select(SelectCommand::ClearSelection))`
//! and the right tool plugin picks it up.
//!
//! ## Why one enum per tool
//!
//! - **Clear ownership**: each `*Command` enum lives next to the tool
//!   plugin that consumes it — no central handler that has to know
//!   every operation.
//! - **Composability**: a consumer that doesn't want a particular
//!   tool just doesn't add the plugin. The corresponding command
//!   variant becomes a no-op (or surfaces an `AppEvent::CommandError`
//!   from the dispatch layer).
//! - **Agent-friendly**: every tool's command enum can derive
//!   `JsonSchema` (or be hand-described) to give an MCP server a
//!   typed surface to expose.
//!
//! ## Sub-states
//!
//! When a tool has internal modes (Figma's Select tool: Translate /
//! Scale / Rotate), they're modelled as Bevy sub-states keyed off
//! the parent tool — e.g. `#[source(Tool = Tool::Select)]`.

pub mod connect;
pub mod create;
pub mod select;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use strum::{EnumIter, EnumProperty, IntoEnumIterator};

pub use connect::{ConnectCommand, ConnectEvent, ConnectKind, ConnectState, ConnectToolPlugin};
pub use create::{CreateCommand, CreateEvent, CreateToolPlugin};
pub use select::{SelectCommand, SelectEvent, SelectToolPlugin};

/// Top-level interaction mode. Exactly one is active at a time.
///
/// `strum` properties carry display metadata so the toolbar /
/// hotkey systems can introspect tools without a hand-maintained
/// table:
/// - `label` — the human-readable name shown on the toolbar button.
/// - `shortcut` — keyboard hotkey string (e.g. `"V"`, `"C"`).
///   `KeyCode::from_str` is used to map back to a Bevy keycode.
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

    /// Click empty canvas to spawn a new node.
    #[strum(props(label = "Create", shortcut = "C"))]
    Create,

    /// Click two nodes in succession to add an edge between them.
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

    /// Bevy `KeyCode` corresponding to this tool's shortcut, if any.
    pub fn shortcut_keycode(&self) -> Option<KeyCode> {
        let s = self.shortcut()?;
        keycode_from_letter(s)
    }

    pub fn all() -> impl Iterator<Item = Self> {
        Self::iter()
    }
}

/// Map a single uppercase letter (`"V"`, `"C"`, …) to the matching
/// `KeyCode`. Returns `None` for anything else; we don't yet support
/// modifier-key shortcuts in the toolbar metadata.
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
///
/// The [`crate::handlers::dispatch_app_commands`] system unwraps a
/// `ToolCommand` into the corresponding tool's internal stream
/// (`MessageWriter<SelectCommand>`, `MessageWriter<CreateCommand>`,
/// `MessageWriter<ConnectCommand>`).
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "tool", content = "command")]
pub enum ToolCommand {
    Select(SelectCommand),
    Create(CreateCommand),
    Connect(ConnectCommand),
}

/// Aggregator for every per-tool event. Lives on the umbrella
/// [`AppEvent::Tool`](crate::msg::AppEvent::Tool) variant.
///
/// `Switched` is the tool-transition projection: emitted by
/// [`crate::handlers::emit_tool_changed`] whenever `State<Tool>`
/// changes — UI click, hotkey, programmatic `SetTool`, all funnel
/// through the same observable event.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum ToolEvent {
    Switched { tool: Tool },
    Select(SelectEvent),
    Create(CreateEvent),
    Connect(ConnectEvent),
}

/// Plugin that registers the `Tool` state and adds the per-tool
/// plugins. Compose into your `App` after [`crate::AppPlugin`]; per-
/// tool plugins can also be added a la carte to skip a tool.
pub struct ToolsPlugin;

impl Plugin for ToolsPlugin {
    fn build(&self, app: &mut App) {
        // `init_state` requires the StateTransition schedule, which
        // ships with bevy's `StatesPlugin`. DefaultPlugins includes
        // it; MinimalPlugins (used in headless tests) doesn't —
        // ensure it's present either way.
        if !app.is_plugin_added::<bevy::state::app::StatesPlugin>() {
            app.add_plugins(bevy::state::app::StatesPlugin);
        }
        app.init_state::<Tool>();
        app.add_message::<ToolCommand>();
        app.add_message::<ToolEvent>();
        app.add_plugins((SelectToolPlugin, CreateToolPlugin, ConnectToolPlugin));
    }
}
