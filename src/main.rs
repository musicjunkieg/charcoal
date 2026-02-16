use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use tracing::info;

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

        /// Only look at events since this date (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
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

            println!(
                "{}",
                "Fingerprint saved. Review the topics above — do they look accurate?".bold()
            );
        }

        Commands::DownloadModel => {
            let config = config::Config::load()?;
            let model_dir = &config.model_dir;

            println!("Downloading ONNX toxicity model...");
            println!("  Destination: {}", model_dir.display());

            charcoal::toxicity::download::download_model(model_dir).await?;

            println!("\n{}", "Model downloaded successfully.".bold());
            println!("You can now run `charcoal scan --analyze` or `charcoal score @handle`.");
        }

        Commands::Scan { analyze, max_followers, concurrency, since: _ } => {
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
                // Scorer won't be called without --analyze, but we need a value
                // for the type. Use a no-op Perspective placeholder.
                Box::new(charcoal::toxicity::perspective::PerspectiveScorer::new(
                    String::new(),
                ))
            };

            let weights = charcoal::scoring::threat::ThreatWeights::default();

            let (events, scored) = charcoal::pipeline::amplification::run(
                &agent,
                scorer.as_ref(),
                &conn,
                &protected_fingerprint,
                &weights,
                analyze,
                max_followers as usize,
                concurrency as usize,
            )
            .await?;

            println!("\n{}", "Scan complete.".bold());
            println!("  Events detected: {events}");
            if analyze {
                println!("  Accounts scored: {scored}");
            }
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

            let score = charcoal::scoring::profile::build_profile(
                &agent,
                scorer.as_ref(),
                handle,
                handle, // Use handle as DID placeholder — real DID comes from profile lookup
                &protected_fingerprint,
                &weights,
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

            let threats =
                charcoal::db::queries::get_ranked_threats(&conn, min_score as f64)?;

            if threats.is_empty() {
                println!("No accounts scored yet. Run `charcoal scan --analyze` first.");
                return Ok(());
            }

            // Display in terminal
            charcoal::output::terminal::display_threat_list(&threats);

            // Also generate a markdown report file
            let fingerprint = charcoal::db::queries::get_fingerprint(&conn)?
                .and_then(|(json, _, _)| serde_json::from_str(&json).ok());

            let report_path = charcoal::output::markdown::generate_report(
                &threats,
                fingerprint.as_ref(),
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
            let fp: charcoal::topics::fingerprint::TopicFingerprint =
                serde_json::from_str(&json)?;
            Ok(fp)
        }
        None => {
            anyhow::bail!(
                "No topic fingerprint found. Run `charcoal fingerprint` first to build one."
            );
        }
    }
}

