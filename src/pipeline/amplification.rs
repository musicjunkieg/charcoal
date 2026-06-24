// Amplification response pipeline: detect event -> score followers.
//
// This is the main threat detection workflow (Mode 1). When someone quotes
// or reposts the protected user's content, this pipeline:
// 1. Receives events from Constellation backlink queries
// 2. Records them in the database
// 3. Fetches the amplifier's follower list
// 4. Scores each follower for toxicity and topic overlap
// 5. Stores the results for the threat report

use anyhow::Result;
use std::sync::Arc;
use tracing::{info, warn};

use std::collections::{HashMap, HashSet};

use crate::bluesky::amplification::AmplificationNotification;
use crate::bluesky::client::PublicAtpClient;
use crate::bluesky::followers;
use crate::bluesky::posts;
use crate::bluesky::relationships::GraphDistance;
use crate::db::Database;
use crate::pipeline::scan_phases::burst;
// `TwoStageToxicityScorer` impls both `ToxicityScorer` and `CleanPassScorer`
// (the gather seam). The phased pipeline needs both views plus the classifier,
// so amplification takes the concrete scorer and coerces it into the two `&dyn`
// views — same pattern as the sweep rewire (Task 6.2).
use crate::pipeline::scan_phases::gather::{AtpPostFetcher, CleanPassScorer};
use crate::pipeline::scan_phases::{run_phased_scan, CandidateInput, PhasedScanDeps};
use crate::scoring::nli::NliScorer;
use crate::scoring::threat::ThreatWeights;
use crate::topics::embeddings::SentenceEmbedder;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::ensemble::TwoStageToxicityScorer;
use crate::toxicity::traits::ToxicityScorer;

/// Run the amplification detection pipeline.
///
/// Processes pre-fetched amplification events (from Constellation backlinks),
/// fetches amplifier followers, and scores them. Returns
/// `(events_processed, accounts_scored, degraded)` — `degraded` is true when the
/// scan is incomplete: either the cost ceiling was hit, or one or more accounts
/// were skipped due to fetch/score errors. Re-run to resume.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    client: &PublicAtpClient,
    // `None` when scanning without `--analyze`: the old NoopScorer always errored
    // on every call, so no accounts were scored. We preserve that by skipping the
    // phased scoring path entirely when there is no real scorer.
    scorer: Option<&TwoStageToxicityScorer>,
    db: &Arc<dyn Database>,
    user_did: &str,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    protected_handle: &str,
    analyze_followers: bool,
    max_followers_per_amplifier: usize,
    concurrency: usize,
    embedder: Option<&SentenceEmbedder>,
    protected_embedding: Option<&[f64]>,
    events: Vec<AmplificationNotification>,
    median_engagement: f64,
    pile_on_dids: &std::collections::HashSet<String>,
    original_text_cache: &std::collections::HashMap<String, String>,
    nli_scorer: Option<&NliScorer>,
    protected_posts_with_embeddings: Option<&[(String, Vec<f64>)]>,
    data_dir: Option<&std::path::Path>,
    graph_distances: &HashMap<String, GraphDistance>,
) -> Result<(usize, usize, bool)> {
    info!(
        total_events = events.len(),
        "Processing amplification events"
    );

    // Record the scan timestamp
    db.set_scan_state(
        user_did,
        "last_scan_at",
        &chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    )
    .await?;

    // Store each event in the database, fetching quote text when available.
    // Look up original post text from the cache for all event types.
    for event in &events {
        let mut amplifier_text: Option<String> = None;
        let mut quote_toxicity: Option<f64> = None;

        // Look up the original (protected user's) post text from the cache
        // (resolved before scoring so the ensemble scorer can use it as context)
        let original_post_text = event
            .original_post_uri
            .as_deref()
            .and_then(|uri| original_text_cache.get(uri))
            .map(|s| s.as_str());

        // For quote and reply events, fetch the amplifier's text UNCONDITIONALLY
        // so it is persisted as event evidence (`insert_amplification_event`)
        // even on a non-`--analyze` scan. Only the SCORING of that text is
        // gated on a real scorer being present.
        if event.event_type == "quote" || event.event_type == "reply" {
            match posts::fetch_post_text(client, &event.amplifier_post_uri).await {
                Ok(Some(text)) => {
                    // Score only when a real scorer is present (i.e. `--analyze`).
                    if let Some(scorer) = scorer {
                        match scorer.score_with_context(&text, original_post_text).await {
                            Ok(result) => {
                                quote_toxicity = Some(result.toxicity);
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to score amplifier text");
                            }
                        }
                    }
                    amplifier_text = Some(text);
                }
                Ok(None) => {
                    info!(
                        uri = event.amplifier_post_uri,
                        "Amplifier post text not found"
                    );
                }
                Err(e) => {
                    warn!(error = %e, "Failed to fetch amplifier post text");
                }
            }
        }

        // Score the interaction pair via NLI when both texts are available
        let context_score = match (nli_scorer, amplifier_text.as_deref(), original_post_text) {
            (Some(nli), Some(amp_text), Some(orig_text)) => {
                match nli.score_pair(orig_text, amp_text).await {
                    Ok((score, hypothesis_scores)) => {
                        info!(
                            handle = event.amplifier_handle,
                            context_score = format!("{:.3}", score),
                            "NLI scored event pair"
                        );
                        if let Some(dir) = data_dir {
                            let audit_event = crate::scoring::audit_log::AuditEvent::nli(
                                crate::scoring::audit_log::NliFields {
                                    target_did: event.amplifier_did.clone(),
                                    target_handle: event.amplifier_handle.clone(),
                                    pair_type: "direct".to_string(),
                                    original_text: orig_text.to_string(),
                                    response_text: amp_text.to_string(),
                                    hypothesis_scores,
                                    hostility_score: score,
                                    similarity: None,
                                },
                            );
                            match crate::scoring::audit_log::AuditWriter::from_env(
                                dir,
                                crate::scoring::audit_log::EventKind::Nli,
                            ) {
                                Ok(writer) => {
                                    if let Err(e) = writer.record(audit_event) {
                                        warn!(error = %e, "Failed to write NLI audit JSONL");
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, "Failed to init NLI audit writer");
                                }
                            }
                        }
                        Some(score)
                    }
                    Err(e) => {
                        warn!(error = %e, "NLI scoring failed for event pair");
                        None
                    }
                }
            }
            _ => None,
        };

        db.insert_amplification_event(
            user_did,
            &event.event_type,
            &event.amplifier_did,
            &event.amplifier_handle,
            event.original_post_uri.as_deref().unwrap_or("unknown"),
            Some(&event.amplifier_post_uri),
            amplifier_text.as_deref(),
            original_post_text,
            context_score,
        )
        .await?;

        let event_label = match event.event_type.as_str() {
            "quote" => "Quote",
            "repost" => "Repost",
            "like" => "Like",
            "reply" => "Reply",
            other => other,
        };
        println!(
            "  {} by @{} ({})",
            event_label, event.amplifier_handle, event.indexed_at,
        );
        if let Some(ref text) = amplifier_text {
            let preview = crate::output::truncate_chars(text, 120);
            let tox_str = quote_toxicity
                .map(|t| format!(" [tox: {:.2}]", t))
                .unwrap_or_default();
            println!("    \"{}\"{}", preview, tox_str);
        }
    }

    // Phase B: Score amplifiers and (optionally) their followers via the
    // three-phase `run_phased_scan` orchestrator (#208).
    //
    // Both the old amplifier-scoring loop and the old per-follower two-pass
    // `build_profile` loop are replaced by a single candidate set fed through
    // one combined phased scan — one burst window, one `scan_phase` marker, one
    // clean resume point. The two-pass NLI gate the followers used to run
    // inline (raw>=8.0 → re-score with NLI) now lives in `finalize_account`,
    // selected per candidate by `direct_pairs`: amplifiers carry
    // `direct_pairs=Some(...)` (always NLI), followers carry `direct_pairs=None`
    // (NLI gated at raw>=8.0). The event-recording loop above is unchanged —
    // it is the Phase-A-time ONNX/NLI step and does not involve the classifier.
    //
    // Dedup precedence: an account that is both an amplifier and a follower is
    // scored ONCE as an amplifier (matching the old order, where amplifiers
    // were scored first and the follower `is_score_stale` check then skipped the
    // re-score). We key the candidate set on DID and never overwrite an
    // amplifier entry with a follower one.
    let mut candidates: Vec<CandidateInput> = Vec::new();
    let mut seen_dids: HashSet<String> = HashSet::new();

    // ── Amplifier candidates ──
    let mut amplifier_handles: HashMap<String, String> = HashMap::new();
    for event in &events {
        amplifier_handles
            .entry(event.amplifier_did.clone())
            .or_insert_with(|| event.amplifier_handle.clone());
    }

    let amplifier_count = amplifier_handles.len();
    if amplifier_count > 0 {
        println!("\nScoring {} amplifiers…", amplifier_count);

        for (did, handle) in &amplifier_handles {
            // Skip the protected user themselves. Match on BOTH handle and DID:
            // handles can change, so the DID is the stable identity check.
            if handle == protected_handle || did == user_did {
                continue;
            }
            if !db.is_score_stale(user_did, did, 7).await.unwrap_or(true) {
                continue;
            }

            // Gather direct text pairs from stored events, deduplicating
            // across scans (the same event can be recorded multiple times)
            let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
            let mut pairs: Vec<(String, String)> = Vec::new();
            match db.get_events_by_amplifier(user_did, did).await {
                Ok(db_events) => {
                    for ev in db_events {
                        if let (Some(orig), Some(amp)) = (ev.original_post_text, ev.amplifier_text)
                        {
                            if !orig.is_empty()
                                && !amp.is_empty()
                                && seen_pairs.insert((orig.clone(), amp.clone()))
                            {
                                pairs.push((orig, amp));
                            }
                        }
                    }
                }
                // Continue on error (matching prior behaviour) but make the
                // dropped pairs visible instead of swallowing the failure.
                Err(e) => {
                    warn!(
                        amplifier_did = %did,
                        error = %e,
                        "Failed to load stored events for amplifier; direct NLI pairs dropped"
                    );
                }
            }

            if seen_dids.insert(did.clone()) {
                candidates.push(CandidateInput {
                    account_did: did.clone(),
                    account_handle: handle.clone(),
                    is_pile_on: pile_on_dids.contains(did),
                    direct_pairs: Some(pairs),
                    graph_distance: graph_distances.get(did).copied(),
                });
            }
        }
    }

    // ── Follower candidates (only when --analyze is set) ──
    //
    // Quotes and replies are direct hostile engagement vectors that warrant
    // follower analysis. Reposts and likes are recorded but don't trigger
    // follower analysis — reposts are usually supportive sharing, and likes
    // are low-signal engagement.
    if analyze_followers && !events.is_empty() {
        let scorable_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "quote" || e.event_type == "reply")
            .collect();
        let skipped_count = events.len() - scorable_events.len();

        if skipped_count > 0 {
            info!(
                skipped = skipped_count,
                "Skipping follower analysis for reposts/likes"
            );
            println!(
                "  Skipping {} reposts/likes (follower analysis is quote/reply-only)",
                skipped_count
            );
        }

        if scorable_events.is_empty() {
            info!("No quote/reply events to analyze");
        }

        for event in &scorable_events {
            println!("\nFetching followers of @{}...", event.amplifier_handle);

            match followers::fetch_followers(
                client,
                &event.amplifier_handle,
                max_followers_per_amplifier,
            )
            .await
            {
                Ok(follower_list) => {
                    // Filter — find followers with stale scores. Exclude the
                    // protected user from their own threat report, and skip any
                    // DID already queued (amplifier entries take precedence).
                    let mut added = 0usize;
                    for f in follower_list
                        .iter()
                        // Exclude the protected user by both handle and DID —
                        // handles can change, so the DID is the stable identity
                        // check (mirrors the amplifier-path exclusion above).
                        .filter(|f| f.handle != protected_handle && f.did != user_did)
                    {
                        if !db.is_score_stale(user_did, &f.did, 7).await.unwrap_or(true) {
                            continue;
                        }
                        if seen_dids.insert(f.did.clone()) {
                            candidates.push(CandidateInput {
                                account_did: f.did.clone(),
                                account_handle: f.handle.clone(),
                                is_pile_on: pile_on_dids.contains(&f.did),
                                direct_pairs: None,
                                graph_distance: None,
                            });
                            added += 1;
                        }
                    }

                    println!(
                        "  Found {} followers, {} need scoring ({} concurrent)...",
                        follower_list.len(),
                        added,
                        concurrency,
                    );
                }
                Err(e) => {
                    warn!(
                        handle = event.amplifier_handle,
                        error = %e,
                        "Failed to fetch followers, skipping"
                    );
                }
            }
        }
    }

    // ── Run the combined phased scan over amplifier + follower candidates ──
    //
    // Resilience note: the old per-account scoring wrapped each `build_profile`
    // in `catch_unwind` to isolate a per-account panic. The phased pipeline's
    // model is different (and stronger): a crash or cost-cap mid-run is
    // recoverable by re-running, which resumes from the DB-staged `scan_phase`
    // marker rather than re-scoring everything — the intended #208 architecture.
    let (accounts_scored, degraded) = match scorer {
        // No real scorer (scan without `--analyze`): the old NoopScorer errored
        // on every `build_profile`, so no accounts were ever scored. Preserve
        // that by skipping the phased scan entirely.
        None => (0, false),
        Some(_) if candidates.is_empty() => (0, false),
        Some(scorer) => {
            let fetcher = AtpPostFetcher { client };
            let classifier = scorer.classifier();

            let deps = PhasedScanDeps {
                fetcher: &fetcher,
                scorer: scorer as &dyn ToxicityScorer,
                clean_pass: scorer as &dyn CleanPassScorer,
                classifier: &classifier,
                protected_fingerprint,
                weights,
                embedder,
                protected_embedding,
                // Amplifiers use direct_pairs (Mode-A precedence in finalize), so
                // they ignore ppwe; followers gate on raw>=8.0 and use ppwe. Both
                // the NLI scorer and protected-post embeddings are threaded through
                // for the follower path.
                nli_scorer,
                protected_posts_with_embeddings,
                data_dir,
                median_engagement,
                gather_concurrency: concurrency,
                burst_concurrency: burst::burst_concurrency(),
                burst_batch: burst::burst_batch(),
            };

            let summary = run_phased_scan(db, user_did, &candidates, &deps).await?;
            (summary.accounts_scored, summary.degraded)
        }
    };

    Ok((events.len(), accounts_scored, degraded))
}
