# @kyoso/agent (TypeScript SDK)

> Status: **scaffold only**. The Pydantic side (`sdk/python`) is the
> reference implementation. This package's layout and codegen pipeline
> are ready; the actual generation step + hand-written client + tests
> come next.

Typed TypeScript client for the [kyoso](https://github.com/hectorcrean/kyoso) scene-agent surface.

Mirrors the structure of `sdk/python`:

- **Wire types** auto-generated from the Rust crate's JSON Schema via
  `json-schema-to-typescript`. Lives in `src/_generated.ts` after
  running the codegen script.
- **`SceneAgent`** class hand-written, dispatches each verb through a
  pluggable `Transport`.
- **`Transport`** strategy interface — `MockTransport` for tests; real
  transports (WS, subprocess, in-process via wasm-bindgen) layered on
  later without touching the verb surface.

## Regenerate the wire types

From the repo root:

```bash
./scripts/codegen.sh ts
```

This runs the Rust schema emitter and pipes `schema.json` through
`json-schema-to-typescript`. The generated file is
`src/_generated.ts` — never edit it by hand.

## Status

- ✓ Directory layout + `package.json` + `tsconfig.json`
- ✓ `scripts/codegen.sh ts` entry point
- ◯ Run the TS codegen step
- ◯ Hand-written `client.ts` + `transport.ts`
- ◯ Vitest test suite mirroring `sdk/python/tests/test_roundtrip.py`
