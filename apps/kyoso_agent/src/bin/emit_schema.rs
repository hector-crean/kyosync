//! Emit a combined JSON Schema covering every kyoso_agent wire type
//! (params + results for every `SceneRead` / `SceneMutate` verb).
//!
//! Output shape — a draft-2020-12 JSON Schema document with `$defs`
//! containing one entry per wire type. The seven verbs surface as
//! top-level entries pairing each verb with its params + result type
//! by `$ref`. Pipe to a file for downstream codegen:
//!
//! ```bash
//! cargo run -p kyoso_agent --bin emit_schema > schema.json
//! ```
//!
//! Downstream consumers:
//! - **MCP** — read `verbs[].params` to build a `tools.json` descriptor.
//! - **Python (Pydantic)** — feed `$defs` into `datamodel-codegen`.
//! - **TypeScript (Zod / interfaces)** — feed `$defs` into
//!   `json-schema-to-zod` or `json-schema-to-typescript`.

use std::collections::BTreeMap;

use kyoso_agent::{
    Cursor, EdgeRef, EntityReport, MatchRefs, NavOpts, NodeRef, NodeTarget, PatternSpec,
    QueryResult, QuerySpec, ScanOpts, SceneIndex, SessionId, Walk, WalkOpts, WatchOpts, WatchPage,
};
use kyoso_agent::{CreateSpec, MoveSpec, MutateError, MutateResult, NewNode, UpdatePatch};
use schemars::{schema_for, JsonSchema};
use serde_json::{json, Value};

fn main() {
    // Verb table: name → (params type, result type). Doubles as the MCP
    // tool surface — each row becomes one tool descriptor downstream.
    let verbs: Vec<(&str, &str, Value)> = vec![
        verb::<ScanOpts, SceneIndex>("scan", "Catalog + depth-bounded outline of the scene"),
        verb::<NodeTarget, EntityReport>(
            "inspect",
            "Schemaless component dump + typed variant for one target",
        ),
        verb_with_root::<WalkOpts, Walk>(
            "walk",
            "Subtree walk under a root with depth / kind / budget caps",
        ),
        verb_with_root::<NavOpts, Vec<NodeRef>>(
            "navigate",
            "One-hop (or transitive) neighbourhood query",
        ),
        verb::<PatternSpec, Vec<MatchRefs>>(
            "match",
            "Subgraph-isomorphism pattern matching in NodeRef space",
        ),
        verb_optional_since::<WatchOpts, WatchPage>(
            "watch",
            "Coalesced change page since the cursor",
        ),
        verb::<QuerySpec, QueryResult>(
            "query",
            "Generic ECS component-presence filter — escape hatch",
        ),
        verb::<CreateSpec, MutateResult>("create", "Spawn a new node"),
        verb_update::<UpdatePatch, MutateResult>("update", "Apply a partial patch to a target"),
        verb::<NodeTarget, MutateResult>("delete", "Despawn a target (cascades through ChildOf)"),
        verb::<MoveSpec, MutateResult>("move", "Reparent or reorder a node"),
    ];

    // Build one combined schema document. We register every wire type
    // into a single `SchemaGenerator`; schemars walks the type graph,
    // so transitively-reached types land in the generator's
    // `definitions` store too. `take_definitions` then hands us the
    // full `BTreeMap<String, Value>` for `$defs`.
    let mut generator = schemars::SchemaGenerator::default();
    register::<ScanOpts>(&mut generator);
    register::<SceneIndex>(&mut generator);
    register::<WalkOpts>(&mut generator);
    register::<Walk>(&mut generator);
    register::<NavOpts>(&mut generator);
    register::<WatchOpts>(&mut generator);
    register::<WatchPage>(&mut generator);
    register::<QuerySpec>(&mut generator);
    register::<QueryResult>(&mut generator);
    register::<PatternSpec>(&mut generator);
    register::<MatchRefs>(&mut generator);
    register::<EdgeRef>(&mut generator);
    register::<EntityReport>(&mut generator);
    register::<NodeRef>(&mut generator);
    register::<NodeTarget>(&mut generator);
    register::<Cursor>(&mut generator);
    register::<SessionId>(&mut generator);
    register::<CreateSpec>(&mut generator);
    register::<NewNode>(&mut generator);
    register::<UpdatePatch>(&mut generator);
    register::<MoveSpec>(&mut generator);
    register::<MutateResult>(&mut generator);
    register::<MutateError>(&mut generator);

    let defs: BTreeMap<String, Value> = generator
        .take_definitions(true)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();

    let document = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "kyoso_agent",
        "description": "Wire surface for the kyoso_agent SceneRead / SceneMutate verbs.",
        "verbs": verbs
            .into_iter()
            .map(|(name, description, body)| {
                let mut v = json!({ "name": name, "description": description });
                merge(&mut v, body);
                v
            })
            .collect::<Vec<_>>(),
        "$defs": defs,
    });

    println!("{}", serde_json::to_string_pretty(&document).unwrap());
}

/// Register `T` (and transitively-reached types) into the shared
/// `SchemaGenerator`'s definitions store. The actual schemas are
/// collected at the end via `take_definitions`.
fn register<T: JsonSchema + ?Sized>(generator: &mut schemars::SchemaGenerator) {
    let _ = generator.subschema_for::<T>();
}

/// Build a verb entry pointing `params` at the trait method's
/// param type and `result` at its return type. Both are `$ref` strings
/// into `$defs`, so consumers can resolve names → full schemas.
fn verb<P: JsonSchema, R: JsonSchema>(name: &'static str, description: &'static str) -> (
    &'static str,
    &'static str,
    Value,
) {
    (
        name,
        description,
        json!({
            "params": { "$ref": format!("#/$defs/{}", schema_for!(P).get("title").and_then(Value::as_str).unwrap_or_default()) },
            "result": { "$ref": format!("#/$defs/{}", schema_for!(R).get("title").and_then(Value::as_str).unwrap_or_default()) },
        }),
    )
}

/// Like [`verb`], but for verbs that take an additional `root: NodeTarget`
/// alongside their opts struct (walk, navigate).
fn verb_with_root<P: JsonSchema, R: JsonSchema>(
    name: &'static str,
    description: &'static str,
) -> (&'static str, &'static str, Value) {
    (
        name,
        description,
        json!({
            "params": {
                "type": "object",
                "required": ["root", "opts"],
                "properties": {
                    "root": { "$ref": "#/$defs/NodeTarget" },
                    "opts": { "$ref": format!("#/$defs/{}", schema_for!(P).get("title").and_then(Value::as_str).unwrap_or_default()) },
                },
            },
            "result": { "$ref": format!("#/$defs/{}", schema_for!(R).get("title").and_then(Value::as_str).unwrap_or_default()) },
        }),
    )
}

/// `watch(since: Option<Cursor>, opts: WatchOpts) -> WatchPage`.
fn verb_optional_since<P: JsonSchema, R: JsonSchema>(
    name: &'static str,
    description: &'static str,
) -> (&'static str, &'static str, Value) {
    (
        name,
        description,
        json!({
            "params": {
                "type": "object",
                "required": ["opts"],
                "properties": {
                    "since": { "anyOf": [{ "$ref": "#/$defs/Cursor" }, { "type": "null" }] },
                    "opts": { "$ref": format!("#/$defs/{}", schema_for!(P).get("title").and_then(Value::as_str).unwrap_or_default()) },
                },
            },
            "result": { "$ref": format!("#/$defs/{}", schema_for!(R).get("title").and_then(Value::as_str).unwrap_or_default()) },
        }),
    )
}

/// `update(target: NodeTarget, patch: UpdatePatch) -> MutateResult`.
fn verb_update<P: JsonSchema, R: JsonSchema>(
    name: &'static str,
    description: &'static str,
) -> (&'static str, &'static str, Value) {
    (
        name,
        description,
        json!({
            "params": {
                "type": "object",
                "required": ["target", "patch"],
                "properties": {
                    "target": { "$ref": "#/$defs/NodeTarget" },
                    "patch": { "$ref": format!("#/$defs/{}", schema_for!(P).get("title").and_then(Value::as_str).unwrap_or_default()) },
                },
            },
            "result": { "$ref": format!("#/$defs/{}", schema_for!(R).get("title").and_then(Value::as_str).unwrap_or_default()) },
        }),
    )
}

/// Merge `extras` into `target` (both must be JSON objects). Shallow —
/// keys in `extras` overwrite those in `target`.
fn merge(target: &mut Value, extras: Value) {
    if let (Some(t), Value::Object(e)) = (target.as_object_mut(), extras) {
        for (k, v) in e {
            t.insert(k, v);
        }
    }
}
