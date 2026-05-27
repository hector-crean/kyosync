"""Pluggable transport for routing :class:`SceneAgent` verb calls to a
concrete kyoso scene host (in-process Rust via PyO3, subprocess over
stdio, HTTP, …).

Every implementation only has to handle :meth:`Transport.dispatch` —
the per-verb routing is the same JSON shape the Rust `kyoso_agent_rpc`
crate will eventually expose: ``method`` is the verb name (e.g.
``"scan"``, ``"match"``), ``params`` is the verb's params object as a
JSON-serialisable ``dict``. The transport returns the verb's result
the same way.

The shape of ``params`` and the return value follows the JSON Schema
emitted by ``cargo run -p kyoso_agent --bin emit_schema``; the
generated :mod:`kyoso_agent._generated` Pydantic models are the
typed mirror.
"""

from __future__ import annotations

from typing import Any, Protocol


class Transport(Protocol):
    """Strategy interface — a thing that can dispatch a verb call.

    Implementations only have to handle ``dispatch(method, params)``.
    The :class:`SceneAgent` wraps every verb call as one such dispatch.
    """

    def dispatch(self, method: str, params: dict[str, Any]) -> Any:
        """Route a verb call. Return the verb's result as a JSON-shaped
        value (``dict`` / ``list`` / scalar / ``None``).

        Raise :class:`TransportError` on transport-level failures (no
        connection, malformed response, …). Application-level errors
        from the verb (e.g. ``MutateError``) come back as part of the
        return payload — they are not exceptions.
        """
        ...


class TransportError(RuntimeError):
    """A transport-layer failure — connection lost, protocol violation,
    timeout, …. Distinct from application-level errors (``MutateError``
    etc.) which travel as ordinary return payloads.
    """


# =============================================================================
# MockTransport — for tests + offline development.
# =============================================================================


class PyO3Transport:
    """In-process transport — runs the Rust ``SceneAgent`` directly in
    the current Python process via the ``kyoso_agent_py`` PyO3 module.

    Zero network hop, zero subprocess; each verb call hands a Python
    ``dict`` to the native extension, which routes through
    ``kyoso_agent_rpc::dispatch`` and returns a Python ``dict``.

    Requires the native module to be built and installed. From the repo
    root:

    .. code-block:: bash

       cd sdk/python && uv sync --extra dev
       cd ../../crates/kyoso_agent_py && uv run --project ../../sdk/python maturin develop --release

    or simpler:

    .. code-block:: bash

       ./scripts/build_pyo3.sh

    Raises :class:`TransportError` at construction time if the native
    module isn't importable.
    """

    def __init__(self) -> None:
        try:
            import kyoso_agent_py as _native  # type: ignore[import-not-found]
        except ImportError as exc:
            raise TransportError(
                "kyoso_agent_py native extension not installed — "
                "run `./scripts/build_pyo3.sh` from the repo root."
            ) from exc
        self._agent = _native.SceneAgent()

    def dispatch(self, method: str, params: dict[str, Any]) -> Any:
        return self._agent.dispatch(method, params)


class MockTransport:
    """In-memory canned-response transport. Register handlers per
    method name; calls go straight through to the registered callable.

    Used by the SDK's own test suite to exercise the typing layer
    without standing up a Rust host. Also useful for application
    tests that want to mock a small subset of verbs.

    >>> mt = MockTransport()
    >>> mt.on("scan", lambda params: {"session": "00000000-0000-0000-0000-000000000000",
    ...                                "generation": 0,
    ...                                "catalog": {"total_nodes": 0, "kind_counts": {}, "max_depth": 0},
    ...                                "roots": []})
    >>> mt.dispatch("scan", {})  # doctest: +ELLIPSIS
    {'session': ..., 'generation': 0, ...}
    """

    def __init__(self) -> None:
        self._handlers: dict[str, _Handler] = {}

    def on(self, method: str, handler: _Handler) -> "MockTransport":
        """Register a handler for ``method``. Returns self for chaining."""
        self._handlers[method] = handler
        return self

    def dispatch(self, method: str, params: dict[str, Any]) -> Any:
        if method not in self._handlers:
            raise TransportError(
                f"MockTransport has no handler for {method!r}; "
                f"register one with .on({method!r}, ...)"
            )
        return self._handlers[method](params)


# Type alias for handler callables passed to MockTransport.on(...).
_Handler = "Callable[[dict[str, Any]], Any]"  # forward-stringified to avoid runtime import

# Re-export at module top for type checkers.
from typing import Callable as _Callable  # noqa: E402

_Handler = _Callable[[dict[str, Any]], Any]
