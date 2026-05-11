//! `GlobalEventChannel` — process-wide programmatic handle to the
//! Duplex bridge.
//!
//! Adapted from `bild_canvas`'s pattern. The Duplex plugin owns the
//! `Sender<In>` / `Receiver<Out>` pair *internally*; some external
//! producers (notably wasm-bindgen FFI handlers and CRDT inbound
//! callbacks) have nowhere to thread a runtime-built handle through.
//! The global channel solves that by giving any code in the process
//! `set_sender(...)` / `set_receiver(...)` once at startup, then
//! `send(...)` / `try_receive(...)` from anywhere.
//!
//! ## Pattern
//!
//! ```ignore
//! pub static GLOBAL: Lazy<GlobalEventChannel<AppCommand, AppEvent>> =
//!     Lazy::new(GlobalEventChannel::new);
//!
//! // bin/main:
//! let (duplex, ext_rx, ext_tx) = create_duplex_plugin::<AppCommand, AppEvent>();
//! GLOBAL.set_sender(ext_tx.clone());
//! GLOBAL.set_receiver(ext_rx);
//!
//! // wasm-bindgen / MCP / agent code:
//! GLOBAL.send(AppCommand::SpawnNode { ... }).unwrap();
//! ```
//!
//! Use the per-app channel handles when you can pass them around;
//! reach for the global only when you can't (FFI surfaces, observer
//! callbacks, agent tool implementations).

use std::sync::Mutex;

use bevy::prelude::Event;
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

/// Process-wide handle for one duplex bridge. Generic over the In/Out
/// message types so consumers parameterise it on their own
/// AppCommand / AppEvent enums.
pub struct GlobalEventChannel<In, Out>
where
    In: Clone + Serialize + for<'a> Deserialize<'a> + Event,
    Out: Clone + Serialize + for<'a> Deserialize<'a> + Event,
{
    /// Sender for inbound messages (external code → Bevy).
    sender: Mutex<Option<Sender<In>>>,
    /// Receiver for outbound messages (Bevy → external code).
    receiver: Mutex<Option<Receiver<Out>>>,
}

impl<In, Out> Default for GlobalEventChannel<In, Out>
where
    In: Clone + Serialize + for<'a> Deserialize<'a> + Event,
    Out: Clone + Serialize + for<'a> Deserialize<'a> + Event,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<In, Out> GlobalEventChannel<In, Out>
where
    In: Clone + Serialize + for<'a> Deserialize<'a> + Event,
    Out: Clone + Serialize + for<'a> Deserialize<'a> + Event,
{
    pub const fn new() -> Self {
        Self {
            sender: Mutex::new(None),
            receiver: Mutex::new(None),
        }
    }

    pub fn set_sender(&self, sender: Sender<In>) {
        *self.sender.lock().expect("sender mutex poisoned") = Some(sender);
    }

    pub fn set_receiver(&self, receiver: Receiver<Out>) {
        *self.receiver.lock().expect("receiver mutex poisoned") = Some(receiver);
    }

    /// Push a command into the Bevy world. Returns `Err` if the
    /// channel hasn't been wired yet (caller should call `set_sender`
    /// first) or the receiving side has dropped (the Bevy app exited).
    pub fn send(&self, msg: In) -> Result<(), String> {
        let guard = self.sender.lock().expect("sender mutex poisoned");
        match &*guard {
            Some(tx) => tx.send(msg).map_err(|e| format!("send: {e}")),
            None => Err("sender not initialised".into()),
        }
    }

    /// Non-blocking poll for an outbound event. Returns `Ok(None)` if
    /// the channel is empty, `Ok(Some)` with one event, or `Err` if
    /// the receiver hasn't been wired or has disconnected.
    pub fn try_receive(&self) -> Result<Option<Out>, String> {
        let guard = self.receiver.lock().expect("receiver mutex poisoned");
        match &*guard {
            Some(rx) => match rx.try_recv() {
                Ok(msg) => Ok(Some(msg)),
                Err(crossbeam_channel::TryRecvError::Empty) => Ok(None),
                Err(e) => Err(format!("recv: {e}")),
            },
            None => Err("receiver not initialised".into()),
        }
    }
}

/// Process-wide handle for the kyoso client's command bus. Wired up
/// in `kyoso_client::run` and any binary that uses the duplex bridge.
/// External producers (JS via wasm-bindgen, MCP server tools, agent
/// frameworks) reach for this when they can't pass a `Sender` around
/// explicitly.
pub static GLOBAL: once_cell::sync::Lazy<
    GlobalEventChannel<crate::msg::AppCommand, crate::msg::AppEvent>,
> = once_cell::sync::Lazy::new(GlobalEventChannel::new);
