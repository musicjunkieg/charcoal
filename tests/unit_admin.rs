//! Tests for admin authorization middleware and impersonation.

#[cfg(feature = "web")]
mod admin_tests {
    use charcoal::web::auth::did_is_admin;
    use charcoal::web::AuthUser;

    #[test]
    fn test_did_is_admin_with_matching_did() {
        assert!(did_is_admin("did:plc:admin1", "did:plc:admin1"));
    }

    #[test]
    fn test_did_is_admin_with_comma_separated_list() {
        assert!(did_is_admin(
            "did:plc:admin2",
            "did:plc:admin1,did:plc:admin2"
        ));
    }

    #[test]
    fn test_did_is_admin_non_admin() {
        assert!(!did_is_admin("did:plc:user1", "did:plc:admin1"));
    }

    #[test]
    fn test_did_is_admin_empty_list() {
        assert!(!did_is_admin("did:plc:user1", ""));
    }

    #[test]
    fn test_auth_user_not_impersonating() {
        let auth = AuthUser {
            did: "did:plc:me".to_string(),
            effective_did: "did:plc:me".to_string(),
            is_admin: false,
        };
        assert!(!auth.is_impersonating());
    }

    #[test]
    fn test_auth_user_impersonating() {
        let auth = AuthUser {
            did: "did:plc:admin".to_string(),
            effective_did: "did:plc:other".to_string(),
            is_admin: true,
        };
        assert!(auth.is_impersonating());
    }
}
