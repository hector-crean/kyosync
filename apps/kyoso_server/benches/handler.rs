//! Micro-benches for the per-model `RoomModelHandler` impls + the
//! `Room` router itself.
//!
//! Run: `cargo bench -p kyoso_server`.
//!
//! What these catch:
//! - Per-handler `submit` regressions (graph). Runs under the handler's
//!   own append-lock, so contention isn't a factor here — this is the
//!   no-contention single-submit cost.
//! - `welcome_for` cost growth with op count (the late-joiner case).
//! - `Room` router overhead (HashMap lookup + broadcast send).

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;

use kyoso_crdt::{CrdtId, ModelId, Op, Tier};
use kyoso_graph_crdt::{OpKind, graph_model};
use kyoso_server::Room;
use kyoso_server::services::handler::HandlerFactory;
use kyoso_server::services::handlers::GraphHandlerFactory;
use kyoso_server::services::store::OpStore;
use tokio::runtime::Runtime;

fn rt() -> Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

async fn build_room() -> Arc<Room> {
    let factories: Vec<Box<dyn HandlerFactory>> = vec![
        Box::new(GraphHandlerFactory::new(OpStore::in_memory())),
    ];
    Arc::new(
        Room::restore("bench-room".to_string(), &factories)
            .await
            .unwrap(),
    )
}

fn graph_submit(c: &mut Criterion) {
    let rt = rt();
    let room = rt.block_on(build_room());
    let model = graph_model();
    let mut group = c.benchmark_group("graph_handler_submit");
    group.bench_function("add_node", |b| {
        let mut local_seq: u64 = 0;
        b.iter(|| {
            local_seq += 1;
            let op: Op<OpKind> = Op::new(CrdtId::new(1, local_seq), OpKind::AddNode);
            let payload = postcard::to_allocvec(&op).unwrap();
            rt.block_on(async {
                let _ = black_box(&room)
                    .submit(black_box(&model), Tier::ReadWrite, payload)
                    .await
                    .unwrap();
            });
        });
    });
    group.finish();
}

fn welcome_growth(c: &mut Criterion) {
    let rt = rt();
    let mut group = c.benchmark_group("welcome_for");
    group.sample_size(20); // welcome_for at 10k ops is slow; smaller sample is fine
    for &n in &[100usize, 1_000, 10_000] {
        // Pre-populate a room with `n` graph ops.
        let room = rt.block_on(async {
            let room = build_room().await;
            let model = graph_model();
            for i in 0..n as u64 {
                let op: Op<OpKind> = Op::new(CrdtId::new(1, i), OpKind::AddNode);
                let payload = postcard::to_allocvec(&op).unwrap();
                room.submit(&model, Tier::ReadWrite, payload).await.unwrap();
            }
            room
        });
        let models = vec![(graph_model(), 0_u64)];
        group.bench_function(format!("welcome_for_{n}_ops"), |b| {
            b.iter(|| {
                rt.block_on(async {
                    let g = black_box(&room)
                        .welcome_for(black_box(&models))
                        .await
                        .unwrap();
                    black_box(g);
                });
            });
        });
    }
    group.finish();
}

fn router_dispatch(c: &mut Criterion) {
    // Bench the cheap path — has_model lookup + a couple of map ops —
    // to catch any accidental allocation in the hot path.
    let rt = rt();
    let room = rt.block_on(build_room());
    let mut group = c.benchmark_group("room_router");
    group.bench_function("has_model_known", |b| {
        let m: ModelId = graph_model();
        b.iter(|| {
            black_box(room.has_model(black_box(&m)));
        });
    });
    group.bench_function("has_model_unknown", |b| {
        let m: ModelId = ModelId::new("never-registered");
        b.iter(|| {
            black_box(room.has_model(black_box(&m)));
        });
    });
    group.finish();
}

criterion_group!(benches, graph_submit, welcome_growth, router_dispatch);
criterion_main!(benches);
