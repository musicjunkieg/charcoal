# Railway Deployment Design

**Date:** 2026-03-18
**Issues:** #112 (ONNX model strategy), #115 (deploy strategy), #111 (DID gate)
**Status:** Approved

## Problem

Charcoal runs locally but has no production deployment. Three blockers:
1. ONNX models (~216MB) need to be available on the server
2. No Railway project or deploy pipeline exists
3. The single-user DID gate prevents other users from signing in

## Solution

### Infrastructure

- **Single Railway service** running the Axum web server (Nixpacks build via
  existing `railway.toml`)
- **Railway Postgres addon** — connection string auto-set via `DATABASE_URL`.
  Railway Postgres supports extensions — our migrations already run
  `CREATE EXTENSION IF NOT EXISTS vector` for pgvector.
- **Railway persistent volume** mounted at `/data/models` for ONNX models
- **Custom domain:** `charcoal.watch` (owned on Porkbun)

### Model Download Strategy

On server startup (in the `serve` command), before binding the port:
1. Check `config.model_dir` (from `CHARCOAL_MODEL_DIR` env var) for model files
2. If missing, download automatically from HuggingFace (reuse existing
   `toxicity::download::download_model(&config.model_dir)` function)
3. Once models are ready, bind the port and start accepting requests

Since models download before the port binds, Railway won't see the service
until it's ready. First deploy takes ~60s for download. Subsequent deploys
reuse the persistent volume — zero cold start.

If the persistent volume is lost (service deletion/recreation), the next
deploy triggers a fresh ~60s download automatically.

### Health Check Endpoint

`GET /health` already exists in the Axum router and returns `200 OK` with
`{"status": "ok"}`. Since models download before the port binds, the health
endpoint always returns 200 by the time Railway can reach it. No changes
needed to the health endpoint or `railway.toml` health check config.

### DID Gate: Optional Allowlist

Make `CHARCOAL_ALLOWED_DID` optional. Three enforcement points need changes:

1. **`run_server` guard in `src/web/mod.rs`** — currently calls `anyhow::bail!`
   when `allowed_did` is empty. Remove this guard so the server starts without
   a DID configured.

2. **`did_is_allowed()` in `src/web/auth.rs`** — currently returns `false`
   when `allowed_did` is empty (short-circuit on `!allowed_did.is_empty()`).
   Change to: if `allowed_did` is empty, return `true` (open access). If
   non-empty, split on commas and check if the DID matches any entry (supports
   comma-separated allowlist).

3. **OAuth callback in `src/web/handlers/oauth.rs`** — calls `did_is_allowed()`
   at line 434. The fix to `did_is_allowed()` covers this transitively, but
   verify during implementation.

**Behavior summary:**
- `CHARCOAL_ALLOWED_DID` unset or empty → all Bluesky users can sign in
- `CHARCOAL_ALLOWED_DID=did:plc:abc` → single user
- `CHARCOAL_ALLOWED_DID=did:plc:abc,did:plc:def` → allowlist

Railway deploy sets no `CHARCOAL_ALLOWED_DID` = open to all.
Local dev keeps the env var for single-user convenience.

### Custom Domain and OAuth

**Domain:** `charcoal.watch` pointed at Railway via CNAME (or ALIAS for apex).

**OAuth client ID:** `https://charcoal.watch/oauth-client-metadata.json`

DNS configured in Porkbun after Railway provides the CNAME target. Railway
auto-provisions SSL via Let's Encrypt.

### Environment Variables on Railway

| Variable | Value | Source |
|---|---|---|
| `DATABASE_URL` | Auto-set by Postgres addon | Railway |
| `CHARCOAL_MODEL_DIR` | `/data/models` | Manual |
| `CHARCOAL_OAUTH_CLIENT_ID` | `https://charcoal.watch/oauth-client-metadata.json` | Manual |
| `CHARCOAL_SESSION_SECRET` | Generated with `openssl rand -base64 32` | Manual |
| `RUST_LOG` | `charcoal=info` | Manual |
| `PORT` | Auto-set | Railway |

**Not needed on Railway:**
- `CHARCOAL_ALLOWED_DID` — open to all
- `BLUESKY_HANDLE` — multi-user, handle comes from OAuth
- `BLUESKY_APP_PASSWORD` — not used
- `CHARCOAL_DB_PATH` — using Postgres
- Tigris vars — Railway has its own Postgres backup

### Code Changes

Three changes needed:

1. **Auto-download models on startup** — in the `serve` command, before
   binding the port, call `download_model(&config.model_dir)` if models are
   missing. Pass `config.model_dir` (not `default_model_dir()`).

2. **Make DID gate optional** — three changes:
   - Remove the `bail!` guard in `run_server` (`src/web/mod.rs`)
   - Change `did_is_allowed()` (`src/web/auth.rs`) to return `true` when
     `allowed_did` is empty, and support comma-separated DIDs when non-empty
   - Verify the OAuth callback (`src/web/handlers/oauth.rs`) works correctly
     with the updated `did_is_allowed()`

3. **No health endpoint changes** — `/health` already exists and returns 200.
   Since models download before port bind, it's always ready when Railway
   polls it.

No changes to `railway.toml`.

### Deploy Sequence

1. Code changes (auto-download, optional DID gate) — feature branch, PR, merge
2. Create Railway project
3. Add Postgres addon
4. Add persistent volume mounted at `/data/models`
5. Set env vars
6. Deploy — connect GitHub repo, auto-builds on push to main
7. Wait for first boot — models download (~60s), then port binds, health check
   goes green
8. Add custom domain — Railway provides CNAME target, configure DNS in Porkbun
9. Verify OAuth — visit `https://charcoal.watch`, sign in, trigger a scan

## Known Limitations

- **Service recreation** triggers a fresh ~60s model download (persistent
  volume is lost)
- **Users with no Bluesky posts** will see "No posts found" when they try
  their first scan — the auto-fingerprint needs posting history to work
- **Single instance** — no horizontal scaling or job queue. Fine for alpha.

## Out of Scope

- Horizontal scaling / job queue (single instance is fine for alpha)
- Dockerfile (Nixpacks works, switch later if build times are painful)
- Rate limiting (add when needed)
- Tigris backup for Postgres (Railway handles Postgres backups)
