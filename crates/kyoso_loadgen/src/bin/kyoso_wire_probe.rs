//! `kyoso_wire_probe` — measure postcard-encoded byte size of every
//! storage-channel op kind plus the presence-channel payloads we'd
//! reasonably broadcast at high frequency (cursor, viewport, selection).
//!
//! Output is per-variant: raw payload bytes (what the server logs +
//! retains) and envelope-wrapped bytes (what travels on the wire as
//! `EnvelopeClientMsg::Submit { model, payload }` for storage ops, or
//! `EnvelopeClientMsg::Presence(state)` for presence). The envelope
//! overhead is the cost of multiplexing through the multi-model router.
//!
//! With these numbers, the broadcast cost at N peers is
//! `N × ops_per_sec × wire_bytes × (N - 1)` for storage (everyone hears
//! every op); presence is the same shape but the server never logs or
//! retains, so its only cost is fan-out.
//!
//! ```text
//! kyoso_wire_probe --output target/harness-reports/wire-sizes.json
//! ```

use std::path::PathBuf;

use clap::Parser;
use kyoso_comments_crdt::{CommentOpKind, comments_model};
use kyoso_crdt::{
    CrdtId, EnvelopeClientMsg, EnvelopeServerMsg, ModelId, Op, Path, PeerId, SubDot, WireDelta,
};
use kyoso_graph_crdt::{EdgeCategory, OpKind, graph_model};
use serde::Serialize;

#[derive(Parser, Debug)]
#[command(version, about = "Wire-size probe for kyoso CRDT op kinds")]
struct Args {
    /// Path to write the JSON report to. Defaults to stdout if omitted.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct WireReport {
    storage_ops: Vec<OpSize>,
    presence_payloads: Vec<PresenceSize>,
    /// Recommended cutover threshold: any op kind whose `submit_bytes`
    /// would dominate at scale (> presence selection at 5 ids) is a
    /// candidate for the storage channel only if its rate is low.
    /// High-rate small-payload kinds belong on presence.
    notes: WireNotes,
}

#[derive(Debug, Serialize)]
struct OpSize {
    model: &'static str,
    variant: String,
    /// Raw `Op<K>` payload bytes (what the server logs).
    payload_bytes: usize,
    /// Bytes on the wire when wrapped in `EnvelopeClientMsg::Submit`.
    submit_bytes: usize,
    /// Bytes the server broadcasts to each subscribed peer (encoded
    /// `EnvelopeServerMsg::Apply`).
    apply_bytes: usize,
    /// `submit_bytes - payload_bytes`: cost of the multi-model router.
    envelope_overhead: usize,
}

#[derive(Debug, Serialize)]
struct PresenceSize {
    label: String,
    /// Raw bytes of the presence struct (the opaque `state: Vec<u8>`
    /// payload — consumers postcard-encode their own struct).
    payload_bytes: usize,
    /// Bytes wrapped in `EnvelopeClientMsg::Presence(state)`.
    submit_bytes: usize,
    /// Bytes broadcast to each peer in `EnvelopeServerMsg::PresenceUpdate`.
    apply_bytes: usize,
}

#[derive(Debug, Serialize)]
struct WireNotes {
    /// Cost in bytes of broadcasting one op of each kind to `N - 1`
    /// peers from `N` total at a steady 10 ops/sec — gives a feel for
    /// the quadratic.
    fanout_per_sec_at_n: Vec<FanoutRow>,
}

#[derive(Debug, Serialize)]
struct FanoutRow {
    n_peers: usize,
    ops_per_sec_per_peer: u32,
    /// Total server egress per second for the smallest storage op
    /// (`AddNode`) at this N.
    smallest_storage_bytes_per_sec: u64,
    /// Total server egress per second for the largest storage op
    /// observed.
    largest_storage_bytes_per_sec: u64,
    /// Same calculation for a 32-byte presence payload (cursor +
    /// modifier flags).
    cursor_presence_bytes_per_sec: u64,
}

fn measure_storage<K: serde::Serialize>(
    model: &'static str,
    variant: &str,
    op: &Op<K>,
    model_id: &ModelId,
) -> OpSize {
    let payload = postcard::to_allocvec(op).expect("encode op");
    let submit = EnvelopeClientMsg::Submit {
        model: model_id.clone(),
        payload: payload.clone(),
    };
    let submit_bytes = submit.encode().expect("encode envelope");
    let apply = EnvelopeServerMsg::Apply {
        model: model_id.clone(),
        payload: payload.clone(),
    };
    let apply_bytes = apply.encode().expect("encode envelope");
    OpSize {
        model,
        variant: variant.to_string(),
        payload_bytes: payload.len(),
        submit_bytes: submit_bytes.len(),
        apply_bytes: apply_bytes.len(),
        envelope_overhead: submit_bytes.len() - payload.len(),
    }
}

fn measure_presence(label: &str, payload: Vec<u8>) -> PresenceSize {
    let submit = EnvelopeClientMsg::Presence(payload.clone());
    let submit_bytes = submit.encode().expect("encode presence");
    let apply = EnvelopeServerMsg::PresenceUpdate {
        peer: 1u32 as PeerId,
        state: payload.clone(),
    };
    let apply_bytes = apply.encode().expect("encode presence apply");
    PresenceSize {
        label: label.to_string(),
        payload_bytes: payload.len(),
        submit_bytes: submit_bytes.len(),
        apply_bytes: apply_bytes.len(),
    }
}

fn graph_ops() -> Vec<OpSize> {
    let model = graph_model();
    let peer: PeerId = 1;
    let id = |seq| CrdtId::new(peer, seq);
    let stamped = |kind: OpKind| {
        let mut op = Op::new(id(1), kind);
        op.seq = Some(42);
        op
    };
    vec![
        measure_storage("graph", "AddNode", &stamped(OpKind::AddNode), &model),
        measure_storage(
            "graph",
            "RemoveNode",
            &stamped(OpKind::RemoveNode { target: id(2) }),
            &model,
        ),
        measure_storage(
            "graph",
            "Move (root, 8-char position)",
            &stamped(OpKind::Move {
                target: id(2),
                new_parent: None,
                position: "p4abc123".into(),
            }),
            &model,
        ),
        measure_storage(
            "graph",
            "Move (parented, 8-char position)",
            &stamped(OpKind::Move {
                target: id(2),
                new_parent: Some(id(3)),
                position: "p4abc123".into(),
            }),
            &model,
        ),
        measure_storage(
            "graph",
            "AddRefEdge (Reference)",
            &stamped(OpKind::AddRefEdge {
                category: EdgeCategory::Reference,
                from: id(2),
                to: id(3),
            }),
            &model,
        ),
        measure_storage(
            "graph",
            "RemoveRefEdge",
            &stamped(OpKind::RemoveRefEdge { target: id(2) }),
            &model,
        ),
        measure_storage(
            "graph",
            "SetNodeProperty (LwwReplace<f32>)",
            &stamped(OpKind::SetNodeProperty {
                target: id(2),
                path: Path::field("opacity"),
                delta: WireDelta::LwwReplace {
                    value: postcard::to_allocvec(&0.42_f32).unwrap(),
                },
            }),
            &model,
        ),
        measure_storage(
            "graph",
            "SetNodeProperty (LwwReplace<32B str>)",
            &stamped(OpKind::SetNodeProperty {
                target: id(2),
                path: Path::field("name"),
                delta: WireDelta::LwwReplace {
                    value: postcard::to_allocvec(&"a-32-byte-display-name-string!!!").unwrap(),
                },
            }),
            &model,
        ),
        measure_storage(
            "graph",
            "SetNodeProperty (OrSetAdd tag)",
            &stamped(OpKind::SetNodeProperty {
                target: id(2),
                path: Path::field("tags"),
                delta: WireDelta::OrSetAdd {
                    value: postcard::to_allocvec(&"design").unwrap(),
                },
            }),
            &model,
        ),
        measure_storage(
            "graph",
            "SetNodeProperty (PnCounterDelta +1)",
            &stamped(OpKind::SetNodeProperty {
                target: id(2),
                path: Path::field("votes"),
                delta: WireDelta::PnCounterDelta { by: 1 },
            }),
            &model,
        ),
        measure_storage(
            "graph",
            "SetNodeProperty (SequenceInsert 8B)",
            &stamped(OpKind::SetNodeProperty {
                target: id(2),
                path: Path::field("text"),
                delta: WireDelta::SequenceInsert {
                    predecessor: Some(SubDot::new(id(5), 0)),
                    value: postcard::to_allocvec(&"abcdefgh").unwrap(),
                },
            }),
            &model,
        ),
    ]
}

fn comment_ops() -> Vec<OpSize> {
    let model = comments_model();
    let peer: PeerId = 1;
    let id = |seq| CrdtId::new(peer, seq);
    let stamped = |kind: CommentOpKind| {
        let mut op = Op::new(id(1), kind);
        op.seq = Some(42);
        op
    };
    vec![
        measure_storage(
            "comments",
            "AddComment (root, 16B body)",
            &stamped(CommentOpKind::AddComment {
                anchor: id(2),
                parent: None,
                body: "looks great here".into(),
            }),
            &model,
        ),
        measure_storage(
            "comments",
            "AddComment (reply, 64B body)",
            &stamped(CommentOpKind::AddComment {
                anchor: id(2),
                parent: Some(id(3)),
                body: "agreed — let's tighten the corner radius and ship the next pass".into(),
            }),
            &model,
        ),
        measure_storage(
            "comments",
            "EditBody (32B body)",
            &stamped(CommentOpKind::EditBody {
                target: id(2),
                body: "rewriting after design review pass".into(),
            }),
            &model,
        ),
        measure_storage(
            "comments",
            "DeleteComment",
            &stamped(CommentOpKind::DeleteComment { target: id(2) }),
            &model,
        ),
    ]
}

fn presence_payloads() -> Vec<PresenceSize> {
    let peer: PeerId = 1;
    // Cursor: peer + (f32 x, f32 y) + small modifier byte.
    let cursor: Vec<u8> = postcard::to_allocvec(&(peer, 1234.5_f32, 678.9_f32, 0u8)).unwrap();
    // Viewport: (x, y, w, h, zoom).
    let viewport: Vec<u8> =
        postcard::to_allocvec(&(0_f32, 0_f32, 1920_f32, 1080_f32, 1.0_f32)).unwrap();
    // Selection: Vec<CrdtId>.
    let selection_1: Vec<u8> = postcard::to_allocvec(&vec![CrdtId::new(peer, 42)]).unwrap();
    let selection_5: Vec<u8> =
        postcard::to_allocvec(&(1u64..=5).map(|s| CrdtId::new(peer, s)).collect::<Vec<_>>())
            .unwrap();
    let selection_20: Vec<u8> =
        postcard::to_allocvec(&(1u64..=20).map(|s| CrdtId::new(peer, s)).collect::<Vec<_>>())
            .unwrap();
    // Combined awareness: cursor + viewport + selection_5 + display_name.
    let awareness: Vec<u8> = postcard::to_allocvec(&(
        (1234.5_f32, 678.9_f32),
        (0_f32, 0_f32, 1920_f32, 1080_f32),
        (1u64..=5).map(|s| CrdtId::new(peer, s)).collect::<Vec<_>>(),
        "Hector C.".to_string(),
    ))
    .unwrap();
    vec![
        measure_presence("cursor (peer + xy + modifier)", cursor),
        measure_presence("viewport (xywh + zoom)", viewport),
        measure_presence("selection (1 id)", selection_1),
        measure_presence("selection (5 ids)", selection_5),
        measure_presence("selection (20 ids)", selection_20),
        measure_presence("combined awareness (cursor+viewport+sel5+name)", awareness),
    ]
}

fn build_fanout_rows(storage: &[OpSize], presence: &[PresenceSize]) -> Vec<FanoutRow> {
    let smallest = storage.iter().map(|o| o.apply_bytes).min().unwrap_or(0) as u64;
    let largest = storage.iter().map(|o| o.apply_bytes).max().unwrap_or(0) as u64;
    let cursor = presence
        .iter()
        .find(|p| p.label.starts_with("cursor"))
        .map(|p| p.apply_bytes as u64)
        .unwrap_or(0);
    [4usize, 16, 64, 256, 1024]
        .into_iter()
        .map(|n| {
            let ops_per_sec_per_peer = 10u32;
            let fanout = (n.saturating_sub(1)) as u64;
            let total_per_op = (n as u64) * (ops_per_sec_per_peer as u64) * fanout;
            FanoutRow {
                n_peers: n,
                ops_per_sec_per_peer,
                smallest_storage_bytes_per_sec: total_per_op * smallest,
                largest_storage_bytes_per_sec: total_per_op * largest,
                cursor_presence_bytes_per_sec: total_per_op * cursor,
            }
        })
        .collect()
}

fn print_table(report: &WireReport) {
    eprintln!("=== Storage ops (server logs + retains) ===");
    eprintln!(
        "{:<10} {:<54} {:>8} {:>8} {:>8} {:>8}",
        "model", "variant", "payload", "submit", "apply", "envOH"
    );
    for o in &report.storage_ops {
        eprintln!(
            "{:<10} {:<54} {:>8} {:>8} {:>8} {:>8}",
            o.model, o.variant, o.payload_bytes, o.submit_bytes, o.apply_bytes, o.envelope_overhead
        );
    }
    eprintln!();
    eprintln!("=== Presence payloads (ephemeral, fanout-only) ===");
    eprintln!("{:<54} {:>8} {:>8} {:>8}", "label", "payload", "submit", "apply");
    for p in &report.presence_payloads {
        eprintln!(
            "{:<54} {:>8} {:>8} {:>8}",
            p.label, p.payload_bytes, p.submit_bytes, p.apply_bytes
        );
    }
    eprintln!();
    eprintln!("=== Server fanout egress @ 10 ops/sec/peer ===");
    eprintln!(
        "{:>8} {:>16} {:>16} {:>16}",
        "N", "smallest_op B/s", "largest_op B/s", "cursor B/s"
    );
    for r in &report.notes.fanout_per_sec_at_n {
        eprintln!(
            "{:>8} {:>16} {:>16} {:>16}",
            r.n_peers,
            r.smallest_storage_bytes_per_sec,
            r.largest_storage_bytes_per_sec,
            r.cursor_presence_bytes_per_sec,
        );
    }
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let mut storage_ops = graph_ops();
    storage_ops.extend(comment_ops());
    let presence = presence_payloads();
    let fanout = build_fanout_rows(&storage_ops, &presence);
    let report = WireReport {
        storage_ops,
        presence_payloads: presence,
        notes: WireNotes {
            fanout_per_sec_at_n: fanout,
        },
    };
    print_table(&report);
    let json = serde_json::to_string_pretty(&report)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    match args.output {
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, json)?;
            eprintln!("wrote report → {}", path.display());
        }
        None => println!("{json}"),
    }
    Ok(())
}
