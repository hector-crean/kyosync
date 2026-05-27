"""High-level Python client for the kyoso scene-agent surface.

Every verb in the Rust ``SceneRead`` / ``SceneMutate`` traits surfaces
as one method on :class:`SceneAgent`. The method:

1. Pydantic-validates the params (catches typos before any wire trip),
2. Dumps them to JSON via ``.model_dump(mode="json", by_alias=True, exclude_none=True)``
   so the shape matches what serde emits on the Rust side,
3. Routes through the :class:`Transport` (mock / subprocess / HTTP / PyO3),
4. Pydantic-parses the response into the typed return model.

The transport is pluggable so the same client works against:

- a :class:`MockTransport` in tests (canned responses),
- a future ``PyO3Transport`` (in-process Rust via the
  ``kyoso_agent_py`` crate),
- a future ``SubprocessTransport`` (long-running Rust process over
  stdio JSON-RPC),
- a future ``HTTPTransport`` (against a Rust server adapter).

The verb names match the Rust trait exactly; the Python identifiers
that collide with reserved words use trailing underscores (``match_``,
``move_``) — the wire ``method`` string still routes to ``"match"`` /
``"move"``.
"""

from __future__ import annotations

from typing import Any

from ._generated import (
    CreateSpec,
    Cursor,
    EntityReport,
    MatchRefs,
    MoveSpec,
    MutateResult,
    NavOpts,
    NodeRef,
    NodeTarget,
    PatternSpec,
    QueryResult,
    QuerySpec,
    ScanOpts,
    SceneIndex,
    UpdatePatch,
    Walk,
    WalkOpts,
    WatchOpts,
    WatchPage,
)
from .transport import Transport


class SceneAgent:
    """Typed Python facade over the Rust scene-agent verb surface.

    Hands the JSON shape produced by ``params.model_dump(mode="json", …)``
    to the configured :class:`Transport`, parses the result back through
    the matching return model.

    Constructed with whichever transport is appropriate:

    .. code-block:: python

       from kyoso_agent import SceneAgent
       from kyoso_agent.transport import MockTransport

       agent = SceneAgent(MockTransport().on("scan", lambda _: {...}))
       idx = agent.scan(ScanOpts())
    """

    def __init__(self, transport: Transport) -> None:
        self._transport = transport

    # ------------------------------------------------------------------
    # Read verbs
    # ------------------------------------------------------------------

    def scan(self, opts: ScanOpts | None = None) -> SceneIndex:
        """Catalog + depth-bounded outline of the scene."""
        return _parse(SceneIndex, self._dispatch("scan", _dump(opts or ScanOpts())))

    def inspect(self, target: NodeTarget | NodeRef) -> EntityReport:
        """Schemaless component dump + typed variant for one target."""
        return _parse(EntityReport, self._dispatch("inspect", _dump(_as_target(target))))

    def walk(self, root: NodeTarget | NodeRef, opts: WalkOpts | None = None) -> Walk:
        """Subtree walk under ``root`` with depth / kind / budget caps."""
        params = {"root": _dump(_as_target(root)), "opts": _dump(opts or WalkOpts())}
        return _parse(Walk, self._dispatch("walk", params))

    def navigate(self, from_: NodeTarget | NodeRef, opts: NavOpts | None = None) -> list[NodeRef]:
        """One-hop (or transitive) neighbourhood query."""
        params = {"root": _dump(_as_target(from_)), "opts": _dump(opts or NavOpts())}
        raw = self._dispatch("navigate", params)
        return [NodeRef.model_validate(r) for r in (raw or [])]

    def match_(self, spec: PatternSpec) -> list[MatchRefs]:
        """Subgraph-isomorphism pattern matching in NodeRef space.

        Python identifier is ``match_`` because ``match`` is a soft
        keyword (Python 3.10+ pattern matching); the wire method is
        ``"match"``.
        """
        raw = self._dispatch("match", _dump(spec))
        return [MatchRefs.model_validate(r) for r in (raw or [])]

    def watch(self, since: Cursor | None = None, opts: WatchOpts | None = None) -> WatchPage:
        """Coalesced change page since ``since``."""
        params: dict[str, Any] = {"opts": _dump(opts or WatchOpts())}
        if since is not None:
            params["since"] = _dump(since)
        return _parse(WatchPage, self._dispatch("watch", params))

    def query(self, spec: QuerySpec) -> QueryResult:
        """Generic ECS component-presence filter — escape hatch."""
        return _parse(QueryResult, self._dispatch("query", _dump(spec)))

    # ------------------------------------------------------------------
    # Mutate verbs
    # ------------------------------------------------------------------

    def create(self, spec: CreateSpec) -> MutateResult:
        """Spawn a new node."""
        return _parse(MutateResult, self._dispatch("create", _dump(spec)))

    def update(
        self,
        target: NodeTarget | NodeRef,
        patch: UpdatePatch,
    ) -> MutateResult:
        """Apply a partial patch to ``target``."""
        params = {"target": _dump(_as_target(target)), "patch": _dump(patch)}
        return _parse(MutateResult, self._dispatch("update", params))

    def delete(self, target: NodeTarget | NodeRef) -> MutateResult:
        """Despawn ``target`` (cascades through ``ChildOf``)."""
        return _parse(MutateResult, self._dispatch("delete", _dump(_as_target(target))))

    def move_(self, spec: MoveSpec) -> MutateResult:
        """Reparent or reorder a node.

        Python identifier is ``move_`` because ``move`` isn't a keyword
        but the trailing underscore matches ``match_`` for symmetry;
        wire method is ``"move"``.
        """
        return _parse(MutateResult, self._dispatch("move", _dump(spec)))

    # ------------------------------------------------------------------
    # Internals
    # ------------------------------------------------------------------

    def _dispatch(self, method: str, params: Any) -> Any:
        return self._transport.dispatch(method, params)


# =============================================================================
# Helpers
# =============================================================================


def _dump(model: Any) -> Any:
    """Serialise a Pydantic model to the wire shape Rust expects.

    - ``mode="json"`` so e.g. UUIDs become strings rather than UUID objects.
    - ``by_alias=True`` reserved for future field-rename use; harmless now.
    - ``exclude_none=True`` matches the Rust ``#[serde(skip_serializing_if = "Option::is_none")]``
      defaults so the wire payload is tight.
    """
    return model.model_dump(mode="json", by_alias=True, exclude_none=True)


def _parse(cls: type, value: Any) -> Any:
    """Pydantic-validate ``value`` into ``cls``."""
    return cls.model_validate(value)


def _as_target(t: NodeTarget | NodeRef) -> NodeTarget:
    """Accept either a raw :class:`NodeRef` or an already-wrapped
    :class:`NodeTarget` and return a :class:`NodeTarget`. Mirrors the
    Rust ``impl Into<NodeTarget>`` ergonomics.
    """
    if isinstance(t, NodeTarget):
        return t
    return NodeTarget(root=t)
