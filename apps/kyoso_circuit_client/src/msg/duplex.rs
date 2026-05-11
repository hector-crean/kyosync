//! `DuplexPlugin` — bridge a pair of `crossbeam_channel`s in/out of Bevy.
//!
//! Adapted from the kyoso_client equivalent. Lets any external source
//! (FFI, MCP, agent, the kyoso_server) push `AppCommand`s into Bevy and
//! receive `AppEvent`s back out without knowing about Bevy internals.
//! Multi-producer, multi-consumer.

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
pub fn create_duplex_plugin<
    In: Event + Message + Debug + Clone + Send + Sync + 'static,
    Out: Event + Message + Debug + Clone + Send + Sync + 'static,
>() -> (DuplexPlugin<In, Out>, Receiver<Out>, Sender<In>) {
    let (ext_tx, rust_rx) = crossbeam_channel::unbounded::<In>();
    let (rust_tx, ext_rx) = crossbeam_channel::unbounded::<Out>();
    let plugin = DuplexPlugin::new(rust_tx, rust_rx);
    (plugin, ext_rx, ext_tx)
}
