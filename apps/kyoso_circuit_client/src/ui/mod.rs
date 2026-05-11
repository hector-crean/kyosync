//! UI scaffolding — toolbar and hotkeys.
//!
//! All UI surfaces emit [`AppCommand`](crate::msg::AppCommand) into the
//! duplex bus rather than mutating Bevy state directly, so a click and
//! a programmatic call from FFI / MCP / agent take the same code path.

pub mod hotkey;
pub mod toolbar;

pub use hotkey::ToolHotkeyPlugin;
pub use toolbar::ToolbarPlugin;

use bevy::prelude::*;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((ToolbarPlugin, ToolHotkeyPlugin));
    }
}
