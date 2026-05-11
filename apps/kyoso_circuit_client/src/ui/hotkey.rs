//! Tool-switching hotkeys. Reads `Tool::shortcut_keycode()` and writes
//! `AppCommand::SetTool(...)` into the duplex bus on `just_pressed`.

use bevy::prelude::*;

use crate::msg::AppCommand;
use crate::tool::Tool;

pub struct ToolHotkeyPlugin;

impl Plugin for ToolHotkeyPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, dispatch_hotkeys);
    }
}

fn dispatch_hotkeys(keys: Res<ButtonInput<KeyCode>>, mut commands_w: MessageWriter<AppCommand>) {
    for tool in Tool::all() {
        let Some(code) = tool.shortcut_keycode() else {
            continue;
        };
        if keys.just_pressed(code) {
            commands_w.write(AppCommand::SetTool(tool));
        }
    }
}
