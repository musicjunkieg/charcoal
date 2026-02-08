// Database layer — SQLite storage for cached scores, scan state, and fingerprints.
//
// We use rusqlite with the "bundled" feature so there's no system SQLite
// dependency. The database file lives wherever CHARCOAL_DB_PATH points
// (defaults to ./charcoal.db).

pub mod schema;
pub mod models;
pub mod queries;

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

/// Open (or create) the database and run migrations.
///
/// This is the main entry point — called by `charcoal init` and by any
/// command that needs database access.
pub fn initialize(db_path: &str) -> Result<Connection> {
    // Create parent directories if needed
    if let Some(parent) = Path::new(db_path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory for database: {}", db_path))?;
        }
    }

    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path))?;

    // Enable WAL mode for better concurrent read performance
    conn.pragma_update(None, "journal_mode", "WAL")?;

    // Run schema creation / migrations
    schema::create_tables(&conn)?;

    Ok(conn)
}

/// Open an existing database (fails if it doesn't exist yet).
pub fn open(db_path: &str) -> Result<Connection> {
    if !Path::new(db_path).exists() {
        anyhow::bail!(
            "Database not found at {}. Run `charcoal init` first.",
            db_path
        );
    }

    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path))?;

    conn.pragma_update(None, "journal_mode", "WAL")?;

    Ok(conn)
}
