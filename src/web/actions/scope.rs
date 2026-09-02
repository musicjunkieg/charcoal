//! The one place the write-consent scope string exists (#315, spec §3.2).
//!
//! Granular AT Protocol scopes only. `transition:generic` is never requested
//! and never used as a fallback: least privilege is the whole point of
//! running a confidential client.

/// The Bluesky AppView service DID, URL-encoded for the `aud` parameter.
/// `#` must be percent-encoded inside a scope string.
const APPVIEW_AUD: &str = "did:web:api.bsky.app%23bsky_appview";

/// Scope requested on the write-consent round-trip: create/delete on the
/// user's own block records, plus the mute/unmute RPCs proxied to the AppView.
///
/// Spike note (spec §3.2): if a live Bluesky PDS answers `invalid_scope` to
/// this exact string, the first thing to try is `aud=*` in place of the
/// AppView DID. Change it HERE only and record the outcome in the spec.
pub fn write_scope() -> String {
    format!(
        "atproto repo:app.bsky.graph.block?action=create&action=delete \
         rpc:app.bsky.graph.muteActor?aud={APPVIEW_AUD} \
         rpc:app.bsky.graph.unmuteActor?aud={APPVIEW_AUD}"
    )
}

/// Client metadata must advertise the union of every scope the client will
/// ever request; login uses the `atproto` prefix on its own.
pub fn client_scope() -> String {
    write_scope()
}

/// Did the authorization server grant what we asked for? Servers may reorder
/// or normalise the scope string. The mute/unmute RPC scopes carry only an
/// `aud` parameter, so a prefix check is still safe for those. The block repo
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

    block_scope_has_create_and_delete
        && has("rpc:app.bsky.graph.muteActor")
        && has("rpc:app.bsky.graph.unmuteActor")
}

#[cfg(test)]
mod tests {
    use super::*;
    use atproto_oauth::scopes::Scope;

    #[test]
    fn write_scope_parses_as_granular_scopes() {
        let scopes = Scope::parse_multiple(&write_scope()).expect("scope string must parse");
        // atproto + block repo + mute rpc + unmute rpc
        assert_eq!(scopes.len(), 4);
        let s = write_scope();
        assert!(s.starts_with("atproto "));
        assert!(s.contains("repo:app.bsky.graph.block?action=create&action=delete"));
        assert!(s.contains("rpc:app.bsky.graph.muteActor?aud="));
        assert!(s.contains("rpc:app.bsky.graph.unmuteActor?aud="));
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
        assert!(!scope_grants_write(
            "atproto rpc:app.bsky.graph.muteActor?aud=x rpc:app.bsky.graph.unmuteActor?aud=x"
        ));
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
        assert!(!scope_grants_write(
            "atproto repo:app.bsky.graph.block?action=create \
             rpc:app.bsky.graph.muteActor?aud=x rpc:app.bsky.graph.unmuteActor?aud=x"
        ));
    }

    /// The mirror case: delete-only, no create, must also fail.
    #[test]
    fn scope_grants_write_rejects_delete_only_block_scope() {
        assert!(!scope_grants_write(
            "atproto repo:app.bsky.graph.block?action=delete \
             rpc:app.bsky.graph.muteActor?aud=x rpc:app.bsky.graph.unmuteActor?aud=x"
        ));
    }

    /// A full block grant with the unmute RPC missing must still fail — all
    /// three resources are required.
    #[test]
    fn scope_grants_write_rejects_missing_unmute() {
        assert!(!scope_grants_write(
            "atproto repo:app.bsky.graph.block?action=create&action=delete \
             rpc:app.bsky.graph.muteActor?aud=x"
        ));
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
        assert!(scope_grants_write(
            "atproto repo:app.bsky.graph.block \
             rpc:app.bsky.graph.muteActor?aud=x rpc:app.bsky.graph.unmuteActor?aud=x"
        ));
    }
}
