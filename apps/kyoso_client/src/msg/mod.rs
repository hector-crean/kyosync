//! External-API messages and the duplex bridge that carries them.

pub mod command;
pub mod duplex;
pub mod event;
pub mod global_channel;
pub mod graph;
pub mod sync;

pub use command::{AppCommand, ExternalId, Pos2, Rgb};
pub use duplex::{create_duplex_plugin, ChannelMsgSender, DuplexPlugin};
pub use event::AppEvent;
pub use global_channel::{GlobalEventChannel, GLOBAL};
pub use graph::{GraphCommandExt, GraphMessageExt};
pub use sync::{SyncCommand, SyncEvent};
