use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{info, warn};

mod config;

/// Charcoal: Predictive threat detection for Bluesky.
///
/// Identifies accounts likely to engage with your content in a toxic or
/// bad-faith manner — before that engagement happens.
#[derive(Parser)]
#[command(name = "charcoal", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize the database and configuration
    Init,

    /// Show or refresh your topic fingerprint
    Fingerprint {
        /// Force a full rebuild of the fingerprint
        #[arg(long)]
        refresh: bool,
    },

    /// Download the ONNX toxicity model (~126 MB)
    DownloadModel,

    /// Scan for amplification events (quotes and reposts)
    Scan {
        /// Also analyze followers of amplifiers
        #[arg(long)]
        analyze: bool,

        /// Max followers to analyze per amplifier (default: 50)
        #[arg(long, default_value = "50")]
        max_followers: u32,

        /// Number of accounts to score in parallel (default: 8)
        #[arg(long, default_value = "8")]
        concurrency: u32,
    },

    /// Sweep second-degree network (followers-of-followers) for threats
    Sweep {
        /// Max first-degree followers to scan (default: 200)
        #[arg(long, default_value = "200")]
        max_followers: u32,

        /// Max second-degree followers per first-degree (default: 50)
        #[arg(long, default_value = "50")]
        depth: u32,

        /// Number of accounts to score in parallel (default: 8)
        #[arg(long, default_value = "8")]
        concurrency: u32,
    },

    /// Score a specific Bluesky account
    Score {
        /// The handle to score (e.g. someone.bsky.social)
        handle: String,
    },

    /// Generate a threat report
    Report {
        /// Only include accounts at or above this threat score
        #[arg(long, default_value = "0")]
        min_score: u32,
    },

    /// Validate scoring by analyzing your blocked accounts
    Validate {
        /// Number of recent blocks to analyze (default: 10)
        #[arg(long, default_value = "10")]
        count: u32,
    },

    /// Show system status (last scan, DB stats, fingerprint age)
    Status,

    /// Migrate data from SQLite to PostgreSQL
    #[cfg(feature = "postgres")]
    Migrate {
        /// PostgreSQL connection URL (e.g. postgres://user:pass@localhost/charcoal)
        #[arg(long)]
        database_url: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present (silently ignore if missing)
    let _ = dotenvy::dotenv();

    // Set up structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("charcoal=info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            info!("Initializing Charcoal database...");
            let config = config::Config::load()?;
            let db = init_database(&config).await?;
            let table_count = db.table_count().await?;
            println!("Database initialized at: {}", config.db_path);
            println!("Tables created: {table_count}");
            println!("\nCharcoal is ready. Next step: set up your .env file");
            println!("  (see .env.example for required variables)");
            println!("\nThen run: cargo run -- fingerprint");
        }

        Commands::Fingerprint { refresh } => {
            let config = config::Config::load()?;
            config.require_bluesky()?;
            let db = open_database(&config).await?;

            // Check if we already have a fingerprint and it's not being refreshed
            if !refresh {
                if let Some((json, _post_count, updated_at)) = db.get_fingerprint().await? {
                    println!("Loading cached fingerprint (built {updated_at})...");
                    let fingerprint: charcoal::topics::fingerprint::TopicFingerprint =
                        serde_json::from_str(&json)?;
                    fingerprint.display();
                    println!(
                        "{}",
                        "To rebuild, run: cargo run -- fingerprint --refresh".dimmed()
                    );
                    return Ok(());
                }
            }

            println!("Building topic fingerprint from your recent posts...");

            let client = charcoal::bluesky::client::PublicAtpClient::new(&config.public_api_url)?;

            // Fetch recent posts (target 500 for a good fingerprint)
            let posts =
                charcoal::bluesky::posts::fetch_recent_posts(&client, &config.bluesky_handle, 500)
                    .await?;

            println!("Analyzing {} posts...", posts.len());

            let post_texts: Vec<String> = posts.iter().map(|p| p.text.clone()).collect();

            // Run TF-IDF extraction
            let extractor = charcoal::topics::tfidf::TfIdfExtractor::default();
            let fingerprint =
                charcoal::topics::traits::TopicExtractor::extract(&extractor, &post_texts)?;

            // Display the fingerprint
            fingerprint.display();

            // Cache in the database
            let json = serde_json::to_string(&fingerprint)?;
            db.save_fingerprint(&json, fingerprint.post_count).await?;

            // Compute and store the mean sentence embedding for semantic overlap.
            // This is optional — if the embedding model isn't downloaded yet, we
            // skip it and fall back to TF-IDF keyword overlap during scoring.
            let embed_dir = charcoal::toxicity::download::embedding_model_dir(&config.model_dir);
            if charcoal::toxicity::download::embedding_files_present(&config.model_dir) {
                println!("\nComputing sentence embeddings...");
                let embedder = charcoal::topics::embeddings::SentenceEmbedder::load(&embed_dir)?;
                let post_embeddings = embedder.embed_batch(&post_texts).await?;
                let mean_emb = charcoal::topics::embeddings::mean_embedding(&post_embeddings);
                db.save_embedding(&mean_emb).await?;
                println!(
                    "  Embedding computed ({} posts → {}-dim vector)",
                    post_texts.len(),
                    charcoal::topics::embeddings::EMBEDDING_DIM,
                );
            } else {
                println!(
                    "\n{}",
                    "Tip: Run `charcoal download-model` to enable semantic topic overlap.".dimmed()
                );
            }

            println!(
                "{}",
                "Fingerprint saved. Review the topics above — do they look accurate?".bold()
            );
        }

        Commands::DownloadModel => {
            let config = config::Config::load()?;
            let model_dir = &config.model_dir;

            println!("Downloading ONNX models...");
            println!("  Destination: {}", model_dir.display());

            charcoal::toxicity::download::download_model(model_dir).await?;

            println!("\n{}", "Models downloaded successfully.".bold());
            println!("You can now run `charcoal scan --analyze` or `charcoal score @handle`.");
        }

        Commands::Scan {
            analyze,
            max_followers,
            concurrency,
        } => {
            let config = config::Config::load()?;
            config.require_bluesky()?;
            let db = open_database(&config).await?;

            println!("Scanning for amplification events...");

            let client = charcoal::bluesky::client::PublicAtpClient::new(&config.public_api_url)?;

            // Load the protected user's fingerprint (needed for scoring)
            let protected_fingerprint = load_fingerprint(&db).await?;

            // Create the toxicity scorer if we'll be analyzing
            let scorer: Box<dyn charcoal::toxicity::traits::ToxicityScorer> = if analyze {
                config.require_scorer()?;
                create_scorer(&config)?
            } else {
                Box::new(charcoal::toxicity::traits::NoopScorer)
            };

            let weights = charcoal::scoring::threat::ThreatWeights::default();
            let (embedder, protected_embedding) = load_embedder(&config, &db).await;

            // Compute behavioral context for scoring
            let median_engagement = db.get_median_engagement().await?;
            let pile_on_events = db.get_events_for_pile_on().await?;
            let pile_on_refs: Vec<(&str, &str, &str)> = pile_on_events
                .iter()
                .map(|(d, u, t)| (d.as_str(), u.as_str(), t.as_str()))
                .collect();
            let pile_on_dids =
                charcoal::scoring::behavioral::detect_pile_on_participants(&pile_on_refs);

            // Query Constellation backlink index for amplification events
            println!("Querying Constellation backlink index...");
            let events = match fetch_constellation_events(&client, &config).await {
                Ok(events) => {
                    println!("  Constellation found {} events", events.len());
                    events
                }
                Err(e) => {
                    warn!(error = %e, "Constellation query failed");
                    println!("  {} Constellation unavailable: {}", "Warning:".yellow(), e);
                    Vec::new()
                }
            };

            let (event_count, scored) = charcoal::pipeline::amplification::run(
                &client,
                scorer.as_ref(),
                &db,
                &protected_fingerprint,
                &weights,
                &config.bluesky_handle,
                analyze,
                max_followers as usize,
                concurrency as usize,
                embedder.as_ref(),
                protected_embedding.as_deref(),
                events,
                median_engagement,
                &pile_on_dids,
            )
            .await?;

            println!("\n{}", "Scan complete.".bold());
            println!("  Events detected: {event_count}");
            if analyze {
                println!("  Accounts scored: {scored}");
            }
        }

        Commands::Sweep {
            max_followers,
            depth,
            concurrency,
        } => {
            let config = config::Config::load()?;
            config.require_bluesky()?;
            config.require_scorer()?;
            let db = open_database(&config).await?;

            println!("Running second-degree network sweep...");

            let client = charcoal::bluesky::client::PublicAtpClient::new(&config.public_api_url)?;

            let protected_fingerprint = load_fingerprint(&db).await?;
            let scorer = create_scorer(&config)?;
            let weights = charcoal::scoring::threat::ThreatWeights::default();
            let (embedder, protected_embedding) = load_embedder(&config, &db).await;

            let median_engagement = db.get_median_engagement().await?;
            let pile_on_events = db.get_events_for_pile_on().await?;
            let pile_on_refs: Vec<(&str, &str, &str)> = pile_on_events
                .iter()
                .map(|(d, u, t)| (d.as_str(), u.as_str(), t.as_str()))
                .collect();
            let pile_on_dids =
                charcoal::scoring::behavioral::detect_pile_on_participants(&pile_on_refs);

            let (pool_size, scored) = charcoal::pipeline::sweep::run(
                &client,
                scorer.as_ref(),
                &db,
                &config.bluesky_handle,
                &protected_fingerprint,
                &weights,
                max_followers as usize,
                depth as usize,
                concurrency as usize,
                embedder.as_ref(),
                protected_embedding.as_deref(),
                median_engagement,
                &pile_on_dids,
            )
            .await?;

            println!("\n{}", "Sweep complete.".bold());
            println!("  Second-degree pool: {pool_size}");
            println!("  Accounts scored: {scored}");
        }

        Commands::Score { handle } => {
            let config = config::Config::load()?;
            config.require_bluesky()?;
            config.require_scorer()?;
            let db = open_database(&config).await?;

            // Strip leading @ if present
            let handle = handle.strip_prefix('@').unwrap_or(&handle);

            println!("Scoring account: @{handle}...");

            let client = charcoal::bluesky::client::PublicAtpClient::new(&config.public_api_url)?;

            // Load the protected user's fingerprint
            let protected_fingerprint = load_fingerprint(&db).await?;

            // Create the toxicity scorer based on configured backend
            let scorer = create_scorer(&config)?;

            let weights = charcoal::scoring::threat::ThreatWeights::default();
            let (embedder, protected_embedding) = load_embedder(&config, &db).await;

            let median_engagement = db.get_median_engagement().await?;
            let pile_on_events = db.get_events_for_pile_on().await?;
            let pile_on_refs: Vec<(&str, &str, &str)> = pile_on_events
                .iter()
                .map(|(d, u, t)| (d.as_str(), u.as_str(), t.as_str()))
                .collect();
            let pile_on_dids =
                charcoal::scoring::behavioral::detect_pile_on_participants(&pile_on_refs);

            let score = charcoal::scoring::profile::build_profile(
                &client,
                scorer.as_ref(),
                handle,
                handle, // Use handle as DID placeholder — real DID comes from profile lookup
                &protected_fingerprint,
                &weights,
                embedder.as_ref(),
                protected_embedding.as_deref(),
                median_engagement,
                &pile_on_dids,
            )
            .await?;

            // Display results
            charcoal::output::terminal::display_account_detail(&score);

            // Store in database
            db.upsert_account_score(&score).await?;
        }

        Commands::Report { min_score } => {
            let config = config::Config::load()?;
            let db = open_database(&config).await?;

            let threats = db.get_ranked_threats(min_score as f64).await?;

            if threats.is_empty() {
                println!("No accounts scored yet. Run `charcoal scan --analyze` first.");
                return Ok(());
            }

            // Fetch recent amplification events for context
            let events = db.get_recent_events(100).await?;

            // Display in terminal
            charcoal::output::terminal::display_threat_list(&threats);
            charcoal::output::terminal::display_amplification_events(&events);

            // Also generate a markdown report file
            let fingerprint = db
                .get_fingerprint()
                .await?
                .and_then(|(json, _, _)| serde_json::from_str(&json).ok());

            let report_path = charcoal::output::markdown::generate_report(
                &threats,
                fingerprint.as_ref(),
                &events,
                "output/charcoal-report.md",
            )?;

            println!(
                "\n{}",
                format!("Markdown report saved to: {report_path}").bold()
            );
        }

        Commands::Validate { count } => {
            let config = config::Config::load()?;
            config.require_bluesky()?;
            config.require_scorer()?;
            let db = open_database(&config).await?;

            let client = charcoal::bluesky::client::PublicAtpClient::new(&config.public_api_url)?;

            println!("Resolving your PDS endpoint...");

            // Resolve handle → DID → PDS URL (block records live on your PDS)
            let did = client.resolve_handle(&config.bluesky_handle).await?;
            let pds_url = client.resolve_pds_url(&did).await?;
            println!("  PDS: {pds_url}");

            let pds_client = charcoal::bluesky::client::PublicAtpClient::new(&pds_url)?;

            println!("Fetching your {} most recent blocks...", count);

            // Fetch block records from the PDS (reverse=true for most recent first)
            let limit_str = count.to_string();
            let blocks: charcoal::bluesky::client::ListRecordsResponse = pds_client
                .xrpc_get(
                    "com.atproto.repo.listRecords",
                    &[
                        ("repo", &did),
                        ("collection", "app.bsky.graph.block"),
                        ("limit", &limit_str),
                        ("reverse", "true"),
                    ],
                )
                .await?;

            if blocks.records.is_empty() {
                println!("No block records found.");
                return Ok(());
            }

            // Extract blocked DIDs and timestamps from the record values
            let blocked_accounts: Vec<charcoal::bluesky::client::BlockRecordValue> = blocks
                .records
                .iter()
                .filter_map(|r| {
                    serde_json::from_value::<charcoal::bluesky::client::BlockRecordValue>(
                        r.value.clone(),
                    )
                    .ok()
                })
                .collect();

            println!("  Found {} block records", blocked_accounts.len());

            // Resolve DIDs to handles
            let dids: Vec<String> = blocked_accounts.iter().map(|b| b.subject.clone()).collect();
            let resolved =
                charcoal::bluesky::profiles::resolve_dids_to_handles(&client, &dids).await?;

            // Set up scoring
            let protected_fingerprint = load_fingerprint(&db).await?;
            let scorer = create_scorer(&config)?;
            let weights = charcoal::scoring::threat::ThreatWeights::default();
            let (embedder, protected_embedding) = load_embedder(&config, &db).await;

            let median_engagement = db.get_median_engagement().await?;
            let pile_on_events = db.get_events_for_pile_on().await?;
            let pile_on_refs: Vec<(&str, &str, &str)> = pile_on_events
                .iter()
                .map(|(d, u, t)| (d.as_str(), u.as_str(), t.as_str()))
                .collect();
            let pile_on_dids =
                charcoal::scoring::behavioral::detect_pile_on_participants(&pile_on_refs);

            println!(
                "\n{}",
                "=== Validation: Scoring Blocked Accounts ===".bold()
            );
            println!(
                "{}",
                "These are accounts you manually blocked. The pipeline should flag them.\n"
                    .dimmed()
            );

            // Header
            println!(
                "  {:<4} {:<36} {:>6} {:>8} {:>8}  Tier",
                "#", "Handle", "Score", "Tox", "Overlap"
            );
            println!("  {}", "-".repeat(80));

            let mut scored_count = 0;
            let mut watch_plus = 0;

            for (i, block) in blocked_accounts.iter().enumerate() {
                let handle = resolved
                    .get(&block.subject)
                    .cloned()
                    .unwrap_or_else(|| block.subject.clone());

                let blocked_date = &block.created_at[..10]; // YYYY-MM-DD

                match charcoal::scoring::profile::build_profile(
                    &client,
                    scorer.as_ref(),
                    &handle,
                    &block.subject,
                    &protected_fingerprint,
                    &weights,
                    embedder.as_ref(),
                    protected_embedding.as_deref(),
                    median_engagement,
                    &pile_on_dids,
                )
                .await
                {
                    Ok(score) => {
                        let tier_str = score.threat_tier.as_deref().unwrap_or("?");
                        let threat = score.threat_score.unwrap_or(0.0);
                        let tox = score.toxicity_score.unwrap_or(0.0);
                        let overlap = score.topic_overlap.unwrap_or(0.0);

                        // Color the tier
                        let tier_colored = match tier_str {
                            "High" => tier_str.red().bold().to_string(),
                            "Elevated" => tier_str.yellow().bold().to_string(),
                            "Watch" => tier_str.yellow().to_string(),
                            _ => tier_str.dimmed().to_string(),
                        };

                        println!(
                            "  {:<4} {:<36} {:>6.1} {:>8.3} {:>8.2}  {}  (blocked {})",
                            format!("{}.", i + 1),
                            format!("@{handle}"),
                            threat,
                            tox,
                            overlap,
                            tier_colored,
                            blocked_date,
                        );

                        // Show top toxic post as evidence if score is notable
                        if threat >= 8.0 {
                            if let Some(top) = score.top_toxic_posts.first() {
                                let preview = charcoal::output::truncate_chars(&top.text, 100);
                                println!(
                                    "        {} \"{}\"",
                                    format!("[tox: {:.2}]", top.toxicity).dimmed(),
                                    preview.dimmed(),
                                );
                            }
                        }

                        if tier_str == "Watch" || tier_str == "Elevated" || tier_str == "High" {
                            watch_plus += 1;
                        }

                        // Store in DB too
                        db.upsert_account_score(&score).await?;
                        scored_count += 1;
                    }
                    Err(e) => {
                        println!(
                            "  {:<4} {:<36} {}  (blocked {})",
                            format!("{}.", i + 1),
                            format!("@{handle}"),
                            format!("Error: {e}").red(),
                            blocked_date,
                        );
                    }
                }
            }

            println!("\n{}", "=== Validation Summary ===".bold());
            println!("  Blocked accounts scored: {scored_count}");
            println!("  Watch or higher:         {watch_plus}");
            let detection_rate = if scored_count > 0 {
                (watch_plus as f64 / scored_count as f64) * 100.0
            } else {
                0.0
            };
            println!("  Detection rate:          {detection_rate:.0}%");

            if detection_rate >= 50.0 {
                println!(
                    "\n  {}",
                    "Pipeline is catching a majority of manually-blocked accounts.".green()
                );
            } else if detection_rate > 0.0 {
                println!(
                    "\n  {}",
                    "Pipeline is catching some blocked accounts. Review the Low-tier ones —"
                        .yellow()
                );
                println!(
                    "  {}",
                    "they may be blocked for reasons outside Charcoal's model (e.g. spam, DMs)."
                        .yellow()
                );
            } else {
                println!(
                    "\n  {}",
                    "No blocked accounts scored Watch+. This could mean:".yellow()
                );
                println!(
                    "  {}",
                    "  - Blocked accounts are inactive or have few posts".yellow()
                );
                println!(
                    "  {}",
                    "  - Blocks were for reasons outside the toxicity model".yellow()
                );
                println!("  {}", "  - Scoring thresholds may need tuning".yellow());
            }
        }

        Commands::Status => {
            let config = config::Config::load()?;
            let db = open_database(&config).await?;
            // Build a display-friendly identifier. For PostgreSQL, redact the
            // password from the connection URL before printing it.
            let db_display = match config.database_url.as_deref() {
                Some(url) if url.starts_with("postgres://") || url.starts_with("postgresql://") => {
                    match url.find('@') {
                        Some(at) => {
                            let scheme_end = url.find("://").map(|i| i + 3).unwrap_or(0);
                            format!("{}****@{}", &url[..scheme_end], &url[at + 1..])
                        }
                        None => url.to_string(),
                    }
                }
                _ => config.db_path.clone(),
            };
            charcoal::status::show(&db, &db_display).await?;
        }

        #[cfg(feature = "postgres")]
        Commands::Migrate { database_url } => {
            let config = config::Config::load()?;

            println!("Migrating data from SQLite to PostgreSQL...");
            println!("  Source: {}", config.db_path);
            // Redact credentials in the connection URL for display.
            // Preserve the scheme and host; hide the user:password portion.
            // e.g. "postgres://user:pass@host/db" → "postgres://****@host/db"
            let redacted = match database_url.find('@') {
                Some(at) => {
                    let scheme_end = database_url.find("://").map(|i| i + 3).unwrap_or(0);
                    format!(
                        "{}****@{}",
                        &database_url[..scheme_end],
                        &database_url[at + 1..]
                    )
                }
                None => database_url.clone(),
            };
            println!("  Destination: {redacted}");
            println!();

            // Open source (SQLite) and destination (Postgres)
            let sqlite_db = charcoal::db::open_sqlite(&config.db_path)?;
            let pg_db = charcoal::db::connect_postgres(&database_url).await?;

            // 1. Migrate fingerprint + embedding
            if let Some((json, count, _)) = sqlite_db.get_fingerprint().await? {
                pg_db.save_fingerprint(&json, count).await?;
                println!(
                    "  {} Topic fingerprint migrated ({count} posts)",
                    "✓".green()
                );

                // Migrate embedding if present
                if let Some(embedding) = sqlite_db.get_embedding().await? {
                    pg_db.save_embedding(&embedding).await?;
                    println!(
                        "  {} Embedding migrated ({}-dim vector)",
                        "✓".green(),
                        embedding.len()
                    );
                }
            } else {
                println!("  {} No fingerprint to migrate", "-".dimmed());
            }

            // 2. Migrate account scores
            let scores = sqlite_db.get_ranked_threats(0.0).await?;
            for score in &scores {
                pg_db.upsert_account_score(score).await?;
            }
            println!("  {} {} account scores migrated", "✓".green(), scores.len());

            // 3. Migrate amplification events — preserve original detected_at
            // timestamps so pile-on detection works correctly after migration.
            // Use i32::MAX as the limit rather than u32::MAX to avoid an
            // overflow when the Postgres backend casts the value to i32.
            let events = sqlite_db.get_recent_events(i32::MAX as u32).await?;
            for event in &events {
                pg_db.insert_amplification_event_raw(event).await?;
            }
            println!(
                "  {} {} amplification events migrated",
                "✓".green(),
                events.len()
            );

            // 4. Migrate all scan state keys (not just a hardcoded subset) so
            // cursors, timestamps, and any future keys transfer automatically.
            let scan_entries = sqlite_db.get_all_scan_state().await?;
            let scan_migrated = scan_entries.len();
            for (key, val) in &scan_entries {
                pg_db.set_scan_state(key, val).await?;
            }
            if scan_migrated > 0 {
                println!(
                    "  {} {scan_migrated} scan state entries migrated",
                    "✓".green()
                );
            }

            println!("\n{}", "Migration complete!".green().bold());
            println!(
                "Set {} in your .env to switch to PostgreSQL.",
                "DATABASE_URL".bold()
            );
        }
    }

    Ok(())
}

/// Select the database backend based on configuration.
///
/// When DATABASE_URL is set and points to PostgreSQL, uses the Postgres backend
/// (requires the `postgres` feature). Otherwise, falls back to SQLite.
async fn open_database(config: &config::Config) -> Result<Arc<dyn charcoal::db::Database>> {
    if let Some(ref url) = config.database_url {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            #[cfg(feature = "postgres")]
            {
                info!("Using PostgreSQL backend");
                return charcoal::db::connect_postgres(url).await;
            }
            #[cfg(not(feature = "postgres"))]
            anyhow::bail!(
                "DATABASE_URL points to PostgreSQL but the 'postgres' feature is not compiled in.\n\
                 Rebuild with: cargo build --features postgres"
            );
        }
    }
    charcoal::db::open_sqlite(&config.db_path)
}

/// Initialize the database (create if needed).
async fn init_database(config: &config::Config) -> Result<Arc<dyn charcoal::db::Database>> {
    if let Some(ref url) = config.database_url {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            #[cfg(feature = "postgres")]
            {
                info!("Using PostgreSQL backend");
                return charcoal::db::connect_postgres(url).await;
            }
            #[cfg(not(feature = "postgres"))]
            anyhow::bail!(
                "DATABASE_URL points to PostgreSQL but the 'postgres' feature is not compiled in.\n\
                 Rebuild with: cargo build --features postgres"
            );
        }
    }
    charcoal::db::initialize_sqlite(&config.db_path)
}

/// Create a toxicity scorer based on the configured backend.
fn create_scorer(
    config: &config::Config,
) -> anyhow::Result<Box<dyn charcoal::toxicity::traits::ToxicityScorer>> {
    match config.scorer_backend {
        config::ScorerBackend::Onnx => {
            info!("Using local ONNX toxicity scorer");
            let scorer = charcoal::toxicity::onnx::OnnxToxicityScorer::load(&config.model_dir)?;
            Ok(Box::new(scorer))
        }
        config::ScorerBackend::Perspective => {
            info!("Using Perspective API toxicity scorer");
            let scorer = charcoal::toxicity::perspective::PerspectiveScorer::new(
                config.perspective_api_key.clone(),
            );
            Ok(Box::new(scorer))
        }
    }
}

/// Load the protected user's fingerprint from the database, or bail with a helpful message.
async fn load_fingerprint(
    db: &Arc<dyn charcoal::db::Database>,
) -> Result<charcoal::topics::fingerprint::TopicFingerprint> {
    match db.get_fingerprint().await? {
        Some((json, _, _)) => {
            let fp: charcoal::topics::fingerprint::TopicFingerprint = serde_json::from_str(&json)?;
            Ok(fp)
        }
        None => {
            anyhow::bail!(
                "No topic fingerprint found. Run `charcoal fingerprint` first to build one."
            );
        }
    }
}

/// Try to load the sentence embedder and the protected user's stored embedding.
/// Returns (None, None) if the model isn't downloaded or no embedding is stored.
/// This is optional — scoring falls back to TF-IDF keyword overlap without it.
async fn load_embedder(
    config: &config::Config,
    db: &Arc<dyn charcoal::db::Database>,
) -> (
    Option<charcoal::topics::embeddings::SentenceEmbedder>,
    Option<Vec<f64>>,
) {
    let embed_dir = charcoal::toxicity::download::embedding_model_dir(&config.model_dir);

    let embedder = if charcoal::toxicity::download::embedding_files_present(&config.model_dir) {
        match charcoal::topics::embeddings::SentenceEmbedder::load(&embed_dir) {
            Ok(e) => {
                info!("Loaded sentence embedding model");
                Some(e)
            }
            Err(e) => {
                warn!("Failed to load embedding model, falling back to TF-IDF: {e}");
                None
            }
        }
    } else {
        None
    };

    let embedding = match db.get_embedding().await {
        Ok(Some(v)) => Some(v),
        Ok(None) => {
            if embedder.is_some() {
                warn!("Embedding model loaded but no stored embedding. Run `charcoal fingerprint --refresh`.");
            }
            None
        }
        Err(e) => {
            warn!("Failed to load stored embedding: {e}");
            None
        }
    };

    (embedder, embedding)
}

/// Query the Constellation backlink index for amplification events.
///
/// Fetches the protected user's recent post URIs, then queries Constellation
/// for quotes and reposts of those posts. Resolves DIDs to handles for display
/// and scoring pipeline compatibility.
async fn fetch_constellation_events(
    client: &charcoal::bluesky::client::PublicAtpClient,
    config: &config::Config,
) -> Result<Vec<charcoal::bluesky::amplification::AmplificationNotification>> {
    let constellation =
        charcoal::constellation::client::ConstellationClient::new(&config.constellation_url)?;

    // Fetch the protected user's recent post URIs to query against
    let posts =
        charcoal::bluesky::posts::fetch_recent_posts(client, &config.bluesky_handle, 50).await?;

    let post_uris: Vec<String> = posts.iter().map(|p| p.uri.clone()).collect();
    info!(
        post_count = post_uris.len(),
        "Querying Constellation for backlinks"
    );

    let mut events = constellation.find_amplification_events(&post_uris).await;

    // Resolve DIDs to human-readable handles. Constellation only returns DIDs,
    // but the scoring pipeline needs handles for follower lookups and display.
    let dids: Vec<String> = events
        .iter()
        .filter(|e| e.amplifier_handle.starts_with("did:"))
        .map(|e| e.amplifier_did.clone())
        .collect();

    if !dids.is_empty() {
        match charcoal::bluesky::profiles::resolve_dids_to_handles(client, &dids).await {
            Ok(resolved) => {
                for event in &mut events {
                    if let Some(handle) = resolved.get(&event.amplifier_did) {
                        event.amplifier_handle = handle.clone();
                    }
                }
                info!(
                    resolved = resolved.len(),
                    total = dids.len(),
                    "Resolved Constellation DIDs to handles"
                );
            }
            Err(e) => {
                warn!(error = %e, "Failed to resolve DIDs, using raw DIDs as handles");
            }
        }
    }

    // Deduplicate events by amplifier_post_uri
    let mut seen = HashSet::new();
    events.retain(|e| seen.insert(e.amplifier_post_uri.clone()));

    Ok(events)
}
