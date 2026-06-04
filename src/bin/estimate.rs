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

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use serde::Serialize;
use tracing::warn;

use charcoal::bluesky::client::PublicAtpClient;
use charcoal::config::Config;
use charcoal::discovery::candidate::CandidateSource;
use charcoal::discovery::{candidate, jetstream, profile_filter, seeds};

/// One JSON output record: the surviving profile's stats plus its harvest
/// provenance, flattened into a single object.
#[derive(Serialize)]
struct OutputRecord<'a> {
    #[serde(flatten)]
    stats: &'a profile_filter::ProfileStats,
    source: CandidateSource,
}

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

    /// Skip the viability filter and emit the raw merged candidate set.
    #[arg(long)]
    skip_filter: bool,

    /// Minimum post count for the viability filter (below this Charcoal can't
    /// score the account reliably).
    #[arg(long, default_value_t = 5)]
    min_posts: i64,

    /// Minimum follower count for the viability filter (0 = no follower gate).
    #[arg(long, default_value_t = 0)]
    min_followers: i64,

    /// Minimum account age in days for the viability filter (0 = no age gate).
    #[arg(long, default_value_t = 0)]
    min_account_age_days: i64,
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
    let (firehose_only, topic_only, both) = candidate::source_breakdown(&candidates);
    eprintln!();
    eprintln!(
        "Harvested {} candidates (firehose only: {firehose_only}, topic only: {topic_only}, both: {both})",
        candidates.len()
    );

    // --skip-filter: emit the raw merged set without fetching any profiles.
    if cli.skip_filter {
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&candidates)?);
        } else {
            for c in &candidates {
                println!("{}\t{:?}", c.did, c.source);
            }
        }
        return Ok(());
    }

    // Viability filter: fetch profiles and drop accounts that can't be scored.
    let source_by_did: HashMap<String, CandidateSource> = candidates
        .iter()
        .map(|c| (c.did.clone(), c.source))
        .collect();
    let dids: Vec<String> = candidates.iter().map(|c| c.did.clone()).collect();

    let thresholds = profile_filter::FilterThresholds {
        min_posts: cli.min_posts,
        min_followers: cli.min_followers,
        min_account_age_days: cli.min_account_age_days,
    };
    eprintln!(
        "Filtering {} candidates (min_posts={}, min_followers={}, min_age_days={})…",
        dids.len(),
        cli.min_posts,
        cli.min_followers,
        cli.min_account_age_days
    );
    let report = profile_filter::filter_candidates(&client, &dids, &thresholds).await;

    eprintln!();
    eprintln!(
        "Viable candidates: {} / {}",
        report.kept.len(),
        report.requested
    );
    eprintln!("  not found (unresolvable): {}", report.not_found);
    for (reason, count) in &report.rejected_by_reason {
        eprintln!("  rejected [{reason}]: {count}");
    }
    eprintln!();

    // A kept profile's DID always came from the candidate set, so the source
    // lookup never misses; default to Both only to satisfy the type.
    let source_of = |did: &str| {
        source_by_did
            .get(did)
            .copied()
            .unwrap_or(CandidateSource::Both)
    };

    if cli.json {
        let records: Vec<OutputRecord> = report
            .kept
            .iter()
            .map(|stats| OutputRecord {
                source: source_of(&stats.did),
                stats,
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&records)?);
    } else {
        for s in &report.kept {
            println!(
                "{}\t@{}\tposts={}\tfollowers={}\t{:?}",
                s.did,
                s.handle,
                s.posts_count.unwrap_or(0),
                s.followers_count.unwrap_or(0),
                source_of(&s.did),
            );
        }
    }

    Ok(())
}
