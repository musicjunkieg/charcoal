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
/// or normalise the scope string, so this checks for the three resources by
/// prefix rather than comparing the whole string.
pub fn scope_grants_write(granted: &str) -> bool {
    let has = |prefix: &str| granted.split_whitespace().any(|s| s.starts_with(prefix));
    has("repo:app.bsky.graph.block")
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
}
