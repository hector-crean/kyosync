//! `DuplexPlugin` — bridge a pair of `crossbeam_channel`s in/out of Bevy.
//!
//! Adapted from `bild_canvas`'s pattern. The plugin lets **any external
//! source** (JavaScript via wasm-bindgen, an MCP server, a CRDT
//! coordinator, a CLI hub, …) push messages into Bevy as `Message`s and
//! receive Bevy-emitted `Message`s back out — without that source
//! needing to know anything about Bevy internals.
//!
//! ## Direction
//!
//! - **`In`** flows: external producer → Bevy. The `In` sender is given
//!   to the producer; the plugin's `PreUpdate` system drains it and
//!   writes Bevy `Message`s.
//! - **`Out`** flows: Bevy → external consumer. Bevy systems write
//!   `Out` messages; the plugin's `PostUpdate` system forwards them on
//!   the channel; the consumer reads from the receiver.
//!
//! Multiple producers (JS *and* MCP *and* the kyoso_server) can clone
//! the same `In` sender and feed the same Bevy stream, because
//! crossbeam channels are MPMC.
//!
//! ## Usage pattern
//!
//! ```ignore
//! let (duplex, ext_rx, ext_tx) = create_duplex_plugin::<AppCommand, AppEvent>();
//! // Hand `ext_tx` to JS / MCP / WS task; they push AppCommands.
//! // Hand `ext_rx` to whoever wants to observe AppEvents (logs, MCP responses).
//! App::new().add_plugins(duplex).add_plugins(MyAppPlugin).run();
//! ```

use core::fmt::Debug;

use bevy::app::{App, PostUpdate, PreUpdate};
use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, TryRecvError};

#[derive(Resource, Clone)]
pub struct ChannelMsgSender<T: Event + Message + Clone>(pub Sender<T>);

#[derive(Resource, Clone)]
struct ChannelMsgReceiver<T: Event + Message + Clone>(pub Receiver<T>);

pub struct DuplexPlugin<
    In: Event + Message + Debug + Clone + Send + Sync + 'static,
    Out: Event + Message + Debug + Clone + Send + Sync + 'static,
> {
    rust_tx: ChannelMsgSender<Out>,
    rust_rx: ChannelMsgReceiver<In>,
}

impl<In, Out> DuplexPlugin<In, Out>
where
    In: Event + Message + Debug + Clone + Send + Sync + 'static,
    Out: Event + Message + Debug + Clone + Send + Sync + 'static,
{
    pub fn new(rust_tx: Sender<Out>, rust_rx: Receiver<In>) -> Self {
        Self {
            rust_tx: ChannelMsgSender(rust_tx),
            rust_rx: ChannelMsgReceiver(rust_rx),
        }
    }

    fn receive_external_event(
        receiver: Res<ChannelMsgReceiver<In>>,
        mut event_wtr: MessageWriter<In>,
    ) {
        loop {
            match receiver.0.try_recv() {
                Ok(msg) => {
                    event_wtr.write(msg);
                }
                Err(TryRecvError::Disconnected) => {
                    error!("DuplexPlugin: external sender dropped");
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
    }

    fn send_events_externally(
        mut events: MessageReader<Out>,
        sender: Res<ChannelMsgSender<Out>>,
    ) {
        for event in events.read() {
            if let Err(err) = sender.0.send(event.clone()) {
                error!("DuplexPlugin: external receiver dropped: {err:?}");
            }
        }
    }
}

impl<In, Out> Plugin for DuplexPlugin<In, Out>
where
    In: Event + Message + Debug + Clone + Send + Sync + 'static,
    Out: Event + Message + Debug + Clone + Send + Sync + 'static,
{
    fn build(&self, app: &mut App) {
        app.insert_resource(self.rust_tx.clone())
            .insert_resource(self.rust_rx.clone())
            .add_message::<In>()
            .add_message::<Out>()
            .add_systems(PreUpdate, Self::receive_external_event)
            .add_systems(PostUpdate, Self::send_events_externally);
    }
}

/// Construct a [`DuplexPlugin`] plus the external-side `(receiver,
/// sender)` pair.
///
/// - The plugin goes into `App::add_plugins(...)`.
/// - The returned `Receiver<Out>` is what external observers read.
/// - The returned `Sender<In>` is what external producers push to.
///
/// Both channels are MPMC — clone the sender for every producer that
/// wants to push, clone the receiver for every consumer that wants to
/// observe.
pub fn create_duplex_plugin<
    In: Event + Message + Debug + Clone + Send + Sync + 'static,
    Out: Event + Message + Debug + Clone + Send + Sync + 'static,
>() -> (DuplexPlugin<In, Out>, Receiver<Out>, Sender<In>) {
    let (ext_tx, rust_rx) = crossbeam_channel::unbounded::<In>();
    let (rust_tx, ext_rx) = crossbeam_channel::unbounded::<Out>();
    let plugin = DuplexPlugin::new(rust_tx, rust_rx);
    (plugin, ext_rx, ext_tx)
}
