use anyhow::Result;
use clap::{Parser, Subcommand};
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

    /// Scan for amplification events (quotes and reposts)
    Scan {
        /// Also analyze followers of amplifiers
        #[arg(long)]
        analyze: bool,

        /// Only look at events since this date (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
    },

    /// Score a specific Bluesky account
    Score {
        /// The handle to score (e.g. @someone.bsky.social)
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
            if refresh {
                println!("Refreshing topic fingerprint...");
            } else {
                println!("Loading topic fingerprint...");
            }
            println!("(Not yet implemented — coming in Phase 3)");
        }

        Commands::Scan { analyze, since } => {
            println!("Scanning for amplification events...");
            if let Some(ref date) = since {
                println!("  Looking back to: {date}");
            }
            if analyze {
                println!("  Will analyze followers of amplifiers");
            }
            println!("(Not yet implemented — coming in Phase 5)");
        }

        Commands::Score { handle } => {
            println!("Scoring account: {handle}");
            println!("(Not yet implemented — coming in Phase 4)");
        }

        Commands::Report { min_score } => {
            println!("Generating threat report (min score: {min_score})...");
            println!("(Not yet implemented — coming in Phase 7)");
        }

        Commands::Status => {
            let config = config::Config::load()?;
            charcoal::status::show(&config)?;
        }
    }

    Ok(())
}
