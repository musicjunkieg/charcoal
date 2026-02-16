// System status display — shows DB stats, fingerprint age, last scan time.

use anyhow::Result;
use std::path::Path;

use crate::db;

/// Display system status to the terminal.
pub fn show(config: &impl HasDbPath) -> Result<()> {
    let db_path = config.db_path();

    if !Path::new(db_path).exists() {
        println!("Database: not initialized");
        println!("\nRun `charcoal init` to set up the database.");
        return Ok(());
    }

    let conn = db::open(db_path)?;

    // Database file size
    let file_size = std::fs::metadata(db_path)
        .map(|m| format_bytes(m.len()))
        .unwrap_or_else(|_| "unknown".to_string());
    println!("Database: {} ({})", db_path, file_size);

    // Fingerprint status
    match db::queries::get_fingerprint(&conn)? {
        Some((_json, post_count, updated_at)) => {
            println!(
                "Fingerprint: built from {} posts (updated {})",
                post_count, updated_at
            );
        }
        None => {
            println!("Fingerprint: not yet built");
            println!("  Run `charcoal fingerprint` to build it");
        }
    }

    // Scored accounts (Elevated tier starts at 15.0)
    let all_scores = db::queries::get_ranked_threats(&conn, 0.0)?;
    let elevated_count = all_scores
        .iter()
        .filter(|s| s.threat_score.is_some_and(|t| t >= 15.0))
        .count();
    println!(
        "Scored accounts: {} total, {} elevated+",
        all_scores.len(),
        elevated_count
    );

    // Recent events
    let events = db::queries::get_recent_events(&conn, 5)?;
    if events.is_empty() {
        println!("Recent events: none detected yet");
        println!("  Run `charcoal scan` to check for quotes/reposts");
    } else {
        println!("Recent events: {} most recent:", events.len());
        for event in &events {
            println!(
                "  {} by @{} ({})",
                event.event_type, event.amplifier_handle, event.detected_at
            );
        }
    }

    // Last scan cursor
    match db::queries::get_scan_state(&conn, "notifications_cursor")? {
        Some(_) => {
            if let Some(last_scan) = db::queries::get_scan_state(&conn, "last_scan_at")? {
                println!("Last scan: {}", last_scan);
            }
        }
        None => {
            println!("Last scan: never");
        }
    }

    Ok(())
}

/// Trait so both the binary's Config and tests can call show().
pub trait HasDbPath {
    fn db_path(&self) -> &str;
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
