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
