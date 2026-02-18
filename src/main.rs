use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
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

        /// Also query Constellation backlink index for amplification events
        #[arg(long)]
        constellation: bool,
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

    /// Show system status (last scan, DB stats, fingerprint age)
    Status,
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
            let conn = charcoal::db::initialize(&config.db_path)?;
            let table_count = charcoal::db::schema::table_count(&conn)?;
            println!("Database initialized at: {}", config.db_path);
            println!("Tables created: {table_count}");
            println!("\nCharcoal is ready. Next step: set up your .env file");
            println!("  (see .env.example for required variables)");
            println!("\nThen run: cargo run -- fingerprint");
        }

        Commands::Fingerprint { refresh } => {
            let config = config::Config::load()?;
            config.require_bluesky()?;
            let conn = charcoal::db::open(&config.db_path)?;

            // Check if we already have a fingerprint and it's not being refreshed
            if !refresh {
                if let Some((json, _post_count, updated_at)) =
                    charcoal::db::queries::get_fingerprint(&conn)?
                {
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

            // Authenticate with Bluesky
            let agent = charcoal::bluesky::client::login(
                &config.bluesky_handle,
                &config.bluesky_app_password,
                &config.bluesky_pds_url,
            )
            .await?;

            // Fetch recent posts (target 500 for a good fingerprint)
            let posts =
                charcoal::bluesky::posts::fetch_recent_posts(&agent, &config.bluesky_handle, 500)
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
            charcoal::db::queries::save_fingerprint(&conn, &json, fingerprint.post_count)?;

            // Compute and store the mean sentence embedding for semantic overlap.
            // This is optional — if the embedding model isn't downloaded yet, we
            // skip it and fall back to TF-IDF keyword overlap during scoring.
            let embed_dir = charcoal::toxicity::download::embedding_model_dir(&config.model_dir);
            if charcoal::toxicity::download::embedding_files_present(&config.model_dir) {
                println!("\nComputing sentence embeddings...");
                let embedder = charcoal::topics::embeddings::SentenceEmbedder::load(&embed_dir)?;
                let post_embeddings = embedder.embed_batch(&post_texts).await?;
                let mean_emb = charcoal::topics::embeddings::mean_embedding(&post_embeddings);
                let emb_json = serde_json::to_string(&mean_emb)?;
                charcoal::db::queries::save_embedding(&conn, &emb_json)?;
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
            constellation,
        } => {
            let config = config::Config::load()?;
            config.require_bluesky()?;
            let conn = charcoal::db::open(&config.db_path)?;

            println!("Scanning for amplification events...");

            // Authenticate
            let agent = charcoal::bluesky::client::login(
                &config.bluesky_handle,
                &config.bluesky_app_password,
                &config.bluesky_pds_url,
            )
            .await?;

            // Load the protected user's fingerprint (needed for scoring)
            let protected_fingerprint = load_fingerprint(&conn)?;

            // Create the toxicity scorer if we'll be analyzing
            let scorer: Box<dyn charcoal::toxicity::traits::ToxicityScorer> = if analyze {
                config.require_scorer()?;
                create_scorer(&config)?
            } else {
                Box::new(charcoal::toxicity::traits::NoopScorer)
            };

            let weights = charcoal::scoring::threat::ThreatWeights::default();
            let (embedder, protected_embedding) = load_embedder(&config, &conn);

            // Query Constellation backlink index if requested
            let supplementary_events = if constellation {
                println!("Querying Constellation backlink index...");
                match fetch_constellation_events(&agent, &config).await {
                    Ok(events) => {
                        println!(
                            "  Constellation found {} supplementary events",
                            events.len()
                        );
                        events
                    }
                    Err(e) => {
                        warn!(error = %e, "Constellation query failed, continuing without");
                        println!(
                            "  {} Constellation unavailable, continuing with notifications only",
                            "Warning:".yellow()
                        );
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };

            let (events, scored) = charcoal::pipeline::amplification::run(
                &agent,
                scorer.as_ref(),
                &conn,
                &protected_fingerprint,
                &weights,
                &config.bluesky_handle,
                analyze,
                max_followers as usize,
                concurrency as usize,
                embedder.as_ref(),
                protected_embedding.as_deref(),
                supplementary_events,
            )
            .await?;

            println!("\n{}", "Scan complete.".bold());
            println!("  Events detected: {events}");
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
            let conn = charcoal::db::open(&config.db_path)?;

            println!("Running second-degree network sweep...");

            let agent = charcoal::bluesky::client::login(
                &config.bluesky_handle,
                &config.bluesky_app_password,
                &config.bluesky_pds_url,
            )
            .await?;

            let protected_fingerprint = load_fingerprint(&conn)?;
            let scorer = create_scorer(&config)?;
            let weights = charcoal::scoring::threat::ThreatWeights::default();
            let (embedder, protected_embedding) = load_embedder(&config, &conn);

            let (pool_size, scored) = charcoal::pipeline::sweep::run(
                &agent,
                scorer.as_ref(),
                &conn,
                &config.bluesky_handle,
                &protected_fingerprint,
                &weights,
                max_followers as usize,
                depth as usize,
                concurrency as usize,
                embedder.as_ref(),
                protected_embedding.as_deref(),
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
            let conn = charcoal::db::open(&config.db_path)?;

            // Strip leading @ if present
            let handle = handle.strip_prefix('@').unwrap_or(&handle);

            println!("Scoring account: @{handle}...");

            // Authenticate
            let agent = charcoal::bluesky::client::login(
                &config.bluesky_handle,
                &config.bluesky_app_password,
                &config.bluesky_pds_url,
            )
            .await?;

            // Load the protected user's fingerprint
            let protected_fingerprint = load_fingerprint(&conn)?;

            // Create the toxicity scorer based on configured backend
            let scorer = create_scorer(&config)?;

            let weights = charcoal::scoring::threat::ThreatWeights::default();
            let (embedder, protected_embedding) = load_embedder(&config, &conn);

            let score = charcoal::scoring::profile::build_profile(
                &agent,
                scorer.as_ref(),
                handle,
                handle, // Use handle as DID placeholder — real DID comes from profile lookup
                &protected_fingerprint,
                &weights,
                embedder.as_ref(),
                protected_embedding.as_deref(),
            )
            .await?;

            // Display results
            charcoal::output::terminal::display_account_detail(&score);

            // Store in database
            charcoal::db::queries::upsert_account_score(&conn, &score)?;
        }

        Commands::Report { min_score } => {
            let config = config::Config::load()?;
            let conn = charcoal::db::open(&config.db_path)?;

            let threats = charcoal::db::queries::get_ranked_threats(&conn, min_score as f64)?;

            if threats.is_empty() {
                println!("No accounts scored yet. Run `charcoal scan --analyze` first.");
                return Ok(());
            }

            // Fetch recent amplification events for context
            let events = charcoal::db::queries::get_recent_events(&conn, 100)?;

            // Display in terminal
            charcoal::output::terminal::display_threat_list(&threats);
            charcoal::output::terminal::display_amplification_events(&events);

            // Also generate a markdown report file
            let fingerprint = charcoal::db::queries::get_fingerprint(&conn)?
                .and_then(|(json, _, _)| serde_json::from_str(&json).ok());

            let report_path = charcoal::output::markdown::generate_report(
                &threats,
                fingerprint.as_ref(),
                &events,
                "charcoal-report.md",
            )?;

            println!(
                "\n{}",
                format!("Markdown report saved to: {report_path}").bold()
            );
        }

        Commands::Status => {
            let config = config::Config::load()?;
            charcoal::status::show(&config)?;
        }
    }

    Ok(())
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
fn load_fingerprint(
    conn: &rusqlite::Connection,
) -> Result<charcoal::topics::fingerprint::TopicFingerprint> {
    match charcoal::db::queries::get_fingerprint(conn)? {
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
fn load_embedder(
    config: &config::Config,
    conn: &rusqlite::Connection,
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

    let embedding = match charcoal::db::queries::get_embedding(conn) {
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
/// for quotes and reposts of those posts. This catches events that notification
/// polling misses (e.g. from blocked/muted accounts).
async fn fetch_constellation_events(
    agent: &bsky_sdk::BskyAgent,
    config: &config::Config,
) -> Result<Vec<charcoal::bluesky::notifications::AmplificationNotification>> {
    let client =
        charcoal::constellation::client::ConstellationClient::new(&config.constellation_url)?;

    // Fetch the protected user's recent post URIs to query against
    let posts =
        charcoal::bluesky::posts::fetch_recent_posts(agent, &config.bluesky_handle, 50).await?;

    let post_uris: Vec<String> = posts.iter().map(|p| p.uri.clone()).collect();
    info!(
        post_count = post_uris.len(),
        "Querying Constellation for backlinks"
    );

    let mut events = client.find_amplification_events(&post_uris).await;

    // Resolve DIDs to human-readable handles. Constellation only returns DIDs,
    // but the scoring pipeline needs handles for follower lookups and display.
    let dids: Vec<String> = events
        .iter()
        .filter(|e| e.amplifier_handle.starts_with("did:"))
        .map(|e| e.amplifier_did.clone())
        .collect();

    if !dids.is_empty() {
        match charcoal::bluesky::profiles::resolve_dids_to_handles(agent, &dids).await {
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

    Ok(events)
}
