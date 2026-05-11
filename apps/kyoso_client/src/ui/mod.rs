//! UI scaffolding — toolbar, hotkeys, and (future) palette / hints.
//!
//! All UI surfaces emit [`AppCommand`](crate::msg::AppCommand) into
//! the duplex bus rather than mutating Bevy state directly, so a
//! click and a programmatic call from JS / MCP / agent take the same
//! code path.

pub mod hotkey;
pub mod toolbar;

pub use hotkey::ToolHotkeyPlugin;
pub use toolbar::ToolbarPlugin;

use bevy::prelude::*;

/// Bundle plugin that adds the standard UI surfaces. Add this from
/// the windowed binary (or skip individual sub-plugins if you only
/// want some of them).
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((ToolbarPlugin, ToolHotkeyPlugin));
    }
}
