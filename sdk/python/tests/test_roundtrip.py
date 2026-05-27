"""End-to-end smoke tests for the Python SDK.

Two goals:

1. Wire-shape parity — every param/return type round-trips through
   ``model_dump(mode="json")`` → JSON → ``model_validate`` losslessly.
   This is the gate that catches Pydantic regenerations diverging from
   the Rust serde shape.

2. ``SceneAgent`` ↔ ``Transport`` glue — a :class:`MockTransport`
   wired up to canned responses lets us run every verb without a Rust
   host, exercising the JSON serialisation + result parsing the
   real client will do.
"""

from __future__ import annotations

import json
from uuid import UUID

from kyoso_agent import (
    ChangeKind,
    Cursor,
    LayoutMode,
    MockTransport,
    NavDir,
    NavEdgeFilter,
    NavOpts,
    NewNode,
    NodeKind,
    NodeRef,
    PatternSpec,
    ScanOpts,
    SceneAgent,
    SceneIndex,
    ScenePath,
    SessionId,
    WalkOpts,
    WatchOpts,
)
from kyoso_agent._generated import NewNode1


# =============================================================================
# Section 1 — wire-shape parity. The tests below build a Pydantic model
# in Python, serialise it to JSON, parse it back, and confirm equality.
# If any of these break after a codegen pass, the regenerated Pydantic
# models have drifted from the schemars-emitted shape.
# =============================================================================


def _roundtrip(value):  # type: ignore[no-untyped-def]
    """Dump → JSON-string → load → re-validate. Returns the rehydrated
    model. Equality assertions sit at the call sites so failures point
    at the specific type."""
    cls = type(value)
    raw = value.model_dump(mode="json", by_alias=True, exclude_none=True)
    s = json.dumps(raw)
    parsed = json.loads(s)
    return cls.model_validate(parsed)


def test_scan_opts_default_roundtrips() -> None:
    opts = ScanOpts(depth=2, max_outline_rows=256, kinds=[])
    assert _roundtrip(opts) == opts


def test_scan_opts_with_filters_roundtrips() -> None:
    opts = ScanOpts(
        depth=3,
        max_outline_rows=128,
        kinds=[NodeKind.frame, NodeKind.text],
        under=NodeRef(path=ScenePath(root="/Root/Header")),
    )
    assert _roundtrip(opts) == opts


def test_layout_mode_serializes_snake_case() -> None:
    """Reserved-keyword rename: `None` → "none" on the wire."""
    raw = LayoutMode.none.model_dump(mode="json") if hasattr(LayoutMode.none, "model_dump") else LayoutMode.none.value
    # Enum value lookup goes through .value (Pydantic Enum field).
    assert LayoutMode.none.value == "none"
    assert LayoutMode.horizontal.value == "horizontal"
    assert LayoutMode.vertical.value == "vertical"


def test_node_kind_wire_form_is_snake_case() -> None:
    """`NodeKind` has `#[serde(rename_all = "snake_case")]`."""
    assert NodeKind.frame.value == "frame"
    assert NodeKind.rectangle.value == "rectangle"
    assert NodeKind.text.value == "text"


def test_nav_dir_reserved_in_variant_keeps_pascal_value() -> None:
    """`NavDir::In` collides with Python `in`; datamodel-codegen renames
    the member to `in_` but preserves the PascalCase wire string."""
    assert NavDir.in_.value == "In"
    assert NavDir.out.value == "Out"


def test_walk_opts_roundtrips() -> None:
    opts = WalkOpts(strategy="Dfs", depth_limit=5, kinds=[NodeKind.frame], max_items=100)
    assert _roundtrip(opts) == opts


def test_nav_opts_roundtrips() -> None:
    opts = NavOpts(
        direction=NavDir.out,
        edges=NavEdgeFilter.tree_only,
        kinds=[NodeKind.text],
        depth_limit=2,
        max_items=50,
    )
    assert _roundtrip(opts) == opts


def test_watch_opts_roundtrips() -> None:
    opts = WatchOpts(events=[ChangeKind.added, ChangeKind.modified], max_items=100, kinds=[])
    assert _roundtrip(opts) == opts


def test_pattern_spec_empty_roundtrips() -> None:
    spec = PatternSpec(nodes=[], edges=[])
    assert _roundtrip(spec) == spec


def test_cursor_baseline_roundtrips() -> None:
    cursor = Cursor(
        session=SessionId(root=UUID("00000000-0000-0000-0000-000000000001")),
        generation=0,
    )
    assert _roundtrip(cursor) == cursor


def test_node_ref_string_path_form() -> None:
    """`ScenePath` is `RootModel[str]` — verify it serialises as a
    bare string, not an object."""
    nr = NodeRef(path=ScenePath(root="/Root/Header"))
    raw = nr.model_dump(mode="json", by_alias=True, exclude_none=True)
    assert raw == {"path": "/Root/Header"}


# =============================================================================
# Section 2 — SceneAgent end-to-end through MockTransport. Confirms
# the request shape (what the client sends) and the response shape
# (what it parses back) both match the Pydantic models.
# =============================================================================


def _make_agent_with_canned(method: str, response):  # type: ignore[no-untyped-def]
    """Build a SceneAgent wired to a MockTransport returning ``response``
    for ``method``. ``response`` is the JSON-shaped dict the wire would
    actually carry."""
    transport = MockTransport().on(method, lambda _params: response)
    return SceneAgent(transport)


def test_agent_scan_dispatches_and_parses() -> None:
    canned = {
        "session": "00000000-0000-0000-0000-000000000000",
        "generation": 42,
        "catalog": {"total_nodes": 5, "kind_counts": {"frame": 2, "text": 2, "rectangle": 1}, "max_depth": 2},
        "roots": [],
    }
    agent = _make_agent_with_canned("scan", canned)
    idx = agent.scan(ScanOpts())
    assert isinstance(idx, SceneIndex)
    assert idx.generation == 42
    assert idx.catalog.total_nodes == 5
    assert idx.catalog.max_depth == 2


def test_agent_navigate_dispatches_with_root() -> None:
    """`navigate(from, opts)` should send ``{"root": ..., "opts": ...}``
    matching the Rust ``verb_with_root`` shape."""
    captured: dict[str, object] = {}

    def handler(params):  # type: ignore[no-untyped-def]
        captured.update(params)
        return []

    agent = SceneAgent(MockTransport().on("navigate", handler))
    nr = NodeRef(path=ScenePath(root="/Root"))
    result = agent.navigate(nr, NavOpts(direction=NavDir.out, edges=NavEdgeFilter.tree_only))
    assert result == []
    assert "root" in captured
    assert "opts" in captured
    # The NodeTarget untagged enum: a NodeRef target serialises as the
    # NodeRef object directly (the inner variant), not wrapped.
    assert captured["root"] == {"path": "/Root"}


def test_agent_match_handles_method_rename() -> None:
    """Python identifier is ``match_``; wire method is ``"match"``."""
    captured: list[str] = []

    def handler(_params):  # type: ignore[no-untyped-def]
        captured.append("called")
        return []

    agent = SceneAgent(MockTransport().on("match", handler))
    agent.match_(PatternSpec(nodes=[], edges=[]))
    assert captured == ["called"]


def test_agent_create_with_new_node_kind_tag() -> None:
    """`NewNode` is `#[serde(tag = "kind", rename_all = "snake_case")]`
    over tuple variants — confirm the wire shape has the discriminator
    + flattened payload."""
    sent: dict[str, object] = {}

    def handler(params):  # type: ignore[no-untyped-def]
        sent.update(params)
        return {
            "node": {"path": "/NewFrame"},
            "cursor": {
                "session": "00000000-0000-0000-0000-000000000000",
                "generation": 1,
            },
        }

    # Build NewNode::Frame variant.
    from kyoso_agent._generated import CreateSpec, NewNode1, Frame, FrameData

    payload = NewNode(
        root=NewNode1(
            frame=Frame(name="X", clips_content=False, layout_mode=LayoutMode.none, fills=[], strokes=[], stroke_weight=0.0),
            size={"width": 10.0, "height": 10.0},  # type: ignore[arg-type]
            kind="frame",
        )
    )
    spec = CreateSpec(data=payload)
    agent = SceneAgent(MockTransport().on("create", handler))
    result = agent.create(spec)
    assert result.node.path.root == "/NewFrame"
    # Confirm the wire form includes the discriminator + flattened payload.
    data = sent["data"]
    assert isinstance(data, dict)
    assert data.get("kind") == "frame"
    assert "frame" in data and isinstance(data["frame"], dict)
    assert data["frame"]["name"] == "X"


def test_mock_transport_raises_on_unknown_method() -> None:
    """Sanity: unmocked verbs should fail loudly rather than silently."""
    from kyoso_agent import TransportError

    agent = SceneAgent(MockTransport())  # no handlers registered

    try:
        agent.scan()
    except TransportError as e:
        assert "scan" in str(e)
    else:
        raise AssertionError("expected TransportError")
