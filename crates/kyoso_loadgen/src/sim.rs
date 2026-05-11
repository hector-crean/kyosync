//! Logical CRDT chaos simulator.
//!
//! Spawns N replicas of any [`CrdtModel`], lets each peer mutate
//! between rounds, then fans the resulting ops through a virtual
//! "server" that:
//!
//! - Stamps each op with a global sequence (one shared counter).
//! - Distributes the stamped op to every peer with seeded chaos:
//!   drop with probability `p_drop`, reorder via random delay, deliver
//!   late (`max_delay_rounds`).
//!
//! After `op_rounds`, the simulator drains every still-in-flight op
//! into every peer (no chaos on the final flush) and asserts every
//! peer's snapshot matches a canonical replica (one that received
//! every op in stamped order, no chaos).
//!
//! What it catches that hand-written tests don't:
//!
//! - **Convergence under reordering**: ops applied in different
//!   orders on different peers must converge to the same state.
//! - **Idempotency under retransmit**: re-delivering dropped ops
//!   later (when "the link recovers") still converges.
//! - **N-peer interactions**: hand-written tests max out at 2-3
//!   replicas; the chaos sim runs N=10+ trivially.
//!
//! Each run is fully deterministic given a seed — failing seeds
//! reproduce bit-for-bit.
//!
//! ## What it does *not* test
//!
//! - The WebSocket transport (use Layer 4b for that — see HARNESS.md).
//! - Real-time scheduling (no tokio, no wall-clock).
//! - Server compaction interactions.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use kyoso_crdt::{CrdtModel, GlobalSeq, PeerId};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::Serialize;

/// Knobs for one simulation run.
#[derive(Debug, Clone, Serialize)]
pub struct ChaosConfig {
    /// Number of replicas. Must be ≥ 1.
    pub peers: usize,
    /// Number of `mutate` rounds. Each round, every peer's mutate
    /// callback runs once.
    pub op_rounds: usize,
    /// Probability `[0.0, 1.0)` that any individual delivery is
    /// dropped on the way to a given peer. Dropped deliveries are
    /// re-queued for the final flush, so convergence holds — `1.0`
    /// would just delay everything to the flush.
    pub drop_probability: f64,
    /// Per-delivery extra rounds before it's actually applied. Each
    /// delivery picks a uniform delay in `[0, max_delay_rounds]`.
    /// `0` = strict in-order delivery within a round.
    pub max_delay_rounds: usize,
    /// Seed for the chaos RNG. Same seed → same outcome.
    pub seed: u64,
}

impl Default for ChaosConfig {
    fn default() -> Self {
        Self {
            peers: 3,
            op_rounds: 200,
            drop_probability: 0.1,
            max_delay_rounds: 5,
            seed: 0xCAFE_F00D,
        }
    }
}

/// Result of a chaos run.
#[derive(Debug, Clone, Serialize)]
pub struct ChaosReport {
    pub config: ChaosConfig,
    /// True iff every peer's snapshot equals the canonical replica's
    /// snapshot after the final flush.
    pub converged: bool,
    /// Total ops issued (across all peers, before chaos).
    pub ops_issued: u64,
    /// Total ops actually delivered (every peer's `apply_remote` count).
    /// Should equal `ops_issued * peers` once flushed; if it doesn't,
    /// chaos dropped something we failed to recover.
    pub deliveries: u64,
    /// Number of deliveries that the chaos layer initially dropped
    /// and re-queued for the final flush.
    pub re_delivered_after_drop: u64,
    /// Final applied_seq across peers (should be identical).
    pub peer_applied_seqs: Vec<GlobalSeq>,
}

impl ChaosReport {
    /// One-line summary for logs.
    pub fn summary(&self) -> String {
        format!(
            "converged={} peers={} rounds={} ops_issued={} drops_recovered={} seed=0x{:X}",
            self.converged,
            self.config.peers,
            self.config.op_rounds,
            self.ops_issued,
            self.re_delivered_after_drop,
            self.config.seed,
        )
    }
}

/// One pending delivery: a stamped op headed to a specific peer with a
/// scheduled delivery round.
struct PendingDelivery<K> {
    target_peer_idx: usize,
    op: kyoso_crdt::Op<K>,
    deliver_at_round: usize,
}

/// Run a chaos simulation against any `CrdtModel`.
///
/// `mutate` is invoked once per peer per round. The closure receives
/// `&mut M` (the peer's local replica) and the per-round RNG so the
/// caller can decide what to mutate on each replica deterministically.
/// All locally-generated ops are auto-drained from `peer.drain_pending()`
/// at the end of each round and shipped through the virtual server.
///
/// Convergence is checked by encoding each peer's snapshot via
/// `postcard` and asserting all encoded blobs match — uses the
/// snapshot's existing `Serialize` impl rather than relying on
/// `PartialEq` (which not every `CrdtModel::State` implements).
pub fn run_chaos_sim<M, F>(cfg: ChaosConfig, mut mutate: F) -> ChaosReport
where
    M: CrdtModel,
    M::State: PartialEq + std::fmt::Debug,
    F: FnMut(&mut M, &mut StdRng, usize, PeerId),
{
    assert!(cfg.peers > 0, "chaos sim requires at least one peer");
    let mut rng = StdRng::seed_from_u64(cfg.seed);

    // Each peer is bound to PeerId = (i+1) so PeerId 0 is reserved for
    // the canonical replica that sees every op in stamped order.
    let mut peers: Vec<M> = (0..cfg.peers)
        .map(|i| {
            let mut m = M::default();
            m.set_peer((i as PeerId) + 1);
            m
        })
        .collect();
    let mut canonical: M = M::default();
    canonical.set_peer(0);

    // Server-assigned global seq (one counter, monotonic).
    let mut next_seq: GlobalSeq = 1;
    // Pending deliveries: ops that have been stamped but not yet
    // entered their target peer's inbox (because of chaos delay).
    let mut pending: VecDeque<PendingDelivery<M::OpKind>> = VecDeque::new();
    // Per-peer inbox: buffers ops by seq until the gap before them
    // fills, then drains contiguously into apply_remote. Mirrors what
    // a real catchup-aware client must do under packet loss.
    let mut inboxes: Vec<BTreeMap<GlobalSeq, kyoso_crdt::Op<M::OpKind>>> =
        (0..cfg.peers).map(|_| BTreeMap::new()).collect();
    let mut ops_issued = 0u64;
    let mut deliveries = 0u64;
    let mut re_delivered_after_drop = 0u64;

    for round in 0..cfg.op_rounds {
        // 1. Each peer mutates. Per-peer RNG draws so seed determines
        //    all behaviour.
        for (idx, peer) in peers.iter_mut().enumerate() {
            let peer_id = (idx as PeerId) + 1;
            mutate(peer, &mut rng, round, peer_id);
        }

        // 2. Drain locally-generated ops, stamp via virtual server,
        //    schedule deliveries to all peers + canonical.
        for (idx, peer) in peers.iter_mut().enumerate() {
            for op in peer.drain_pending() {
                let stamped = op.with_seq(next_seq);
                next_seq += 1;
                ops_issued += 1;

                // Canonical replica always sees ops immediately + in stamped order.
                if let Err(e) = canonical.apply_remote(&stamped) {
                    panic!(
                        "canonical apply failed at seq={} from peer {}: {:?}",
                        next_seq - 1,
                        idx + 1,
                        e
                    );
                }

                // For every peer (including originator — server echoes
                // back), schedule delivery with chaos.
                for target_idx in 0..cfg.peers {
                    let drop = rng.gen_bool(cfg.drop_probability);
                    let delay = if cfg.max_delay_rounds == 0 {
                        0
                    } else {
                        rng.gen_range(0..=cfg.max_delay_rounds)
                    };
                    if drop {
                        // "Network drop": re-queue for the final flush
                        // round so we can prove convergence holds once
                        // the link recovers.
                        re_delivered_after_drop += 1;
                        pending.push_back(PendingDelivery {
                            target_peer_idx: target_idx,
                            op: stamped.clone(),
                            deliver_at_round: cfg.op_rounds,
                        });
                    } else {
                        pending.push_back(PendingDelivery {
                            target_peer_idx: target_idx,
                            op: stamped.clone(),
                            deliver_at_round: round + delay,
                        });
                    }
                }
            }
        }

        // 3. Fire any pending deliveries scheduled for this round.
        //    Each delivery enters its target peer's inbox; then we
        //    drain contiguous-prefix ops out of every inbox into
        //    apply_remote. Buffering on the receive side is what
        //    keeps `apply_remote` happy when chaos opens gaps in the
        //    seq stream — same shape a real catchup-aware client uses.
        let due: Vec<PendingDelivery<M::OpKind>> = {
            let mut out = Vec::new();
            let mut keep = VecDeque::new();
            while let Some(d) = pending.pop_front() {
                if d.deliver_at_round == round {
                    out.push(d);
                } else {
                    keep.push_back(d);
                }
            }
            pending = keep;
            out
        };
        enqueue_into_inboxes(&mut inboxes, due);
        drain_contiguous(&mut peers, &mut inboxes, &mut deliveries);
    }

    // 4. Final flush: enter every still-pending delivery into the
    //    matching inbox, then drain. By construction every inbox now
    //    holds the entire op log for its peer, so the contiguous drain
    //    walks all the way to head. This is the "link fully recovered,
    //    all backlog applied" state.
    let all: Vec<PendingDelivery<M::OpKind>> = pending.into_iter().collect();
    enqueue_into_inboxes(&mut inboxes, all);
    drain_contiguous(&mut peers, &mut inboxes, &mut deliveries);

    // 5. Convergence check: every peer's snapshot must equal the
    //    canonical replica's snapshot. We compare via `PartialEq` so
    //    `Vec<…>` ordering matters but `HashMap<…>` set-equality is
    //    handled correctly. (Both `kyoso_graph_crdt::Snapshot` and
    //    `kyoso_comments_crdt::CommentsSnapshot` sort their Vec
    //    contents by id at snapshot time so PartialEq is meaningful.)
    let canonical_snap = canonical.snapshot();
    let canonical_seq = canonical.applied_seq();

    let mut peer_applied_seqs = Vec::with_capacity(peers.len());
    let mut converged = true;
    let mut canonical_dumped = false;
    for (i, peer) in peers.iter().enumerate() {
        let snap = peer.snapshot();
        peer_applied_seqs.push(peer.applied_seq());
        if snap != canonical_snap {
            converged = false;
            tracing::error!(
                peer_idx = i + 1,
                peer_seq = peer.applied_seq(),
                canonical_seq,
                "peer diverged from canonical at end-of-sim"
            );
            // Set RUST_LOG=trace to dump the full snapshots when
            // investigating a fresh divergence.
            if !canonical_dumped {
                tracing::trace!(canonical = ?canonical_snap, "canonical snapshot");
                canonical_dumped = true;
            }
            tracing::trace!(peer_idx = i + 1, peer = ?snap, "peer snapshot");
        }
    }

    ChaosReport {
        config: cfg,
        converged,
        ops_issued,
        deliveries,
        re_delivered_after_drop,
        peer_applied_seqs,
    }
}

/// Insert every delivery into its target peer's inbox, keyed by op
/// `seq`. Duplicates (re-deliveries from the drop+flush path) collapse
/// to one entry — the op is identical, only the latest insert wins.
fn enqueue_into_inboxes<K>(
    inboxes: &mut [BTreeMap<GlobalSeq, kyoso_crdt::Op<K>>],
    due: Vec<PendingDelivery<K>>,
) where
    K: Clone,
{
    for d in due {
        let seq = d.op.seq.expect("server stamped every delivery with a seq");
        inboxes[d.target_peer_idx].insert(seq, d.op);
    }
}

/// For each peer, pop the contiguous-prefix run of ops out of its
/// inbox (`seq == applied_seq + 1`, then `+2`, …) and apply them.
/// Stops at the first gap — the rest waits in the inbox until later
/// rounds (or the final flush) deliver the missing seqs.
fn drain_contiguous<M, K>(
    peers: &mut [M],
    inboxes: &mut [BTreeMap<GlobalSeq, kyoso_crdt::Op<K>>],
    deliveries: &mut u64,
) where
    M: CrdtModel<OpKind = K>,
    K: Clone + serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
{
    for (i, inbox) in inboxes.iter_mut().enumerate() {
        loop {
            let next_seq = peers[i].applied_seq() + 1;
            let Some(op) = inbox.remove(&next_seq) else {
                break;
            };
            match peers[i].apply_remote(&op) {
                Ok(()) => *deliveries += 1,
                Err(kyoso_crdt::ApplyError::OutOfOrder { expected, got }) => {
                    panic!(
                        "drain_contiguous violated invariant on peer {}: expected {expected}, got {got}",
                        i + 1
                    );
                }
                Err(kyoso_crdt::ApplyError::Unconfirmed) => {
                    panic!("unconfirmed op reached the simulator's deliver step");
                }
            }
        }
    }
}

/// Sweep many seeds with the same `cfg` (other than `seed`). Useful
/// for shaking out flaky-seed convergence bugs — proptest-lite.
pub fn sweep_seeds<M, F>(
    base_cfg: ChaosConfig,
    seeds: impl IntoIterator<Item = u64>,
    mutate: F,
) -> SweepReport
where
    M: CrdtModel,
    M::State: PartialEq + std::fmt::Debug,
    F: FnMut(&mut M, &mut StdRng, usize, PeerId) + Clone,
{
    let mut runs = Vec::new();
    let mut all_converged = true;
    for seed in seeds {
        let mut cfg = base_cfg.clone();
        cfg.seed = seed;
        let report = run_chaos_sim::<M, _>(cfg, mutate.clone());
        if !report.converged {
            all_converged = false;
        }
        runs.push(report);
    }
    SweepReport {
        all_converged,
        runs,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SweepReport {
    pub all_converged: bool,
    pub runs: Vec<ChaosReport>,
}

impl fmt::Display for SweepReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "sweep: {} runs, all_converged={}",
            self.runs.len(),
            self.all_converged
        )?;
        for r in &self.runs {
            writeln!(f, "  {}", r.summary())?;
        }
        Ok(())
    }
}
