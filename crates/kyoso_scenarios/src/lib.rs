//! Scripted multi-client scenarios for the bench harness.
//!
//! Each scenario spins up a real in-process `kyoso_server` plus N
//! headless Bevy clients (using [`kyoso_circuit_client::AppPlugin`] —
//! the same plugin the real GUI client uses, minus rendering / input),
//! drives them through a deterministic script of joins, mutations,
//! disconnects, server snapshot / GC triggers, and reconnects, then
//! captures each peer's view of the CRDT-replicated world and diffs
//! peers pairwise.
//!
//! Output is a [`ScenarioReport`] JSON document written under
//! `target/harness-reports/scenario-<name>.json`, parseable by
//! `kyoso_loadgen`'s findings aggregator. Divergence between any two
//! peers (different node counts, different transforms, different
//! per-schema property values, etc.) surfaces as a critical finding.
//!
//! ## Why Bevy, not raw WS
//!
//! The suspected hydration / `SchemaDoc` / `EntityCrdtIndex` bugs all
//! live in the `kyoso_graph_sync` Bevy layer. Raw-WS clients (the
//! pattern `kyoso_loadgen` uses) drive the envelope protocol but skip
//! every system that sits between the wire and Bevy ECS — so they
//! can't surface those bugs at all. This crate carries the bevy
//! dependency so the other harness binaries don't have to.

pub mod capture;
pub mod harness;
pub mod report;
pub mod scenarios;
pub mod timeline;

pub use capture::{capture_peer_state, PeerState};
pub use harness::{build_app, pump_apps_until, ScenarioApp, ScenarioHarness};
pub use report::{
    Divergence, ScenarioOutcome, ScenarioReport, ScenarioStatus, write_report,
};
pub use scenarios::{run_scenario, scenario_names};
pub use timeline::{drain_timeline, write_timeline_jsonl, Timeline, TimelineEntry, TimelinePlugin};
