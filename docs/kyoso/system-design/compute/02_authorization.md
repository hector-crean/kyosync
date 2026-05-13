# Authorization

_Status: draft._

## What we're replacing

Phase 1 added `Tier::ReadWrite | Tier::Read` to the `Hello` envelope
and a `RoomModelHandler::allows_submit(tier, payload)` policy hook.
The server currently **trusts the client's claimed tier
unconditionally** — there's no notion of identity, no permission
check, no token. This works for the test rig and the loadgen but is
obviously not production-ready.

## Identity model

**Token-based, JWT in `Authorization: Bearer <jwt>` on the WS
upgrade request.** Standard, well-understood, no novel
infrastructure.

- **Issuer**: a separate auth service (or for v1, a small
  `kyoso_auth` binary that signs tokens after a password / OAuth
  flow). RS256 — public key the kyoso_server trusts, private key on
  the auth service only.
- **Token claims**: `sub` (user id, uuid), `exp`, `iat`,
  `kind: "user" | "service"`, optional `service_account_id` for
  workers.
- **Verification**: kyoso_server holds the auth-service public key
  (loaded from env var or a JWKS endpoint). Verifies signature on
  every Hello. Rejects expired tokens with `Error { message }` and
  closes the connection.

Tokens are scoped to "I am peer X" — they don't carry per-room
permissions. Per-room permissions are a Postgres lookup.

## Permission model

**Role-based per-room** with four roles. (Could be extended later
without breaking the protocol — server just maps additional roles to
the existing `Tier` + `allows_submit` machinery.)

| Role        | Granted tier  | Read | Comment | Edit | Admin |
|-------------|---------------|------|---------|------|-------|
| `viewer`    | `Tier::Read`  | ✓    | ✗       | ✗    | ✗     |
| `commenter` | `Tier::Read`  | ✓    | ✓       | ✗    | ✗     |
| `editor`    | `Tier::ReadWrite` | ✓ | ✓       | ✓    | ✗     |
| `owner`     | `Tier::ReadWrite` | ✓ | ✓       | ✓    | ✓     |

Rows live in `room_members` (see [01_storage.md](01_storage.md)).
Admin actions (transfer ownership, delete room, change member roles)
go through HTTP endpoints, not the WS protocol — kept separate from
the realtime path.

## The flow

1. **Client gets a token** — out-of-band login flow against
   `kyoso_auth`. Stored in browser storage / OS keychain.
2. **Client opens WS** with `Authorization: Bearer <jwt>` header on
   the upgrade.
3. **Server verifies token** in the upgrade handler. Failure → 401
   close, no envelope traffic.
4. **Server stores `peer_identity = (user_id, kind)`** on the
   connection state (alongside `tier`, `peer_id`).
5. **Client sends `Hello { room, tier_requested, models }`**.
6. **Server consults `AuthService::authorize_join(user_id, room_id,
   tier_requested) → Tier`**:
   - Look up `room_members.role` for `(user_id, room_id)`.
   - Map role to max permitted tier per the table above.
   - Grant `min(tier_requested, max_permitted)`.
   - Return `Error { message }` if no membership exists at all.
7. **Server replies with `Welcome { tier_granted, ... }`** as today.
8. **Per-Submit policy** — the existing `allows_submit(tier, payload)`
   on the handler keeps working unchanged. It already gates
   per-tier; the role-tier mapping above keeps observers from
   submitting graph ops while letting commenters submit comments.

## Service accounts

Workers need identity but aren't human users. A `service_accounts`
row holds a name + a list of capabilities (e.g.
`['compute.image_segment', 'compute.thumbnail']`). The token claims
`kind: "service", service_account_id: <uuid>` and the auth check is
different:

- Service accounts are **not** members of any specific room.
- They get `Tier::ReadWrite` on every room (assumed trusted within
  the deployment).
- The per-handler `allows_submit` check sees the `peer_identity` and
  can recognize service accounts, gating which ops they may submit.
  E.g. graph handler accepts `SetNodeProperty` from a service account
  on `compute_*` paths but rejects writes to user-editable
  properties.

This keeps the trust model explicit: workers can ONLY write derived
state, never user state. A compromised worker can't impersonate a
user to delete their nodes.

## Trait + integration

```rust
// kyoso_authz crate (new)

#[async_trait]
pub trait AuthService: Send + Sync + 'static {
    /// Verify a JWT and return the peer's identity.
    async fn verify_token(&self, jwt: &str) -> Result<PeerIdentity>;

    /// Decide what tier this peer gets on this room. May downgrade
    /// the requested tier; rejects with PermissionDenied if no
    /// access at all.
    async fn authorize_join(
        &self,
        identity: &PeerIdentity,
        room_id: &RoomId,
        tier_requested: Tier,
    ) -> Result<Tier>;
}

pub enum PeerIdentity {
    User { user_id: Uuid },
    Service { service_account_id: Uuid, capabilities: Vec<String> },
}
```

Plus an updated handler trait:

```rust
// in kyoso_server::services::handler
pub trait RoomModelHandler {
    fn allows_submit(
        &self,
        tier: Tier,
        identity: &PeerIdentity,    // NEW
        payload: &[u8],
    ) -> bool { /* ... */ }
}
```

The graph handler's `allows_submit` then looks like:

```rust
fn allows_submit(&self, tier: Tier, identity: &PeerIdentity, payload: &[u8]) -> bool {
    // Service accounts can write compute-derived property updates
    // but nothing else.
    if let PeerIdentity::Service { capabilities, .. } = identity {
        let op: Op<OpKind> = match postcard::from_bytes(payload) {
            Ok(o) => o, Err(_) => return false,
        };
        return matches!(op.kind, OpKind::SetNodeProperty { ref path, .. }
            if path.0.first().map_or(false, |seg| matches!(seg, PathSegment::Field(s) if s.starts_with("compute_"))));
    }
    matches!(tier, Tier::ReadWrite)
}
```

## Implementation plan

**Slice 2.A** — `AuthService` trait + JWT verification + a
`PostgresAuthService` impl reading `room_members`. Stub impl in
test-only module that returns whatever the test wants.

**Slice 2.B** — wire `AuthService` into the WS upgrade handler.
Token verification before envelope traffic begins. `PeerIdentity`
threaded through to `Room::submit` and the handler's `allows_submit`.

**Slice 2.C** — service-account flow + capability checks in the
graph + comments handlers.

**Slice 2.D** — minimal `kyoso_auth` binary (signs tokens) + a CLI
to mint admin tokens for testing. Real OAuth integration is
follow-up.

**Slice 2.E** — admin HTTP endpoints (`POST /rooms`, `POST
/rooms/:id/members`, etc.) for room creation + member management.

Slices A-C are the meat; D-E unblock real-world usage.

## OPEN decisions

1. **Token lifetime + refresh strategy.** Short-lived access tokens
   (15 min?) + refresh tokens? Or longer-lived (24h) sessions?
   Long-lived simplifies the WS reconnect story (token doesn't expire
   mid-session); short-lived reduces blast radius of a leak. **Lean
   long-lived for v1**, revisit when we have a real product.
2. **JWKS endpoint vs static public key.** Static is simpler;
   JWKS handles key rotation. Static for v1.
3. **Authorization header on WS upgrade vs in-band Hello field?**
   Standard is `Authorization` header, but some clients (browsers)
   can't easily set headers on the WS handshake. Workaround: also
   accept `?token=` query param (less ideal — leaks to access logs).
   Or carry `token` as a field on Hello. **Lean: try header first,
   fall back to Hello field if browser support is bad.**
4. **Multi-tenant org boundaries.** Room members table doesn't
   currently express orgs. If we want "users can only see rooms in
   their org", add `organizations` + `org_members` tables.
5. **Mid-session permission change** (e.g. owner demotes a peer to
   viewer while connected). Server should kick the peer; they
   reconnect with the new tier. Polling for this is wasteful — could
   use Postgres `LISTEN/NOTIFY` to trigger room broadcast of "peer X
   has been removed". Defer.
