//! Transaction recording for undo/redo and collaborative replication.
//!
//! This module provides:
//! - [`ChangeSource`] to track the origin of mutations (user, solver, network, etc.)
//! - [`CommandLog`] for undo/redo history
//! - [`PendingTransaction`] as a frame-level operation accumulator
//! - [`TransactionPlugin`] to wire recording into the graph pipeline

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Identifies the origin of a change, enabling feedback-loop prevention
/// and selective undo/redo.
///
/// Solvers should set [`CurrentChangeSource`] to [`ChangeSource::Solver`]
/// before making mutations. Change detection reads this resource and
/// stamps it into [`super::NodeChangeSet`] / [`super::EdgeChangeSet`].
/// Downstream systems can then filter changes to avoid re-triggering the
/// solver that produced them.
#[derive(Clone, Debug, Reflect, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[reflect(Serialize, Deserialize)]
pub enum ChangeSource {
    #[default]
    User,
    Solver,
    Propagation,
    Remote { peer_id: u64 },
}

/// Current change source for the frame.
///
/// Set this before making mutations so that change detection can attribute
/// them correctly. Resets to [`ChangeSource::User`] at the start of each
/// frame via [`reset_change_source`].
#[derive(Resource, Clone, Debug, Default)]
pub struct CurrentChangeSource(pub ChangeSource);

/// Unique identifier for a transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Reflect, Serialize, Deserialize)]
pub struct TransactionId(pub u64);

/// A recorded command paired with data needed to compute its inverse.
#[derive(Clone, Debug)]
pub struct CommandRecord {
    pub command: super::GraphCommand,
    pub inverse: Option<super::GraphCommand>,
}

/// A group of commands applied atomically in a single frame.
#[derive(Clone, Debug)]
pub struct Transaction {
    pub id: TransactionId,
    pub source: ChangeSource,
    pub records: Vec<CommandRecord>,
}

/// Resource maintaining undo/redo history.
#[derive(Resource)]
pub struct CommandLog {
    undo_stack: Vec<Transaction>,
    redo_stack: Vec<Transaction>,
    next_id: u64,
    max_history: usize,
}

impl Default for CommandLog {
    fn default() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            next_id: 1,
            max_history: 100,
        }
    }
}

impl CommandLog {
    pub fn with_max_history(max_history: usize) -> Self {
        Self {
            max_history,
            ..Default::default()
        }
    }

    pub fn next_transaction_id(&mut self) -> TransactionId {
        let id = TransactionId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Push a committed transaction. Clears the redo stack.
    pub fn push(&mut self, transaction: Transaction) {
        self.redo_stack.clear();
        self.undo_stack.push(transaction);
        while self.undo_stack.len() > self.max_history {
            self.undo_stack.remove(0);
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn pop_undo(&mut self) -> Option<Transaction> {
        self.undo_stack.pop()
    }

    pub fn pop_redo(&mut self) -> Option<Transaction> {
        self.redo_stack.pop()
    }

    pub fn push_redo(&mut self, transaction: Transaction) {
        self.redo_stack.push(transaction);
    }

    pub fn push_undo(&mut self, transaction: Transaction) {
        self.undo_stack.push(transaction);
    }

    pub fn undo_len(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo_stack.len()
    }

    pub fn undo_stack(&self) -> &[Transaction] {
        &self.undo_stack
    }

    pub fn redo_stack(&self) -> &[Transaction] {
        &self.redo_stack
    }

    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }
}

/// Frame-level accumulator for operations being built up before commit.
///
/// Systems that apply mutations via [`super::GraphCommand`] should record
/// them here. At the end of the frame's snapshot phase,
/// [`commit_pending_transaction`] moves them into the [`CommandLog`].
#[derive(Resource, Default)]
pub struct PendingTransaction {
    pub source: ChangeSource,
    pub records: Vec<CommandRecord>,
}

impl PendingTransaction {
    pub fn record(
        &mut self,
        command: super::GraphCommand,
        inverse: Option<super::GraphCommand>,
    ) {
        self.records.push(CommandRecord { command, inverse });
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn set_source(&mut self, source: ChangeSource) {
        self.source = source;
    }
}

/// Commits the pending transaction to the command log.
pub fn commit_pending_transaction(
    mut pending: ResMut<PendingTransaction>,
    mut log: ResMut<CommandLog>,
) {
    if pending.is_empty() {
        return;
    }

    let source = std::mem::take(&mut pending.source);
    let records = std::mem::take(&mut pending.records);
    let id = log.next_transaction_id();
    log.push(Transaction {
        id,
        source,
        records,
    });
}

/// Resets the change source to `User` at the start of each frame.
pub fn reset_change_source(mut source: ResMut<CurrentChangeSource>) {
    source.0 = ChangeSource::User;
}

/// Plugin that sets up transaction recording infrastructure.
///
/// Add alongside [`super::GraphManagerPlugin`] to enable command logging
/// and undo/redo support. Systems in
/// [`super::GraphSystemSet::SnapshotCreation`] will commit the pending
/// transaction each frame.
pub struct TransactionPlugin;

impl Plugin for TransactionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CommandLog>()
            .init_resource::<PendingTransaction>()
            .init_resource::<CurrentChangeSource>()
            .add_systems(
                Update,
                reset_change_source.in_set(super::GraphSystemSet::TransactionRecording),
            )
            .add_systems(
                Update,
                commit_pending_transaction.in_set(super::GraphSystemSet::SnapshotCreation),
            );
    }
}
