"""End-to-end test driving the real Rust ``SceneAgent`` via the
``kyoso_agent_py`` PyO3 extension.

Skipped automatically if the native module isn't installed. To
build/install it for this venv:

.. code-block:: bash

   ./scripts/build_pyo3.sh           # from the repo root
"""

from __future__ import annotations

import pytest

from kyoso_agent import (
    NavDir,
    NavEdgeFilter,
    NavOpts,
    PatternSpec,
    PyO3Transport,
    ScanOpts,
    SceneAgent,
    TransportError,
    WalkOpts,
)

# Pre-flight: skip the whole module if the native extension isn't loadable.
try:
    PyO3Transport()  # bare construction is enough to flush an ImportError
except TransportError as exc:
    pytest.skip(f"kyoso_agent_py not built: {exc}", allow_module_level=True)


@pytest.fixture
def agent() -> SceneAgent:
    """Fresh in-process SceneAgent for each test."""
    return SceneAgent(PyO3Transport())


def test_scan_on_empty_scene_returns_zero_nodes(agent: SceneAgent) -> None:
    """The PyO3 SceneAgent ships with no spawned scene — scan should
    return an empty SceneIndex."""
    idx = agent.scan(ScanOpts())
    assert idx.catalog.total_nodes == 0
    assert idx.roots == []


def test_scan_with_explicit_depth(agent: SceneAgent) -> None:
    idx = agent.scan(ScanOpts(depth=3, max_outline_rows=64, kinds=[]))
    # Still empty scene, but the verb routed and returned a SceneIndex.
    assert idx.catalog.max_depth == 0


def test_walk_under_unknown_root_is_empty(agent: SceneAgent) -> None:
    """Walking under a non-existent NodeRef returns an empty result —
    `Walk.rows` is empty, no error raised."""
    from kyoso_agent import NodeRef, ScenePath

    walk = agent.walk(NodeRef(path=ScenePath(root="/does/not/exist")), WalkOpts())
    assert walk.rows == []


def test_match_routes_via_dispatch(agent: SceneAgent) -> None:
    """Single-node PatternSpec on empty scene returns no matches.

    NB: An *empty* pattern (zero nodes) currently panics inside the
    subgraph-isomorphism iterator — that's a pre-existing kyoso_graph
    edge case, separate from the PyO3 surface. Use a 1-node pattern to
    exercise the dispatch path without tripping it.
    """
    from kyoso_agent import NodePattern, NodeKind

    spec = PatternSpec(nodes=[NodePattern(kind=NodeKind.frame)], edges=[])
    result = agent.match_(spec)
    assert result == []


def test_unknown_method_raises(agent: SceneAgent) -> None:
    """The raw dispatch path: bypass the typed methods to confirm the
    PyO3 layer correctly surfaces UnknownMethod as a ValueError."""
    transport = agent._transport  # noqa: SLF001 — internal but stable for this assert
    with pytest.raises(ValueError, match="unknown method"):
        transport.dispatch("not_a_verb", {})


def test_navigate_routes_root_and_opts(agent: SceneAgent) -> None:
    """Navigate from a non-existent ref returns empty — verifies the
    `{root, opts}` envelope is unpacked correctly on the Rust side."""
    from kyoso_agent import NodeRef, ScenePath

    result = agent.navigate(
        NodeRef(path=ScenePath(root="/missing")),
        NavOpts(direction=NavDir.out, edges=NavEdgeFilter.tree_only),
    )
    assert result == []
