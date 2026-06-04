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

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use futures::stream::{self, StreamExt};
use serde::Serialize;
use tracing::warn;

use charcoal::bluesky::client::PublicAtpClient;
use charcoal::config::Config;
use charcoal::constellation::client::ConstellationClient;
use charcoal::discovery::candidate::CandidateSource;
use charcoal::discovery::counting_scorer::CountingScorer;
use charcoal::discovery::{candidate, dry_run, engagement, jetstream, profile_filter, seeds};
use charcoal::toxicity::download::model_files_present;
use charcoal::toxicity::onnx::OnnxToxicityScorer;

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

    /// Skip engagement stratification; stop after the viability filter.
    #[arg(long)]
    skip_engagement: bool,

    /// Recent posts to examine per candidate when measuring engagement.
    #[arg(long, default_value_t = 50)]
    max_posts: usize,

    /// Also count likes when measuring engagement (affects A, not Q).
    #[arg(long)]
    include_likes: bool,

    /// Also detect drive-by replies when measuring engagement (more accurate Q,
    /// but API-heavy: a thread fetch per post plus the candidate's follow graph).
    #[arg(long)]
    include_replies: bool,

    /// Concurrent candidates to measure during engagement stratification.
    #[arg(long, default_value_t = 4)]
    concurrency: usize,

    /// Run the instrumented dry-run instead of engagement stratification: drive
    /// the real scoring pipeline with a counting scorer to tally would-be
    /// Zentropi calls. Requires the ONNX model (`charcoal download-model`) and
    /// makes NO Zentropi calls.
    #[arg(long)]
    dry_run: bool,

    /// Followers to score per quote/reply amplifier during the dry run.
    #[arg(long, default_value_t = 50)]
    max_followers: usize,
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

    // --skip-engagement: emit the filtered survivors and stop here.
    if cli.skip_engagement {
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
        return Ok(());
    }

    // Stage 4 (opt-in): instrumented dry-run. Drives the real scoring pipeline
    // with a CountingScorer to tally would-be Zentropi calls. Replaces the
    // engagement stage when set. Requires the ONNX model; makes no Zentropi calls.
    if cli.dry_run {
        if !model_files_present(&config.model_dir) {
            anyhow::bail!(
                "ONNX model files not found at {}. Run `charcoal download-model` first — \
                 the dry run needs ONNX to replicate the clean-pass gate.",
                config.model_dir.display()
            );
        }
        let model_dir = config.model_dir.clone();
        let onnx = tokio::task::spawn_blocking(move || OnnxToxicityScorer::load(&model_dir))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking panicked loading ONNX: {e}"))??;
        let scorer = CountingScorer::new(Box::new(onnx));

        let constellation = ConstellationClient::new(&config.constellation_url)?;
        let opts = dry_run::DryRunOptions {
            engagement: engagement::EngagementOptions {
                max_posts: cli.max_posts,
                include_likes: cli.include_likes,
                include_replies: cli.include_replies,
            },
            max_followers: cli.max_followers,
            concurrency: cli.concurrency.max(1),
        };

        eprintln!(
            "Dry-run scanning {} viable candidates (max_followers={}, follower_concurrency={}) — no Zentropi calls…",
            report.kept.len(),
            cli.max_followers,
            cli.concurrency.max(1)
        );

        // Candidates run sequentially: each shares the one ONNX scorer and does
        // its own internal follower concurrency, which bounds load and keeps
        // per-candidate attribution exact.
        let mut results: Vec<dry_run::CandidateDryRun> = Vec::with_capacity(report.kept.len());
        for s in &report.kept {
            let r = dry_run::dry_run_candidate(
                &client,
                &constellation,
                &scorer,
                &s.did,
                &s.handle,
                &opts,
            )
            .await;
            eprintln!(
                "  @{}  Q={}  scored={}  would-be Zentropi={}",
                r.handle,
                r.fanout_amplifiers,
                r.amplifiers_scored + r.followers_scored,
                r.counts.zentropi_calls
            );
            results.push(r);
        }

        let total_zentropi: u64 = results.iter().map(|r| r.counts.zentropi_calls).sum();
        let total_classified: u64 = results.iter().map(|r| r.counts.posts_classified).sum();
        let total_cleared: u64 = results.iter().map(|r| r.counts.posts_cleared).sum();
        let total_accounts: usize = results
            .iter()
            .map(|r| r.amplifiers_scored + r.followers_scored)
            .sum();
        let clean_pass_rate = if total_classified > 0 {
            total_cleared as f64 / total_classified as f64
        } else {
            0.0
        };

        eprintln!();
        eprintln!("Dry-run totals across {} candidates:", results.len());
        eprintln!("  accounts scored:         {total_accounts}");
        eprintln!("  posts classified:        {total_classified}");
        eprintln!("  ONNX clean-pass rate:    {:.1}%", clean_pass_rate * 100.0);
        eprintln!("  would-be Zentropi calls: {total_zentropi}");
        eprintln!();

        if cli.json {
            println!("{}", serde_json::to_string_pretty(&results)?);
        } else {
            for r in &results {
                println!(
                    "{}\t@{}\tQ={}\taccounts={}\tzentropi={}\t{}",
                    r.did,
                    r.handle,
                    r.fanout_amplifiers,
                    r.amplifiers_scored + r.followers_scored,
                    r.counts.zentropi_calls,
                    r.stratum.as_str(),
                );
            }
        }
        return Ok(());
    }

    // Stage 3: engagement stratification — measure each survivor's cost driver
    // (distinct quote/reply amplifiers Q) via free Constellation/reply reads.
    let constellation = ConstellationClient::new(&config.constellation_url)?;
    let opts = engagement::EngagementOptions {
        max_posts: cli.max_posts,
        include_likes: cli.include_likes,
        include_replies: cli.include_replies,
    };
    let concurrency = cli.concurrency.max(1);
    eprintln!(
        "Measuring engagement for {} viable candidates (posts={}, likes={}, replies={}, concurrency={})…",
        report.kept.len(),
        cli.max_posts,
        cli.include_likes,
        cli.include_replies,
        concurrency
    );

    let client_ref = &client;
    let constellation_ref = &constellation;
    let opts_ref = &opts;
    let profiles: Vec<engagement::EngagementProfile> = stream::iter(report.kept.iter())
        .map(|s| async move {
            engagement::collect_engagement(
                client_ref,
                constellation_ref,
                &s.did,
                &s.handle,
                opts_ref,
            )
            .await
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;

    // Stratum histogram — the headline distribution for the estimate.
    let mut hist: BTreeMap<&str, usize> = BTreeMap::new();
    for p in &profiles {
        *hist.entry(p.stratum.as_str()).or_insert(0) += 1;
    }
    eprintln!();
    eprintln!("Engagement strata (by distinct quote/reply amplifiers Q):");
    for stratum in ["none", "low", "medium", "high", "viral"] {
        eprintln!(
            "  {:<7} {}",
            stratum,
            hist.get(stratum).copied().unwrap_or(0)
        );
    }
    eprintln!();

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&profiles)?);
    } else {
        for p in &profiles {
            println!(
                "{}\t@{}\tA={}\tQ={}\t{}",
                p.did,
                p.handle,
                p.total_amplifiers,
                p.fanout_amplifiers,
                p.stratum.as_str(),
            );
        }
    }

    Ok(())
}
