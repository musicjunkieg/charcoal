# The self-protective invariant (#261)

> Charcoal's outputs act only on the user's own experience and reach.

Charcoal predicts which accounts are likely to engage with a user in bad
faith. That prediction names real people. The invariant limits what Charcoal
is allowed to *do* with a prediction, so that being wrong costs the user a
little friction and costs the named person nothing.

## What the invariant permits

- **Muting.** A mute is private to the user. Nobody else can see it, and the
  muted account's experience does not change.
- **Blocking.** A block is a record in the user's own repository — the same
  `app.bsky.graph.block` record the Bluesky app writes when the user taps
  Block. It is public, but it is scoped to the relationship between the user
  and one account. Charcoal writes exactly that record and nothing more.
- **Undoing either.** Every mute or block Charcoal applied can be reversed
  from inside Charcoal, and Charcoal only ever deletes records it created.

## What the invariant forbids

- Charcoal never attaches a reason, tier, score, label, or its own name to
  anything it writes to the network. A block created by Charcoal is
  indistinguishable from one created by hand.
- Charcoal never creates a moderation list, a labeler label, an export, or
  any other shareable artefact from its results. Predictions stay inside the
  user's dashboard.
- Charcoal never acts with one user's credentials on another user's behalf.
  Admin impersonation is read-only: an admin can look at another user's
  dashboard and action log, and can never mute, block, connect, or
  disconnect as them.
- Charcoal never re-creates or deletes a block or mute the user set up
  themselves. Reconciliation before every batch marks those as already in
  place and leaves them alone.

## Why this shape

The primary harassment vector on Bluesky is amplification to an audience
that did not opt in. A prediction that leaked outward — as a list, a label, or
a public reason — would itself be an amplification event aimed at the named
account, and Charcoal would become the thing it exists to defend against.
Keeping every output inside the user's own reach means a false positive is
a private mistake the user can reverse in one tap.

## Where it is enforced

- `src/web/actions/pds.rs` — the only code that writes to a PDS. It builds
  `app.bsky.graph.block` records with `subject` + `createdAt` only, and
  deletes only by a stored record URI.
- `src/web/handlers/actions.rs` — every write endpoint refuses impersonated
  sessions with `403 impersonation_forbidden`.
- `src/web/actions/runner.rs` — reconciliation runs before every batch.

Related: `PRODUCT.md` principle 3 (automation stays reversible).
