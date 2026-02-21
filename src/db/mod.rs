// Database layer — pluggable backend for cached scores, scan state, and fingerprints.
//
// SQLite is the default backend (enabled by the `sqlite` feature). PostgreSQL
// is available via the `postgres` feature + DATABASE_URL env var.
//
// The database file lives wherever CHARCOAL_DB_PATH points (defaults to
// ./charcoal.db) for SQLite. PostgreSQL uses DATABASE_URL.

pub mod models;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "sqlite")]
pub mod queries;
#[cfg(feature = "sqlite")]
pub mod schema;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod traits;

pub use traits::Database;

use anyhow::Result;
use std::sync::Arc;

#[cfg(feature = "sqlite")]
use anyhow::Context;
#[cfg(feature = "sqlite")]
use rusqlite::Connection;
#[cfg(feature = "sqlite")]
use std::path::Path;

/// Open (or create) the SQLite database and run migrations.
///
/// This is the main entry point — called by `charcoal init` and by any
/// command that needs database access.
#[cfg(feature = "sqlite")]
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

/// Open an existing SQLite database (fails if it doesn't exist yet).
///
/// Also runs any pending migrations so schema changes apply automatically
/// without requiring `charcoal init` again.
#[cfg(feature = "sqlite")]
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

    // Run pending migrations (idempotent — skips already-applied ones)
    schema::create_tables(&conn)?;

    Ok(conn)
}

/// Open SQLite database and return it as a trait object.
#[cfg(feature = "sqlite")]
pub fn open_sqlite(db_path: &str) -> Result<Arc<dyn Database>> {
    let conn = open(db_path)?;
    Ok(Arc::new(sqlite::SqliteDatabase::new(conn)))
}

/// Initialize SQLite database and return it as a trait object.
#[cfg(feature = "sqlite")]
pub fn initialize_sqlite(db_path: &str) -> Result<Arc<dyn Database>> {
    let conn = initialize(db_path)?;
    Ok(Arc::new(sqlite::SqliteDatabase::new(conn)))
}

/// Connect to PostgreSQL and return it as a trait object.
#[cfg(feature = "postgres")]
pub async fn connect_postgres(database_url: &str) -> Result<Arc<dyn Database>> {
    let db = postgres::PgDatabase::connect(database_url).await?;
    Ok(Arc::new(db))
}
