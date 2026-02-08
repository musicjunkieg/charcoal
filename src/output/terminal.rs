// Colored terminal output for threat lists and fingerprints.
//
// This module handles all terminal-specific formatting: colors, tables,
// progress indicators. The main.rs display functions delegate here.

use colored::Colorize;

use crate::db::models::AccountScore;

/// Display a ranked threat list in the terminal.
pub fn display_threat_list(accounts: &[AccountScore]) {
    if accounts.is_empty() {
        println!("No accounts scored yet. Run `charcoal scan --analyze` first.");
        return;
    }

    println!(
        "\n{}",
        format!("=== Threat Report ({} accounts) ===", accounts.len()).bold()
    );
    println!();

    // Header
    println!(
        "  {:>4}  {:<32} {:>6}  {:<10}  {:>5}  {:>7}",
        "Rank".dimmed(),
        "Handle".dimmed(),
        "Score".dimmed(),
        "Tier".dimmed(),
        "Tox".dimmed(),
        "Overlap".dimmed(),
    );
    println!("  {}", "-".repeat(78).dimmed());

    for (i, account) in accounts.iter().enumerate() {
        let tier_str = account.threat_tier.as_deref().unwrap_or("?");
        let colored_tier = colorize_tier(tier_str);

        println!(
            "  {:>4}. @{:<30} {:>6.1}  {:<10}  {:>.2}  {:>7.2}",
            i + 1,
            account.handle,
            account.threat_score.unwrap_or(0.0),
            colored_tier,
            account.toxicity_score.unwrap_or(0.0),
            account.topic_overlap.unwrap_or(0.0),
        );
    }

    println!();

    // Summary
    let high = accounts
        .iter()
        .filter(|a| a.threat_tier.as_deref() == Some("High"))
        .count();
    let elevated = accounts
        .iter()
        .filter(|a| a.threat_tier.as_deref() == Some("Elevated"))
        .count();
    let watch = accounts
        .iter()
        .filter(|a| a.threat_tier.as_deref() == Some("Watch"))
        .count();

    if high > 0 {
        println!("  {} {} threat accounts", "!!".red().bold(), high);
    }
    if elevated > 0 {
        println!("  {} {} elevated accounts", "!".bright_red(), elevated);
    }
    if watch > 0 {
        println!("  {} {} watch accounts", "~".yellow(), watch);
    }
}

/// Display a single account's detailed score.
pub fn display_account_detail(score: &AccountScore) {
    println!(
        "\n{}",
        format!("=== Score for @{} ===", score.handle).bold()
    );

    if let Some(tier) = &score.threat_tier {
        println!("  Threat tier: {}", colorize_tier(tier));
    }

    if let Some(threat) = score.threat_score {
        println!("  Threat score: {:.1}/100", threat);
    }
    if let Some(tox) = score.toxicity_score {
        println!("  Toxicity: {:.2}", tox);
    }
    if let Some(overlap) = score.topic_overlap {
        println!("  Topic overlap: {:.2}", overlap);
    }
    println!("  Posts analyzed: {}", score.posts_analyzed);

    if !score.top_toxic_posts.is_empty() {
        println!(
            "\n  {} most toxic posts (evidence):",
            score.top_toxic_posts.len()
        );
        for (i, post) in score.top_toxic_posts.iter().enumerate() {
            let preview = if post.text.len() > 120 {
                format!("{}...", &post.text[..120])
            } else {
                post.text.clone()
            };
            println!(
                "    {}. [tox: {:.2}] {}",
                i + 1,
                post.toxicity,
                preview.dimmed()
            );
        }
    }
}

/// Colorize a threat tier string.
fn colorize_tier(tier: &str) -> colored::ColoredString {
    match tier {
        "High" => tier.red().bold(),
        "Elevated" => tier.bright_red(),
        "Watch" => tier.yellow(),
        "Low" => tier.green(),
        _ => tier.dimmed(),
    }
}
