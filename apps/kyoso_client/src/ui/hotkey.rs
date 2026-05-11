//! Tool-switching hotkeys.
//!
//! Reads `Tool::shortcut_keycode()` for every variant on every frame
//! and, on `just_pressed`, writes `AppCommand::SetTool(...)` into the
//! duplex bus. Same routing as the toolbar — easier debugging because
//! every tool transition (UI click, hotkey, agent call) shows up in
//! one place.
//!
//! Shortcuts come from `strum` props on `Tool` (`Select` → `V`,
//! `Create` → `C`, `Connect` → `E`). Add a new variant + `shortcut`
//! prop and the hotkey "just works".

use bevy::prelude::*;

use crate::msg::AppCommand;
use crate::tool::Tool;

pub struct ToolHotkeyPlugin;

impl Plugin for ToolHotkeyPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, dispatch_hotkeys);
    }
}

fn dispatch_hotkeys(
    keys: Res<ButtonInput<KeyCode>>,
    mut commands_w: MessageWriter<AppCommand>,
) {
    for tool in Tool::all() {
        let Some(code) = tool.shortcut_keycode() else {
            continue;
        };
        if keys.just_pressed(code) {
            commands_w.write(AppCommand::SetTool(tool));
        }
    }
}
