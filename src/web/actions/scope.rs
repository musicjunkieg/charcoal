//! The one place the write-consent scope string exists (#315, spec §3.2).
//!
//! Granular AT Protocol scopes only. `transition:generic` is never requested
//! and never used as a fallback: least privilege is the whole point of
//! running a confidential client.

/// The Bluesky AppView service DID. This is both the `atproto-proxy` header
/// value on every `app.bsky.*` call (`pds.rs`) and the audience every `rpc:`
/// scope below is granted for — the PDS checks that the two agree on each
/// proxied request, so they are derived from one constant on purpose (#322).
pub const APPVIEW_DID: &str = "did:web:api.bsky.app#bsky_appview";

/// `APPVIEW_DID` with `#` percent-encoded, as the scope-string syntax needs.
const APPVIEW_AUD: &str = "did:web:api.bsky.app%23bsky_appview";

/// Scope requested on the write-consent round-trip: create/delete on the
/// user's own block records, plus the four `app.bsky.graph.*` RPCs proxied to
/// the AppView. The two reads (`getMutes`/`getBlocks`) are what the runner
/// reconciles against before writing; the PDS checks proxied *reads* against
/// the `rpc:` grant just as it does writes (#322).
///
/// Spike note (spec §3.2): if a live Bluesky PDS answers `invalid_scope` to
/// this exact string, the first thing to try is `aud=*` in place of the
/// AppView DID. Change it HERE only and record the outcome in the spec.
pub fn write_scope() -> String {
    format!(
        "atproto repo:app.bsky.graph.block?action=create&action=delete \
         rpc:app.bsky.graph.muteActor?aud={APPVIEW_AUD} \
         rpc:app.bsky.graph.unmuteActor?aud={APPVIEW_AUD} \
         rpc:app.bsky.graph.getMutes?aud={APPVIEW_AUD} \
         rpc:app.bsky.graph.getBlocks?aud={APPVIEW_AUD}"
    )
}

/// Client metadata must advertise the union of every scope the client will
/// ever request; login uses the `atproto` prefix on its own.
pub fn client_scope() -> String {
    write_scope()
}

/// Did the authorization server grant what we asked for? Servers may reorder
/// or normalise the scope string. The RPC scopes carry only an `aud`
/// parameter, so a prefix check is still safe for those. The block repo
/// scope is different: a naive prefix check on `repo:app.bsky.graph.block`
/// would also accept a *downgraded* grant such as
/// `repo:app.bsky.graph.block?action=create` (create only, no delete) — the
/// first undo would then fail at the PDS with an authorization error instead
/// of failing here, up front. So the block scope is parsed and its actions
/// inspected directly, requiring BOTH `create` and `delete`.
///
/// Reading of a bare `repo:app.bsky.graph.block` (no `?action=` query at
/// all): the vendored parser (`atproto_oauth::scopes::Scope::parse_repo`)
/// treats an absent `action` param as "all actions" (create+update+delete),
/// matching the AT Protocol scope spec's documented behavior that an
/// unqualified repo scope is the maximal grant for that collection. That is
/// a superset of create+delete, so it satisfies this check.
pub fn scope_grants_write(granted: &str) -> bool {
    use atproto_oauth::scopes::{RepoAction, RepoCollection, Scope};

    let has = |prefix: &str| granted.split_whitespace().any(|s| s.starts_with(prefix));

    let Ok(scopes) = Scope::parse_multiple(granted) else {
        return false;
    };

    let block_scope_has_create_and_delete = scopes.iter().any(|scope| match scope {
        Scope::Repo(repo) => {
            let is_block_collection = match &repo.collection {
                RepoCollection::Nsid(nsid) => nsid == "app.bsky.graph.block",
                RepoCollection::All => true,
            };
            is_block_collection
                && repo.actions.contains(&RepoAction::Create)
                && repo.actions.contains(&RepoAction::Delete)
        }
        _ => false,
    });

    // Also the gate that retires sessions stored under an older
    // `write_scope()`: `SessionStore` reads a row that fails this check as
    // "not connected", so the person consents again once (#322).
    block_scope_has_create_and_delete
        && has("rpc:app.bsky.graph.muteActor")
        && has("rpc:app.bsky.graph.unmuteActor")
        && has("rpc:app.bsky.graph.getMutes")
        && has("rpc:app.bsky.graph.getBlocks")
}

#[cfg(test)]
mod tests {
    use super::*;
    use atproto_oauth::scopes::Scope;

    /// Every RPC grant the PDS will check, with a placeholder audience.
    const ALL_RPC: &str = "rpc:app.bsky.graph.muteActor?aud=x \
         rpc:app.bsky.graph.unmuteActor?aud=x \
         rpc:app.bsky.graph.getMutes?aud=x \
         rpc:app.bsky.graph.getBlocks?aud=x";

    #[test]
    fn write_scope_parses_as_granular_scopes() {
        let scopes = Scope::parse_multiple(&write_scope()).expect("scope string must parse");
        // atproto + block repo + mute/unmute rpc + getMutes/getBlocks rpc
        assert_eq!(scopes.len(), 6);
        let s = write_scope();
        assert!(s.starts_with("atproto "));
        assert!(s.contains("repo:app.bsky.graph.block?action=create&action=delete"));
        assert!(s.contains("rpc:app.bsky.graph.muteActor?aud="));
        assert!(s.contains("rpc:app.bsky.graph.unmuteActor?aud="));
        assert!(s.contains("rpc:app.bsky.graph.getMutes?aud="));
        assert!(s.contains("rpc:app.bsky.graph.getBlocks?aud="));
        assert!(!s.contains("transition:generic"));
    }

    #[test]
    fn client_scope_is_union_of_login_and_write() {
        let c = client_scope();
        assert!(c.starts_with("atproto "));
        for part in write_scope().split(' ') {
            assert!(c.contains(part), "client scope missing {part}");
        }
        Scope::parse_multiple(&c).expect("client scope must parse");
    }

    #[test]
    fn scope_grants_write_accepts_the_full_grant_in_any_order() {
        assert!(scope_grants_write(&write_scope()));
        let scope = write_scope();
        let reordered: Vec<&str> = scope.split(' ').rev().collect::<Vec<_>>();
        assert!(scope_grants_write(&reordered.join(" ")));
    }

    #[test]
    fn scope_grants_write_rejects_partial_grants() {
        assert!(!scope_grants_write("atproto"));
        assert!(!scope_grants_write(
            "atproto repo:app.bsky.graph.block?action=create&action=delete"
        ));
        assert!(!scope_grants_write(&format!("atproto {ALL_RPC}")));
        assert!(!scope_grants_write("transition:generic"));
    }

    /// #315 review, finding 1: the exact `write_scope()` string must pass.
    #[test]
    fn scope_grants_write_accepts_the_exact_write_scope_string() {
        assert!(scope_grants_write(&write_scope()));
    }

    /// Token order within the space-separated scope string must not matter —
    /// servers may reorder or renormalize.
    #[test]
    fn scope_grants_write_accepts_reordered_tokens() {
        let scope = write_scope();
        let reordered: Vec<&str> = scope.split(' ').rev().collect();
        assert!(scope_grants_write(&reordered.join(" ")));
    }

    /// The regression this task fixes: a create-only block grant must NOT
    /// pass, or the first undo (a delete) would fail at the PDS instead of
    /// here.
    #[test]
    fn scope_grants_write_rejects_create_only_block_scope() {
        assert!(!scope_grants_write(&format!(
            "atproto repo:app.bsky.graph.block?action=create {ALL_RPC}"
        )));
    }

    /// The mirror case: delete-only, no create, must also fail.
    #[test]
    fn scope_grants_write_rejects_delete_only_block_scope() {
        assert!(!scope_grants_write(&format!(
            "atproto repo:app.bsky.graph.block?action=delete {ALL_RPC}"
        )));
    }

    /// A full block grant with any one RPC missing must still fail — the PDS
    /// checks every proxied call (reads included, #322) against the grant.
    #[test]
    fn scope_grants_write_rejects_any_missing_rpc() {
        let block = "repo:app.bsky.graph.block?action=create&action=delete";
        for missing in [
            "rpc:app.bsky.graph.muteActor",
            "rpc:app.bsky.graph.unmuteActor",
            "rpc:app.bsky.graph.getMutes",
            "rpc:app.bsky.graph.getBlocks",
        ] {
            let rest: Vec<&str> = ALL_RPC
                .split_whitespace()
                .filter(|s| !s.starts_with(missing))
                .collect();
            let granted = format!("atproto {block} {}", rest.join(" "));
            assert!(
                !scope_grants_write(&granted),
                "should reject without {missing}"
            );
        }
    }

    /// The write scope shipped in #315 (mute/unmute only) must now read as an
    /// insufficient grant, so sessions stored under it are re-consented once
    /// instead of failing every batch at the reconcile read (#322).
    #[test]
    fn scope_grants_write_rejects_the_pre_322_grant() {
        assert!(!scope_grants_write(
            "atproto repo:app.bsky.graph.block?action=create&action=delete \
             rpc:app.bsky.graph.muteActor?aud=did:web:api.bsky.app%23bsky_appview \
             rpc:app.bsky.graph.unmuteActor?aud=did:web:api.bsky.app%23bsky_appview"
        ));
    }

    /// The `atproto-proxy` header value and the `aud` in every `rpc:` scope
    /// must name the same service, or the PDS's `assertRpc` check fails even
    /// though consent succeeded. `#` is literal in the header and
    /// percent-encoded in the scope string.
    #[test]
    fn appview_did_and_scope_aud_name_the_same_service() {
        assert_eq!(APPVIEW_DID, "did:web:api.bsky.app#bsky_appview");
        let encoded = APPVIEW_DID.replace('#', "%23");
        for part in write_scope()
            .split_whitespace()
            .filter(|s| s.starts_with("rpc:"))
        {
            assert!(part.ends_with(&format!("?aud={encoded}")), "{part}");
        }
    }

    /// `transition:generic` is never an acceptable substitute for the
    /// granular scopes, even alongside `atproto`.
    #[test]
    fn scope_grants_write_rejects_transition_generic() {
        assert!(!scope_grants_write("atproto transition:generic"));
    }

    /// A bare `repo:app.bsky.graph.block` (no `?action=` query at all) is
    /// read as the maximal grant for that collection per the vendored
    /// parser's documented behavior (see the comment on
    /// `scope_grants_write`), so it satisfies the create+delete requirement.
    #[test]
    fn scope_grants_write_accepts_bare_block_scope_as_full_grant() {
        assert!(scope_grants_write(&format!(
            "atproto repo:app.bsky.graph.block {ALL_RPC}"
        )));
    }
}
