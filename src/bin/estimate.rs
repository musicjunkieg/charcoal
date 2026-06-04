// estimate — network-estimation tooling for Zentropi call volume.
//
// First stage: candidate harvesting. Draws a sample of candidate "protected
// user" accounts from two complementary sources and prints the merged set:
//
//   1. Jetstream firehose  — activity-weighted sample of accounts posting now
//   2. Topic-keyword search — accounts active in the sensitive topic areas
//
// Later stages (cheap profile filtering, free Constellation engagement
// stratification, and the instrumented Zentropi dry-run counter) build on this
// candidate set. This binary is gated behind the `estimate` feature:
//
//   cargo run --features estimate --bin estimate -- --help
//
// Output is a candidate list (human summary or `--json`). It performs only
// public, read-only API calls — no Zentropi calls, no third-party content sent
// anywhere — so it's safe to run broadly.

use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tracing::warn;

use charcoal::bluesky::client::PublicAtpClient;
use charcoal::config::Config;
use charcoal::discovery::{candidate, jetstream, seeds};

/// Harvest candidate protected-user accounts for Zentropi call-volume estimation.
#[derive(Parser)]
#[command(name = "estimate", version, about)]
struct Cli {
    /// Jetstream subscribe endpoint (wss://).
    #[arg(
        long,
        default_value = "wss://jetstream2.us-east.bsky.network/subscribe"
    )]
    jetstream_url: String,

    /// Number of unique active authors to sample from the firehose.
    #[arg(long, default_value_t = 500)]
    firehose_target: usize,

    /// Maximum seconds to listen to the firehose before giving up.
    #[arg(long, default_value_t = 60)]
    firehose_seconds: u64,

    /// Unique authors to collect per seed keyword from searchPosts.
    #[arg(long, default_value_t = 100)]
    per_keyword: usize,

    /// Skip the firehose sampler (topic search only).
    #[arg(long)]
    skip_firehose: bool,

    /// Skip the topic-keyword harvest (firehose only).
    #[arg(long)]
    skip_topic: bool,

    /// Emit candidates as a JSON array instead of a human-readable summary.
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("charcoal=info,estimate=info")
            }),
        )
        .init();

    let cli = Cli::parse();

    if cli.skip_firehose && cli.skip_topic {
        anyhow::bail!("Nothing to do: both --skip-firehose and --skip-topic were set");
    }

    let config = Config::load()?;
    let client = PublicAtpClient::new(&config.public_api_url)?;

    // Source 1: firehose. A connection failure here is non-fatal — we degrade
    // to whatever the topic harvest finds rather than aborting the whole run.
    let firehose: Vec<String> = if cli.skip_firehose {
        Vec::new()
    } else {
        eprintln!(
            "Sampling Jetstream firehose for up to {} unique authors ({}s max)…",
            cli.firehose_target, cli.firehose_seconds
        );
        let cfg = jetstream::JetstreamConfig {
            endpoint: cli.jetstream_url.clone(),
            target_unique: cli.firehose_target,
            max_duration: Duration::from_secs(cli.firehose_seconds),
        };
        match jetstream::sample_active_authors(&cfg).await {
            Ok(authors) => authors,
            Err(e) => {
                warn!(error = %e, "Firehose sampling failed, continuing without it");
                Vec::new()
            }
        }
    };

    // Source 2: topic-keyword search.
    let topic: Vec<String> = if cli.skip_topic {
        Vec::new()
    } else {
        eprintln!(
            "Harvesting topic-keyword authors ({} keywords, up to {} each)…",
            seeds::SEED_KEYWORDS.len(),
            cli.per_keyword
        );
        match seeds::harvest_by_keywords(&client, seeds::SEED_KEYWORDS, cli.per_keyword).await {
            Ok(authors) => authors,
            Err(e) => {
                warn!(error = %e, "Topic harvest failed, continuing without it");
                Vec::new()
            }
        }
    };

    let candidates = candidate::merge_candidates(&firehose, &topic);

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&candidates)?);
    } else {
        let (firehose_only, topic_only, both) = candidate::source_breakdown(&candidates);
        eprintln!();
        eprintln!("Candidate accounts: {}", candidates.len());
        eprintln!("  firehose only: {firehose_only}");
        eprintln!("  topic only:    {topic_only}");
        eprintln!("  both sources:  {both}");
        eprintln!();
        for c in &candidates {
            println!("{}\t{:?}", c.did, c.source);
        }
    }

    Ok(())
}
