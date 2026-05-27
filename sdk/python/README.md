# kyoso-agent (Python SDK)

Typed Python client for the [kyoso](https://github.com/hectorcrean/kyoso) scene-agent surface.

The wire types are **generated from the Rust `kyoso_agent` crate's JSON Schema** (see `scripts/codegen.sh` in the repo root), so every parameter, return value, and discriminator stays in lock-step with the canonical Rust surface. They're emitted as Pydantic v2 models under `kyoso_agent._generated`.

The `SceneAgent` class itself is hand-written and **pluggable** — it takes a `Transport` (one of `MockTransport`, `SubprocessTransport`, `HTTPTransport`, an in-process PyO3 binding when one ships) and routes each verb call through it. That keeps the client identical whether you're driving an in-process Rust app, a long-running daemon over stdio, or a remote server.

## Install (dev)

```bash
cd sdk/python
uv sync               # creates .venv, installs runtime + dev deps
uv run pytest         # run tests
```

## Regenerate the wire types

```bash
# from the repo root
./scripts/codegen.sh python
```

This runs the Rust schema emitter and pipes the result through `datamodel-codegen`. The generated file is `src/kyoso_agent/_generated.py` — never edit it by hand.

## Status

- ✓ Pydantic types for every verb's params + return values
- ✓ `SceneAgent` class shape with the 11 verbs (`scan`, `inspect`, `walk`, `navigate`, `match_`, `watch`, `query`, `create`, `update`, `delete`, `move_`)
- ✓ `MockTransport` for tests
- ◯ `SubprocessTransport` (stdio JSON-RPC; not yet implemented)
- ◯ `HTTPTransport` (requires a server-side adapter; not yet implemented)
- ◯ `PyO3Transport` (in-process; requires `kyoso_agent_py` Rust crate; not yet implemented)
