# Data formats

_Status: draft. Cross-cutting reference for the wire / storage /
compute formats we use, and what we learn from rerun's
Arrow-everything model._

## What we have today

Two formats coexist on the wire:

- **postcard** — for CRDT envelopes (`EnvelopeClientMsg`,
  `EnvelopeServerMsg`, `Op<K>`, `Diff<K>`, `WireDelta`). Compact,
  variant-tagged, varint-friendly. ~13 bytes per `AddNode`. Great
  for high-frequency tiny messages.
- **opaque bytes** — `Vec<u8>` payloads inside the envelope (for
  presence, snapshots). Consumers postcard-encode their own structs
  inside.

This works because CRDT ops are tiny and structured; postcard is a
near-optimal fit.

## What rerun gets right

Rerun's design treats **all data as Arrow record batches**, including
logs, metrics, geometry. Key insights:

1. **Time is a first-class column.** Each row carries a `seq` /
   `timestamp` so you can scrub / range-query freely. The store is
   columnar so range queries by entity + time are cheap.
2. **Components are the unit of replication.** A `Position3D` is a
   3-column record batch. The schema is the type. Flexible without
   needing typed Rust structs everywhere.
3. **Same primitive end-to-end.** The wire format (gRPC streaming
   Arrow Flight), the in-memory format (`RecordBatch`), and the
   storage format (Parquet) are all Arrow. No serialization
   boundaries to cross.
4. **Datafusion-compatible queries.** Once data is in the columnar
   store, you get SQL/Datafusion for free.

For kyoso, **rerun's model fits the compute-output story but not the
CRDT op story**:
- CRDT ops are tiny, variant-tagged, ordered. postcard wins on size
  and decode speed.
- Compute outputs are larger, often tabular (an image is an array, a
  segmentation is rows of polygons), reused across rooms via the
  cache. Arrow wins on size, query, and tooling.

## Recommendation: **two formats, two purposes**

| Use case | Format | Why |
|---|---|---|
| CRDT envelopes (`Op<K>`, `EnvelopeClientMsg`, etc.) | **postcard** | Tiny payloads (~13B for AddNode), variant-tagged, fast decode |
| Snapshots | **postcard** for now | Already works; Arrow is overkill for graph state under N MB |
| Compute outputs (images, segmentations, embeddings) | **Arrow IPC** | Tabular, efficient, queryable in cache, fits rabona's existing port-schema model |
| Compute cache storage | **Arrow IPC** for outputs; raw bytes for opaque blobs | Same reason |
| Worker IPC (rabona's `Stage` ports) | **Arrow IPC** | Already designed in rabona's `flow` |

The two formats meet at the **`SetNodeProperty + WireDelta::LwwReplace`**
that publishes a compute output back to the graph. The `value:
Vec<u8>` inside the LwwReplace is the Arrow IPC payload. Clients that
want to render the output decode it as Arrow; clients that just want
to know "is there an output" don't have to decode it at all.

This gives us:
- CRDT path stays small + fast (postcard).
- Compute path benefits from Arrow's ecosystem (cross-process via
  Flight, columnar query, Parquet snapshot).
- Single bridge point: the LwwReplace value is bytes, format is
  Arrow IPC, schema is documented per-stage in the registry.

## Sketch — compute output as Arrow

```rust
// Stage's output port is an Arrow schema:
fn output_ports(&self) -> &[Port] {
    &[
        Port {
            name: "segmentation",
            schema: arrow_schema!(
                "polygon_id" => Int32,
                "vertex_x"   => Float32,
                "vertex_y"   => Float32,
                "label"      => Utf8,
                "confidence" => Float32,
            ),
        },
    ]
}

// Worker produces a RecordBatch matching the schema, encodes as
// Arrow IPC stream, ships back via the Submit op:
let batch: RecordBatch = stage.execute(...)?;
let mut buf = Vec::new();
let mut writer = StreamWriter::try_new(&mut buf, &batch.schema())?;
writer.write(&batch)?;
writer.finish()?;

let op = Op::new(SetNodeProperty {
    target: node_id,
    path: Path::field("compute_output"),
    delta: WireDelta::LwwReplace { value: buf },
});
submit_via_service_account(op);

// Client decodes on the way out:
let mut reader = StreamReader::try_new(Cursor::new(payload), None)?;
let batch = reader.next().unwrap()?;
// render the polygons...
```

Three things this gets right:
1. **Schema is the contract.** The stage's output port schema is
   documented; clients render against it. Versioning happens via
   stage version (already in the cache key).
2. **Cache value is reusable.** A cache hit returns the same Arrow
   bytes; client decodes once, renders once.
3. **Cross-process IPC is cheap.** Worker → orchestrator → wire →
   client all use the same byte representation. No transcoding.

## What about Arrow Flight for worker IPC?

Rabona uses `arrow::ipc` over the existing transport (queue / RPC).
Arrow Flight is the gRPC layer on top of Arrow IPC; gives you streaming
+ schema negotiation + auth.

For kyoso v1: **don't bother with Flight**. Use Arrow IPC over the
existing wire (bytes inside `WireDelta::LwwReplace` for results;
bytes inside `jobs.payload` for inputs). Adopt Flight if/when we have
high-throughput streaming compute (e.g., live ML inference on a
camera feed).

## Future: time-series query store?

Rerun's columnar store is genuinely impressive. For kyoso we could
imagine:
- The op log gets replicated to a Parquet store (one Parquet file
  per `(room, model, day)`).
- Datafusion / DuckDB / Polars query interface over it.
- Use cases: "show me every `Move` op on node X this week", "what
  was the room's state at 3:42 PM on Tuesday", analytics dashboards
  over usage patterns.

This is **way out of v1 scope** but the architecture allows it
because:
- The op log is durable + queryable in Postgres anyway (we just don't
  expose a query interface yet).
- Adding a Parquet replication tail is a separate background process,
  doesn't touch the realtime path.

Park the idea, revisit if a real product use case appears.

## Implementation plan

There's not really an implementation plan for "data formats" — it's a
reference doc. The relevant slices are:

- Compute path adopts Arrow for stage outputs (slice 3.B forward).
- Worker IPC uses Arrow IPC inside the `jobs.payload` jsonb ?  Or
  separate `bytea` columns? **OPEN — see below.**

## OPEN decisions

1. **Job payload format: jsonb or bytea or both?**
   - jsonb is Postgres-native, queryable.
   - For Arrow inputs we need bytes; jsonb-as-base64 is gross.
   - Probably split: `jobs.payload jsonb` for routing/metadata
     ("which room, which node, which stage version"), `jobs.input
     bytea` for the Arrow bytes.
2. **Snapshot format — stay postcard or migrate to Arrow?**
   - postcard works; Arrow would unlock query.
   - **Lean: stay postcard for v1. Migrate when we want query.**
3. **WireDelta::LwwReplace `value: Vec<u8>` semantics.** Currently
   opaque to the framework — the consuming Crdt impl interprets the
   bytes. For compute outputs we'd want the schema declared somewhere
   so generic tooling can decode. **Probably: add a `output_kind:
   String` mime-ish field to compute-node properties (already in
   01_storage.md cache schema) and consult it client-side.**
4. **Postcard vs MessagePack vs other binary serde formats.**
   Decided early in the project to use postcard. Not relitigating
   here — tradeoffs are well-understood.
