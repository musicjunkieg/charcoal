// System status display — shows DB stats, fingerprint age, last scan time.

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

use crate::db::Database;

/// Display system status to the terminal.
pub async fn show(db: &Arc<dyn Database>, db_display_path: &str) -> Result<()> {
    if !Path::new(db_display_path).exists() {
        println!("Database: not initialized");
        println!("\nRun `charcoal init` to set up the database.");
        return Ok(());
    }

    // Database file size
    let file_size = std::fs::metadata(db_display_path)
        .map(|m| format_bytes(m.len()))
        .unwrap_or_else(|_| "unknown".to_string());
    println!("Database: {} ({})", db_display_path, file_size);

    // Fingerprint status
    match db.get_fingerprint().await? {
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
    let all_scores = db.get_ranked_threats(0.0).await?;
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
    let events = db.get_recent_events(5).await?;
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
    match db.get_scan_state("notifications_cursor").await? {
        Some(_) => {
            if let Some(last_scan) = db.get_scan_state("last_scan_at").await? {
                println!("Last scan: {}", last_scan);
            }
        }
        None => {
            println!("Last scan: never");
        }
    }

    Ok(())
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
