//! JSON report shape + pairwise divergence diff.
//!
//! The shape is intentionally flat enough for `kyoso_loadgen`'s
//! findings aggregator to parse without needing a full type clone.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::capture::PeerState;

/// Top-level outcome of one scenario run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScenarioReport {
    pub scenario: String,
    pub status: ScenarioStatus,
    /// One entry per checkpoint the scenario captured. Most scenarios
    /// capture once at the end; some capture before+after to localise
    /// when state changed.
    pub checkpoints: Vec<ScenarioOutcome>,
    /// Human-readable summary line for the findings aggregator.
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioStatus {
    /// Every peer pair agreed on every captured field.
    Converged,
    /// At least one pairwise difference at one checkpoint.
    Diverged,
    /// Scenario aborted (timeout, panic, connection failure).
    Aborted,
}

/// One checkpoint within a scenario: a label + the captured state of
/// each peer + the pairwise diff against the chosen reference peer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScenarioOutcome {
    pub label: String,
    pub peers: Vec<PeerState>,
    /// Diff against `peers[0]` (the reference). Empty means full
    /// convergence at this checkpoint.
    pub divergences: Vec<Divergence>,
}

/// One specific field where two peers disagreed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Divergence {
    pub peer_a: String,
    pub peer_b: String,
    pub kind: DivergenceKind,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DivergenceKind {
    /// Different node count.
    NodeCount,
    /// Different edge count.
    EdgeCount,
    /// One peer has a node the other doesn't (by `CrdtId`).
    MissingNode,
    /// One peer has an edge the other doesn't.
    MissingEdge,
    /// Both peers have a node with the same id but different fields
    /// (kind / layer / transform / property / label).
    NodeFieldMismatch,
    /// Same for edges (kind / endpoints).
    EdgeFieldMismatch,
    /// Different engine `applied_seq`.
    AppliedSeqMismatch,
}

/// Build a pairwise diff between every peer and the first peer in the
/// list. Returns one [`Divergence`] per disagreement, empty list on
/// full convergence.
pub fn diff_against_reference(peers: &[PeerState]) -> Vec<Divergence> {
    let mut out = Vec::new();
    let Some((reference, rest)) = peers.split_first() else {
        return out;
    };
    for peer in rest {
        out.extend(diff_pair(reference, peer));
    }
    out
}

fn diff_pair(a: &PeerState, b: &PeerState) -> Vec<Divergence> {
    let mut out = Vec::new();
    if a.applied_seq != b.applied_seq {
        out.push(Divergence {
            peer_a: a.label.clone(),
            peer_b: b.label.clone(),
            kind: DivergenceKind::AppliedSeqMismatch,
            detail: format!("a={}, b={}", a.applied_seq, b.applied_seq),
        });
    }
    if a.nodes.len() != b.nodes.len() {
        out.push(Divergence {
            peer_a: a.label.clone(),
            peer_b: b.label.clone(),
            kind: DivergenceKind::NodeCount,
            detail: format!("a={}, b={}", a.nodes.len(), b.nodes.len()),
        });
    }
    if a.edges.len() != b.edges.len() {
        out.push(Divergence {
            peer_a: a.label.clone(),
            peer_b: b.label.clone(),
            kind: DivergenceKind::EdgeCount,
            detail: format!("a={}, b={}", a.edges.len(), b.edges.len()),
        });
    }
    // Node-by-node.
    for (id, na) in &a.nodes {
        match b.nodes.get(id) {
            None => out.push(Divergence {
                peer_a: a.label.clone(),
                peer_b: b.label.clone(),
                kind: DivergenceKind::MissingNode,
                detail: format!("node {id} present in {} but missing in {}", a.label, b.label),
            }),
            Some(nb) => {
                if na != nb {
                    out.push(Divergence {
                        peer_a: a.label.clone(),
                        peer_b: b.label.clone(),
                        kind: DivergenceKind::NodeFieldMismatch,
                        detail: format!(
                            "node {id}: {} = {:?}, {} = {:?}",
                            a.label, na, b.label, nb
                        ),
                    });
                }
            }
        }
    }
    for id in b.nodes.keys() {
        if !a.nodes.contains_key(id) {
            out.push(Divergence {
                peer_a: a.label.clone(),
                peer_b: b.label.clone(),
                kind: DivergenceKind::MissingNode,
                detail: format!("node {id} present in {} but missing in {}", b.label, a.label),
            });
        }
    }
    // Edge-by-edge.
    for (id, ea) in &a.edges {
        match b.edges.get(id) {
            None => out.push(Divergence {
                peer_a: a.label.clone(),
                peer_b: b.label.clone(),
                kind: DivergenceKind::MissingEdge,
                detail: format!("edge {id} present in {} but missing in {}", a.label, b.label),
            }),
            Some(eb) => {
                if ea != eb {
                    out.push(Divergence {
                        peer_a: a.label.clone(),
                        peer_b: b.label.clone(),
                        kind: DivergenceKind::EdgeFieldMismatch,
                        detail: format!(
                            "edge {id}: {} = {:?}, {} = {:?}",
                            a.label, ea, b.label, eb
                        ),
                    });
                }
            }
        }
    }
    for id in b.edges.keys() {
        if !a.edges.contains_key(id) {
            out.push(Divergence {
                peer_a: a.label.clone(),
                peer_b: b.label.clone(),
                kind: DivergenceKind::MissingEdge,
                detail: format!("edge {id} present in {} but missing in {}", b.label, a.label),
            });
        }
    }
    out
}

/// Write the report to `target/harness-reports/scenario-<name>.json`.
/// Creates the directory if it doesn't exist. Returns the path.
pub fn write_report(report: &ScenarioReport) -> std::io::Result<std::path::PathBuf> {
    let dir = Path::new("target/harness-reports");
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("scenario-{}.json", report.scenario));
    let json = serde_json::to_string_pretty(report)
        .expect("ScenarioReport always serializes");
    fs::write(&path, json)?;
    Ok(path)
}
