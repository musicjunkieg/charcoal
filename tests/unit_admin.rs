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

#[cfg(feature = "web")]
mod impersonation_tests {
    use charcoal::web::auth::resolve_effective_did;

    #[test]
    fn test_no_as_user_returns_own_did() {
        let result = resolve_effective_did("did:plc:me", true, None);
        assert_eq!(result.unwrap(), "did:plc:me");
    }

    #[test]
    fn test_admin_with_as_user_returns_target() {
        let result = resolve_effective_did("did:plc:me", true, Some("did:plc:other"));
        assert_eq!(result.unwrap(), "did:plc:other");
    }

    #[test]
    fn test_non_admin_with_as_user_returns_error() {
        let result = resolve_effective_did("did:plc:me", false, Some("did:plc:other"));
        assert!(result.is_err());
    }

    #[test]
    fn test_non_admin_without_as_user_returns_own_did() {
        let result = resolve_effective_did("did:plc:me", false, None);
        assert_eq!(result.unwrap(), "did:plc:me");
    }
}

#[cfg(feature = "web")]
mod db_tests {
    use charcoal::db::sqlite::SqliteDatabase;
    use charcoal::db::Database;
    use rusqlite::Connection;

    async fn setup_db() -> SqliteDatabase {
        let conn = Connection::open_in_memory().unwrap();
        charcoal::db::schema::create_tables(&conn).unwrap();
        SqliteDatabase::new(conn)
    }

    #[tokio::test]
    async fn test_list_users_empty() {
        let db = setup_db().await;
        let users = db.list_users().await.unwrap();
        assert!(users.is_empty());
    }

    #[tokio::test]
    async fn test_list_users_after_upsert() {
        let db = setup_db().await;
        db.upsert_user("did:plc:abc", "alice.bsky.social")
            .await
            .unwrap();
        db.upsert_user("did:plc:def", "bob.bsky.social")
            .await
            .unwrap();
        let users = db.list_users().await.unwrap();
        assert_eq!(users.len(), 2);
    }

    #[tokio::test]
    async fn test_has_fingerprint_false() {
        let db = setup_db().await;
        db.upsert_user("did:plc:abc", "alice.bsky.social")
            .await
            .unwrap();
        assert!(!db.has_fingerprint("did:plc:abc").await.unwrap());
    }

    #[tokio::test]
    async fn test_has_fingerprint_true() {
        let db = setup_db().await;
        db.upsert_user("did:plc:abc", "alice.bsky.social")
            .await
            .unwrap();
        db.save_fingerprint("did:plc:abc", r#"{"topics":[]}"#, 10)
            .await
            .unwrap();
        assert!(db.has_fingerprint("did:plc:abc").await.unwrap());
    }

    #[tokio::test]
    async fn test_get_scored_account_count_zero() {
        let db = setup_db().await;
        let count = db.get_scored_account_count("did:plc:abc").await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_update_last_login() {
        let db = setup_db().await;
        db.upsert_user("did:plc:abc", "alice.bsky.social")
            .await
            .unwrap();
        db.update_last_login("did:plc:abc").await.unwrap();
        let users = db.list_users().await.unwrap();
        assert!(users[0].last_login_at.is_some());
    }

    #[tokio::test]
    async fn test_delete_user_data() {
        let db = setup_db().await;
        db.upsert_user("did:plc:abc", "alice.bsky.social")
            .await
            .unwrap();
        db.delete_user_data("did:plc:abc").await.unwrap();
        let users = db.list_users().await.unwrap();
        assert!(users.is_empty());
    }
}

#[cfg(feature = "web")]
mod scan_manager_tests {
    use charcoal::web::scan_job::ScanManager;

    #[test]
    fn test_scan_manager_starts_empty() {
        let mgr = ScanManager::new();
        assert!(!mgr.is_any_running());
    }

    #[test]
    fn test_scan_manager_try_start_succeeds() {
        let mut mgr = ScanManager::new();
        assert!(mgr.try_start_scan("did:plc:abc").is_ok());
        assert!(mgr.is_any_running());
    }

    #[test]
    fn test_scan_manager_try_start_rejects_second() {
        let mut mgr = ScanManager::new();
        mgr.try_start_scan("did:plc:abc").unwrap();
        assert!(mgr.try_start_scan("did:plc:def").is_err());
    }

    #[test]
    fn test_scan_manager_finish_allows_next() {
        let mut mgr = ScanManager::new();
        mgr.try_start_scan("did:plc:abc").unwrap();
        mgr.finish_scan("did:plc:abc");
        assert!(mgr.try_start_scan("did:plc:def").is_ok());
    }

    #[test]
    fn test_scan_manager_per_user_status() {
        let mut mgr = ScanManager::new();
        mgr.try_start_scan("did:plc:abc").unwrap();
        let status = mgr.get_status("did:plc:abc");
        assert!(status.is_some());
        assert!(status.unwrap().running);
        assert!(mgr.get_status("did:plc:other").is_none());
    }

    #[test]
    fn test_fingerprint_building_tracking() {
        let mut mgr = ScanManager::new();
        assert!(!mgr.is_fingerprint_building("did:plc:abc"));
        mgr.start_fingerprint_build("did:plc:abc");
        assert!(mgr.is_fingerprint_building("did:plc:abc"));
        mgr.finish_fingerprint_build("did:plc:abc");
        assert!(!mgr.is_fingerprint_building("did:plc:abc"));
    }
}
