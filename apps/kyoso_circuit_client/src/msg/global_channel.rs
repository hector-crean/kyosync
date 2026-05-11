//! `GlobalEventChannel` — process-wide programmatic handle to the
//! Duplex bridge.
//!
//! The Duplex plugin owns the `Sender<In>` / `Receiver<Out>` pair
//! internally; some external producers (FFI surfaces, MCP servers,
//! agent tool implementations) have nowhere to thread a runtime-built
//! handle through. The global channel solves that by giving any code
//! in the process `set_sender(...)` / `set_receiver(...)` once at
//! startup, then `send(...)` / `try_receive(...)` from anywhere.

use std::sync::Mutex;

use bevy::prelude::Event;
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

pub struct GlobalEventChannel<In, Out>
where
    In: Clone + Serialize + for<'a> Deserialize<'a> + Event,
    Out: Clone + Serialize + for<'a> Deserialize<'a> + Event,
{
    sender: Mutex<Option<Sender<In>>>,
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

    pub fn send(&self, msg: In) -> Result<(), String> {
        let guard = self.sender.lock().expect("sender mutex poisoned");
        match &*guard {
            Some(tx) => tx.send(msg).map_err(|e| format!("send: {e}")),
            None => Err("sender not initialised".into()),
        }
    }

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

/// Process-wide handle for the kyoso circuit-client command bus. Wired
/// up in [`crate::run`] and any binary that uses the duplex bridge.
pub static GLOBAL: once_cell::sync::Lazy<
    GlobalEventChannel<crate::msg::AppCommand, crate::msg::AppEvent>,
> = once_cell::sync::Lazy::new(GlobalEventChannel::new);
