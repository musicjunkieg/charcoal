# AT Protocol OAuth Design — Charcoal v0.4

**Date:** 2026-03-08
**Status:** Approved
**Milestone:** v0.4 — Replace password auth with AT Protocol OAuth

---

## Problem

The current auth mechanism (HMAC session cookie signed against a hardcoded
`CHARCOAL_WEB_PASSWORD`) has no identity: anyone with the password is
"authenticated." This is acceptable for a local tool but unacceptable for a
deployed service. Moving to muting/blocking (planned post-multi-user) requires
the backend to hold valid AT Protocol tokens to make authenticated writes. We
need real identity from day one.

---

## Decision

**Backend-driven OAuth (Option B)** using the `atproto-oauth-axum` crate from
[ngerakines.me/atproto-crates](https://tangled.org/ngerakines.me/atproto-crates).
Endorsed by the Bluesky team on their official SDKs page.

Option A (browser-side OAuth via `@atproto/oauth-client-browser`) was
considered and rejected because:

1. The muting/blocking roadmap requires the backend to hold AT Protocol tokens
   for authenticated writes. Option A stores tokens in the browser; Option B
   stores them server-side where they can be used for future XRPC calls.
2. `atproto-oauth-axum` provides production-ready Axum handlers with DPoP
   and PKCE support, eliminating the maturity concern that would otherwise
   favor the TypeScript SDK.

---

## Scope

This milestone is **single-user only**. Only `CHARCOAL_ALLOWED_DID` may log
in; other DIDs receive a 403. The session cookie carries the authenticated DID
so no rework is needed when multi-user arrives — that milestone only adds DB
schema changes and removes the DID gate.

Token storage is **in-memory** for this milestone (`RwLock<Option<TokenSet>>`
in `AppState`). Tokens are lost on server restart; the user re-authenticates.
This is acceptable for single-user. The multi-user milestone moves tokens to
the database.

---

## OAuth Flow

```
Browser                   Axum                        User's PDS
  |                         |                              |
  | POST /api/auth/initiate |                              |
  | { handle: "user.bsky.social" }                        |
  |-----------------------> |                              |
  |                         | resolve handle → DID         |
  |                         | discover PDS auth server     |
  |                         | generate PAR request    ---> |
  |                         | <-- request_uri              |
  | <-- { redirect_url }    |                              |
  | [browser follows redirect to PDS]                      |
  |                                                        |
  | [user authenticates on PDS, PDS issues code]           |
  |                                                        |
  | GET /api/auth/callback?code=...&state=...              |
  |-----------------------> |                              |
  |                         | exchange code for tokens     |
  |                         | (PKCE + DPoP via SDK)        |
  |                         | extract DID from token sub   |
  |                         | verify DID == CHARCOAL_ALLOWED_DID
  |                         | store TokenSet in AppState   |
  |                         | issue HMAC session cookie    |
  |                         |   payload: { did, exp }      |
  | <-- 302 /dashboard      |                              |
  |   Set-Cookie: charcoal_session=...                     |
```

---

## New Endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/api/auth/initiate` | Accept `{ handle }`, return `{ redirect_url }` |
| `GET` | `/api/auth/callback` | PDS redirects here; exchange code, set cookie |
| `GET` | `/oauth-client-metadata.json` | Serve client metadata document |

**Removed:** `POST /api/login` (password auth deleted entirely)

`POST /api/logout` is kept unchanged — it clears the session cookie.

---

## Client Metadata

The AT Protocol OAuth spec requires `client_id` to be a public HTTPS URL
serving a client metadata JSON document.

**Development:** Use `https://cimd-service.fly.dev/` — POST the client
metadata document once, receive a permanent dev `client_id` URL. The
client name will display as `Charcoal (Development)` in the PDS auth UI.

**Production (Railway):** Axum serves `GET /oauth-client-metadata.json`
directly. The `client_id` is `https://{RAILWAY_PUBLIC_DOMAIN}/oauth-client-metadata.json`.

Client metadata document structure:
```json
{
  "client_id": "https://{host}/oauth-client-metadata.json",
  "client_name": "Charcoal",
  "client_uri": "https://{host}",
  "redirect_uris": ["https://{host}/api/auth/callback"],
  "scope": "atproto",
  "grant_types": ["authorization_code", "refresh_token"],
  "response_types": ["code"],
  "token_endpoint_auth_method": "none",
  "application_type": "web",
  "dpop_bound_access_tokens": true
}
```

---

## Environment Variables

| Variable | Required | Purpose |
|----------|----------|---------|
| `CHARCOAL_ALLOWED_DID` | Yes | Only this DID may authenticate |
| `CHARCOAL_OAUTH_CLIENT_ID` | Yes | Public URL of client metadata document |
| `CHARCOAL_SESSION_SECRET` | Yes (unchanged) | HMAC signing key for session cookies |
| `CHARCOAL_WEB_PASSWORD` | **Removed** | Deleted — no longer used |

---

## Session Cookie Changes

The session cookie HMAC payload changes from a bare timestamp+nonce to
include the authenticated DID:

```
{timestamp}.{did_b64}.{nonce}.{hmac}
```

Where `did_b64` is the URL-safe base64 of the DID string. The middleware
extracts and validates this; handlers can read the DID from request extensions
via the `AuthUser` struct (which gains a `did: String` field).

This makes the session multi-user-capable without rework when the gate is removed.

---

## New Cargo Dependencies

```toml
[dependencies]
atproto-oauth     = { git = "https://tangled.org/ngerakines.me/atproto-crates" }
atproto-oauth-axum = { git = "https://tangled.org/ngerakines.me/atproto-crates" }
atproto-identity  = { git = "https://tangled.org/ngerakines.me/atproto-crates" }
```

`atproto-client` is deferred to the muting/blocking milestone.

---

## SvelteKit Login Page Changes

The password form is replaced with a handle input. The existing visual design
(animated rings logo, dark charcoal theme, copper accents) is preserved.

**Before:** `<input type="password">` + "Continue" button
**After:** `<input type="text" placeholder="yourhandle.bsky.social">` +
"Sign in with Bluesky" button

The button POSTs `{ handle }` to `/api/auth/initiate`, receives
`{ redirect_url }`, and sets `window.location.href` to follow the PDS redirect.

The callback is handled entirely server-side; the browser is redirected to
`/dashboard` with the session cookie already set.

---

## Testing Strategy (BDD)

Tests are written **before** implementation. Each scenario describes
observable behavior from the outside.

### Rust unit tests (`tests/unit_oauth.rs`)

- `token_with_did_roundtrip` — create_token(did) → verify_token() returns did
- `wrong_secret_rejected` — tampered HMAC fails
- `wrong_did_in_token_rejected` — mismatched DID fails gate check
- `future_dated_token_rejected` — checked_sub prevents bypass
- `disallowed_did_rejected` — DID != CHARCOAL_ALLOWED_DID returns false

### Rust integration tests (`tests/web_oauth.rs`)

Using `axum::test` (or `tower::ServiceExt`):

- `GET /oauth-client-metadata.json` returns valid JSON with correct fields
- `POST /api/auth/initiate` with valid handle returns `{ redirect_url }`
- `POST /api/auth/initiate` with empty handle returns 400
- `GET /api/auth/callback` with invalid state returns 400
- Protected routes return 401 with no session cookie
- Protected routes return 403 when session DID != allowed DID
- `POST /api/logout` clears the session cookie

### SvelteKit component tests

- Login page renders handle input (not password input)
- Submit with empty handle is disabled
- Submit calls `/api/auth/initiate` and follows redirect_url
- Error from initiate is displayed to user

---

## Files Changed

**Rust:**
- `Cargo.toml` — add 3 new dependencies
- `src/web/auth.rs` — update session token to include DID
- `src/web/handlers/auth.rs` — replace login handler with initiate + callback
- `src/web/mod.rs` — register new routes, add token storage to AppState
- `src/config.rs` — add CHARCOAL_ALLOWED_DID, CHARCOAL_OAUTH_CLIENT_ID; remove CHARCOAL_WEB_PASSWORD
- `tests/unit_oauth.rs` — new test file
- `tests/web_oauth.rs` — new integration test file

**SvelteKit:**
- `web/src/routes/login/+page.svelte` — replace password form with handle input
- `web/src/lib/api.ts` — replace `login(password)` with `initiateAuth(handle)`

**Docs/Config:**
- `railway.toml` — document new env vars
- `README.md` — update auth setup instructions
- `CLAUDE.md` — update current status

---

## Out of Scope for This Milestone

- Multi-user data isolation (separate milestone)
- Token refresh on expiry (acceptable to force re-auth for now)
- `atproto-client` for authenticated writes (muting/blocking milestone)
- AT Protocol event streams
