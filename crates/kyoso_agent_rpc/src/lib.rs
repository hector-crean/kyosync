//! JSON-shaped dispatcher over the `kyoso_agent` verb surface.
//!
//! [`dispatch`] is a single function: give it a method name and a
//! `serde_json::Value` of params, get back a `Value` result (or a
//! [`DispatchError`]). It's the **single seam** every JSON-shaped
//! transport routes through — stdio JSON-RPC, HTTP, WebSocket, PyO3
//! (via [`kyoso_agent_py`](../kyoso_agent_py/index.html)),
//! wasm-bindgen, etc.
//!
//! # Why one dispatcher
//!
//! The 11-verb surface (`scan`, `inspect`, `walk`, `navigate`, `match`,
//! `watch`, `query`, `create`, `update`, `delete`, `move`) is small
//! enough to enumerate by hand once. Doing the string-name → trait-method
//! routing in *one* place means:
//!
//! - Adding a verb: edit the trait, edit the inherent forwarder, edit
//!   one arm in [`dispatch`]. No N transports to update.
//! - Mocking on the wire: callers can feed canned JSON straight at
//!   [`dispatch`]; no need to spin up a real transport.
//! - Cross-language SDKs: every wrapper (Python, TS, Swift, …) calls
//!   the same dispatcher. Wire shape can't diverge.
//!
//! # Wire shape
//!
//! Each verb's params shape mirrors the JSON Schema emitted by
//! `cargo run -p kyoso_agent --bin emit_schema`. Verbs that take two
//! positional arguments in Rust (`walk(root, opts)`, `navigate(from, opts)`,
//! `update(target, patch)`, `watch(since, opts)`) wrap them in an object
//! with named fields — exactly the structure the emitter and the
//! Python SDK already use.
//!
//! Mutate-verb errors travel as a [`DispatchError::Mutate`] variant —
//! callers wrapping in JSON-RPC can surface them as an `error` envelope;
//! callers wrapping in stdio can pretty-print them.
//!
//! # Example
//!
//! ```no_run
//! use kyoso_agent::SceneAgent;
//! use kyoso_agent_rpc::dispatch;
//! use serde_json::json;
//!
//! let mut agent = SceneAgent::new();
//! let result = dispatch(&mut agent, "scan", json!({})).unwrap();
//! // `result` is the SceneIndex shape from the JSON Schema.
//! ```

use kyoso_agent::{
    CreateSpec, Cursor, MoveSpec, MutateError, NavOpts, NodeTarget, PatternSpec, QuerySpec,
    SceneAgent, ScanOpts, UpdatePatch, WalkOpts, WatchOpts,
};
use serde::Deserialize;
use serde_json::Value;

/// Anything that can go wrong inside [`dispatch`]. Distinct from
/// `MutateError` — those are *expected* application-level failures the
/// agent might report; this enum is for transport/dispatch-layer issues
/// the agent host caused (typos, malformed payloads, or a failed
/// mutate that needs to surface to the caller).
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    /// `method` doesn't match any verb in the surface.
    #[error("unknown method: {0}")]
    UnknownMethod(String),

    /// `params` couldn't be deserialised into the verb's input type.
    /// The original `serde_json::Error` is stringified because
    /// `serde_json::Error` isn't `Clone` and we want this enum to be
    /// cheap to pass around.
    #[error("params parse error for `{method}`: {message}")]
    ParamsParse { method: String, message: String },

    /// One of the mutate verbs (`create` / `update` / `delete` / `move`)
    /// reported a domain error. Mirrors the Rust trait's
    /// `Result<MutateResult, MutateError>`.
    #[error("mutate error: {0:?}")]
    Mutate(MutateError),

    /// The verb ran fine but its return value couldn't be re-serialised
    /// to JSON. In practice this only fires if a wire type starts
    /// carrying a non-JSON-friendly inner field — which the
    /// `cargo run -p kyoso_agent --bin emit_schema` build guards
    /// against (every wire type derives `JsonSchema`).
    #[error("result serialize error for `{method}`: {message}")]
    ResultSerialize { method: String, message: String },
}

/// Route a single verb call. Returns the verb's return value as
/// `serde_json::Value`; the caller decides how to frame it (raw,
/// JSON-RPC envelope, MCP tool result, etc.).
///
/// The `agent` is borrowed mutably for the duration of the call —
/// every verb on `SceneAgent` already takes `&mut self`, and the
/// dispatcher inherits that.
pub fn dispatch(
    agent: &mut SceneAgent,
    method: &str,
    params: Value,
) -> Result<Value, DispatchError> {
    match method {
        // ----- Read verbs -----
        "scan" => {
            let opts: ScanOpts = parse(method, params)?;
            serialize(method, &agent.scan(opts))
        }
        "inspect" => {
            let target: NodeTarget = parse(method, params)?;
            serialize(method, &agent.inspect(target))
        }
        "walk" => {
            let p: RootAndOpts<WalkOpts> = parse(method, params)?;
            serialize(method, &agent.walk(p.root, p.opts))
        }
        "navigate" => {
            let p: RootAndOpts<NavOpts> = parse(method, params)?;
            serialize(method, &agent.navigate(p.root, p.opts))
        }
        "match" => {
            let spec: PatternSpec = parse(method, params)?;
            serialize(method, &agent.r#match(&spec))
        }
        "watch" => {
            let p: WatchParams = parse(method, params)?;
            serialize(method, &agent.watch(p.since, p.opts))
        }
        "query" => {
            let spec: QuerySpec = parse(method, params)?;
            serialize(method, &agent.query(spec))
        }
        // ----- Mutate verbs -----
        "create" => {
            let spec: CreateSpec = parse(method, params)?;
            let result = agent.create(spec).map_err(DispatchError::Mutate)?;
            serialize(method, &result)
        }
        "update" => {
            let p: UpdateParams = parse(method, params)?;
            let result = agent
                .update(p.target, p.patch)
                .map_err(DispatchError::Mutate)?;
            serialize(method, &result)
        }
        "delete" => {
            let target: NodeTarget = parse(method, params)?;
            let result = agent.delete(target).map_err(DispatchError::Mutate)?;
            serialize(method, &result)
        }
        "move" => {
            let spec: MoveSpec = parse(method, params)?;
            let result = agent.r#move(spec).map_err(DispatchError::Mutate)?;
            serialize(method, &result)
        }
        unknown => Err(DispatchError::UnknownMethod(unknown.to_string())),
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn parse<T: for<'de> Deserialize<'de>>(method: &str, params: Value) -> Result<T, DispatchError> {
    serde_json::from_value(params).map_err(|err| DispatchError::ParamsParse {
        method: method.to_string(),
        message: err.to_string(),
    })
}

fn serialize<T: serde::Serialize>(method: &str, value: &T) -> Result<Value, DispatchError> {
    serde_json::to_value(value).map_err(|err| DispatchError::ResultSerialize {
        method: method.to_string(),
        message: err.to_string(),
    })
}

// =============================================================================
// Params envelopes for multi-argument verbs.
//
// These mirror the JSON shapes the `emit_schema` binary documents —
// see `verb_with_root`, `verb_optional_since`, `verb_update` there.
// We re-derive `Deserialize` here so the dispatcher doesn't have to
// pattern-match raw `Value`s field-by-field.
// =============================================================================

#[derive(Deserialize)]
#[serde(bound(deserialize = "O: Default + Deserialize<'de>"))]
struct RootAndOpts<O> {
    root: NodeTarget,
    #[serde(default)]
    opts: O,
}

#[derive(Deserialize)]
struct WatchParams {
    #[serde(default)]
    since: Option<Cursor>,
    #[serde(default)]
    opts: WatchOpts,
}

#[derive(Deserialize)]
struct UpdateParams {
    target: NodeTarget,
    patch: UpdatePatch,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use kyoso_agent::spawn_demo_scene;
    use serde_json::json;

    fn fresh_agent_with_demo_scene() -> SceneAgent {
        let mut agent = SceneAgent::new();
        let _ = spawn_demo_scene(agent.scene_world());
        agent.scene_world().update();
        agent
    }

    // ----- Read-verb routing -----

    #[test]
    fn scan_with_empty_params_returns_a_scene_index() {
        let mut agent = fresh_agent_with_demo_scene();
        let result = dispatch(&mut agent, "scan", json!({})).expect("scan dispatch");

        // Generation + catalog + roots are always present in a SceneIndex.
        let obj = result.as_object().expect("object");
        assert!(obj.contains_key("session"));
        assert!(obj.contains_key("generation"));
        assert!(obj.contains_key("catalog"));
        assert!(obj.contains_key("roots"));
    }

    #[test]
    fn scan_with_explicit_opts_round_trips_kinds_filter() {
        let mut agent = fresh_agent_with_demo_scene();
        let params = json!({
            "depth": 1,
            "kinds": ["frame"],
            "max_outline_rows": 16,
        });
        let result = dispatch(&mut agent, "scan", params).expect("scan dispatch");

        // The depth-1 + kind=frame scan should give us Frame roots only
        // (Root + Header are Frames; Rectangle is at depth-1 and gets
        // filtered).
        let roots = result["roots"].as_array().expect("array");
        for root in roots {
            assert_eq!(root["kind"], json!("frame"));
        }
    }

    #[test]
    fn inspect_routes_node_target_input_form() {
        let mut agent = fresh_agent_with_demo_scene();
        // The demo scene root is at /Root. Inspect that.
        let params = json!({ "path": "/Root" });
        let result = dispatch(&mut agent, "inspect", params).expect("inspect dispatch");

        let obj = result.as_object().expect("object");
        assert!(obj.contains_key("node"));
        assert!(obj.contains_key("component_names"));
    }

    #[test]
    fn walk_envelopes_root_and_opts() {
        let mut agent = fresh_agent_with_demo_scene();
        let params = json!({
            "root": { "path": "/Root" },
            "opts": { "depth_limit": 1, "max_items": 10 }
        });
        let result = dispatch(&mut agent, "walk", params).expect("walk dispatch");

        let rows = result["rows"].as_array().expect("rows");
        assert!(!rows.is_empty(), "demo scene has visible descendants");
    }

    #[test]
    fn match_routes_pattern_spec() {
        let mut agent = fresh_agent_with_demo_scene();
        let params = json!({
            "nodes": [{}, {}],
            "edges": [{ "from": 0, "to": 1, "direction": "Forward" }],
        });
        let result = dispatch(&mut agent, "match", params).expect("match dispatch");
        assert!(result.is_array(), "match returns a list of MatchRefs");
    }

    // ----- Mutate verbs surface MutateError -----

    #[test]
    fn delete_on_unresolvable_target_returns_mutate_error() {
        let mut agent = fresh_agent_with_demo_scene();
        let params = json!({ "path": "/Nope" });
        let err = dispatch(&mut agent, "delete", params).expect_err("should fail");
        match err {
            DispatchError::Mutate(MutateError::TargetNotFound) => {}
            other => panic!("expected TargetNotFound, got {other:?}"),
        }
    }

    // ----- Dispatch-layer errors -----

    #[test]
    fn unknown_method_surfaces_cleanly() {
        let mut agent = SceneAgent::new();
        let err = dispatch(&mut agent, "nope", json!({})).expect_err("unknown");
        match err {
            DispatchError::UnknownMethod(s) => assert_eq!(s, "nope"),
            other => panic!("expected UnknownMethod, got {other:?}"),
        }
    }

    #[test]
    fn malformed_params_surface_a_parse_error_with_method_name() {
        let mut agent = SceneAgent::new();
        // ScanOpts expects an object — give it a number.
        let err = dispatch(&mut agent, "scan", json!(42)).expect_err("parse fail");
        match err {
            DispatchError::ParamsParse { method, .. } => assert_eq!(method, "scan"),
            other => panic!("expected ParamsParse, got {other:?}"),
        }
    }

    #[test]
    fn walk_envelope_missing_opts_uses_default() {
        let mut agent = fresh_agent_with_demo_scene();
        // No `opts` field — WalkParams should fall back to WalkOpts::default().
        let result = dispatch(
            &mut agent,
            "walk",
            json!({ "root": { "path": "/Root" } }),
        )
        .expect("walk with default opts");
        assert!(result.is_object());
    }
}
