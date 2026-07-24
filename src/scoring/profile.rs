// Profile builder — orchestrates scoring for a single account.
//
// Given a target account, this module:
// 1. Fetches their recent posts
// 2. Runs toxicity scoring on those posts
// 3. Builds their topic fingerprint
// 4. Computes topic overlap with the protected user
// 5. Calculates the combined threat score
// 6. Returns a complete AccountScore ready for storage

use anyhow::Result;
use tracing::{info, warn};

use crate::bluesky::client::PublicAtpClient;
use crate::bluesky::posts::{self, FingerprintQuality, Post};
use crate::bluesky::relationships::GraphDistance;
use crate::db::models::{AccountScore, ThreatTier, ToxicPost};
use crate::scoring::behavioral;
use crate::scoring::nli::NliScorer;
use crate::scoring::threat::{self, ThreatWeights};
use crate::topics::embeddings::{self, SentenceEmbedder};
use crate::topics::fingerprint::TopicFingerprint;
use crate::topics::overlap;
use crate::topics::tfidf::TfIdfExtractor;
use crate::topics::traits::TopicExtractor;
use crate::toxicity::traits::{BinaryVerdict, ToxicityScorer};

/// Result of the Stage 1 quick check.
///
/// Stage 1 fetches a small (25-post) sample and decides whether the account is
/// obviously a non-threat (too few posts to score, or clean-and-irrelevant). In
/// those cases it produces a terminal [`AccountScore`] and the full pipeline is
/// skipped. Otherwise it signals `Proceed`, carrying any Stage-1-computed value
/// the caller may want for logging.
///
/// Note: the only Stage-1 value carried forward is `stage1_overlap` (the cheap
/// TF-IDF overlap against the 25-post sample). The Stage 2 path deliberately
/// recomputes overlap and fingerprint quality from the larger 50-post sample —
/// it does **not** reuse Stage 1's values for the scoring math — so `Proceed`
/// only needs to surface `stage1_overlap` for diagnostics, not for correctness.
pub enum Stage1Outcome {
    /// Account fully scored at Stage 1 — return this score, skip Stage 2.
    /// Boxed because `AccountScore` is large relative to the `Proceed` variant.
    Terminal(Box<AccountScore>),
    /// Account survived the early-exit gate — run the full Stage 2 pipeline.
    Proceed {
        /// Cheap TF-IDF overlap from the 25-post sample (`None` if extraction
        /// failed). Carried for diagnostics; Stage 2 recomputes its own overlap.
        stage1_overlap: Option<f64>,
    },
}

/// Build the terminal `AccountScore` for an account whose posts our English-only
/// models cannot assess (#222). Shared by the Stage-1 and both Stage-2 seams.
pub(crate) fn not_assessed_score(
    did: &str,
    handle: &str,
    posts_analyzed: u32,
    graph_distance: Option<GraphDistance>,
) -> AccountScore {
    AccountScore {
        did: did.to_string(),
        handle: handle.to_string(),
        toxicity_score: None,
        topic_overlap: None,
        threat_score: None,
        threat_tier: Some(ThreatTier::NotAssessed.as_str().to_string()),
        posts_analyzed,
        top_toxic_posts: vec![],
        scored_at: String::new(),
        behavioral_signals: None,
        context_score: None,
        graph_distance: graph_distance.map(|d| d.as_str().to_string()),
        fingerprint_quality: None,
        scoring_confidence: None,
    }
}

/// Build a complete threat profile for a single account.
///
/// This is the core scoring function. It fetches the target's posts,
/// scores them for toxicity, extracts their topics, and computes the
/// combined threat score against the protected user's fingerprint.
///
/// When `embedder` and `protected_embedding` are provided, topic overlap
/// is computed using sentence embeddings (semantic similarity). Otherwise,
/// falls back to TF-IDF keyword cosine similarity.
#[allow(clippy::too_many_arguments)]
pub async fn build_profile(
    client: &PublicAtpClient,
    scorer: &dyn ToxicityScorer,
    target_handle: &str,
    target_did: &str,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    embedder: Option<&SentenceEmbedder>,
    protected_embedding: Option<&[f64]>,
    median_engagement: f64,
    pile_on_dids: &std::collections::HashSet<String>,
    nli_scorer: Option<&NliScorer>,
    protected_posts_with_embeddings: Option<&[(String, Vec<f64>)]>,
    direct_pairs: Option<&[(String, String)]>,
    data_dir: Option<&std::path::Path>,
    graph_distance: Option<GraphDistance>,
) -> Result<AccountScore> {
    // ── Stage 1: Quick check with 25 posts ──
    // Fetch a small sample and run ONNX + TF-IDF overlap.
    // If the account is clearly clean AND topically irrelevant, exit early.
    // This catches ~50-60% of sweep accounts with minimal cost.
    let stage1_sample = posts::fetch_posts_with_replies(client, target_handle, 25).await?;

    let stage1_overlap = match stage1_outcome(
        &stage1_sample,
        scorer,
        target_handle,
        target_did,
        protected_fingerprint,
        weights,
        graph_distance,
    )
    .await?
    {
        Stage1Outcome::Terminal(score) => return Ok(*score),
        Stage1Outcome::Proceed { stage1_overlap } => stage1_overlap,
    };

    // ── Stage 2: Full pipeline with 50 posts ──
    // Account wasn't clean enough for early exit — run the full analysis.
    let sample = posts::fetch_posts_with_replies(client, target_handle, 50).await?;

    // All posts go to toxicity scoring, with per-post context for replies.
    // Originals and quotes are scored solo; replies are scored as a parent/reply
    // pair so the conversation-scoped Zentropi labeler can correctly evaluate
    // whether the reply is hostile toward the parent's author.
    let all_post_texts: Vec<String> = sample
        .originals
        .iter()
        .map(|p| p.text.clone())
        .chain(sample.replies.iter().map(|r| r.post.text.clone()))
        .chain(sample.quotes.iter().map(|p| p.text.clone()))
        .collect();

    let parent_uris: Vec<String> = sample
        .replies
        .iter()
        .map(|r| r.parent_uri.clone())
        .collect();
    let parent_texts = posts::fetch_parent_posts(client, &parent_uris).await?;

    // contexts[i] aligns with all_post_texts[i]: parent text for replies, None otherwise.
    let mut contexts: Vec<Option<String>> = Vec::with_capacity(all_post_texts.len());
    contexts.extend(std::iter::repeat_n(None, sample.originals.len()));
    for r in &sample.replies {
        contexts.push(parent_texts.get(&r.parent_uri).cloned());
    }
    contexts.extend(std::iter::repeat_n(None, sample.quotes.len()));

    // Step 3: Two-stage classification — ONNX clean-pass + Zentropi binary verdict.
    // Each verdict carries the binary `is_toxic` flag plus the underlying ONNX
    // score (for evidence sorting and audit).
    let verdicts = scorer
        .classify_batch_with_contexts(&all_post_texts, &contexts)
        .await?;

    // Precompute the pile-on flag from the caller-supplied set, matching the
    // `AccountInput` blob design — `score_from_sample` takes the bare bool.
    let pile_on = pile_on_dids.contains(target_did);

    score_from_sample(
        &sample,
        &all_post_texts,
        &contexts,
        &verdicts,
        protected_fingerprint,
        weights,
        embedder,
        protected_embedding,
        // build_profile is the monolithic (non-decoupled) path — it has no
        // Phase-A precompute, so it embeds at score time exactly as before.
        None,
        median_engagement,
        pile_on,
        nli_scorer,
        protected_posts_with_embeddings,
        direct_pairs,
        data_dir,
        graph_distance,
        target_handle,
        target_did,
        stage1_overlap,
    )
    .await
}

/// Stage 1 quick check — decide whether to early-exit or proceed to Stage 2.
///
/// Operates on an already-fetched 25-post sample (it does **not** fetch). Runs
/// the ONNX clean-pass filter over first-person posts plus a cheap TF-IDF
/// overlap, then applies [`should_early_exit_stage1`]. Returns
/// [`Stage1Outcome::Terminal`] with a fully-built terminal `AccountScore` for
/// the two non-threat cases (`< 5` posts → "Insufficient Data"; clean and
/// topically irrelevant → early-exit "Low"), or [`Stage1Outcome::Proceed`] when
/// the account warrants the full pipeline.
#[allow(clippy::too_many_arguments)]
pub async fn stage1_outcome(
    stage1_sample: &posts::PostSample,
    scorer: &dyn ToxicityScorer,
    target_handle: &str,
    target_did: &str,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    graph_distance: Option<GraphDistance>,
) -> Result<Stage1Outcome> {
    // #222: drop posts our English-only models cannot assess, and decide whether
    // the account is scoreable, unassessable, or genuinely sparse — BEFORE the
    // ONNX clean-pass, so a non-English account can't early-exit to Low.
    let (assessable_sample, dropped) =
        crate::scoring::language::partition_assessable(stage1_sample);
    match crate::scoring::language::coverage_gate(assessable_sample.total_posts, dropped) {
        crate::scoring::language::CoverageOutcome::NotAssessed => {
            return Ok(Stage1Outcome::Terminal(Box::new(not_assessed_score(
                target_did,
                target_handle,
                (assessable_sample.total_posts + dropped) as u32,
                graph_distance,
            ))));
        }
        crate::scoring::language::CoverageOutcome::InsufficientData => {
            // Falls through to the existing <5 terminal below (sparse account).
        }
        crate::scoring::language::CoverageOutcome::Score => {}
    }

    // From here down, operate on the assessable-only sample.
    let stage1_sample = &assessable_sample;

    if stage1_sample.total_posts < 5 {
        info!(
            handle = target_handle,
            post_count = stage1_sample.total_posts,
            "Insufficient posts for reliable scoring"
        );
        return Ok(Stage1Outcome::Terminal(Box::new(AccountScore {
            did: target_did.to_string(),
            handle: target_handle.to_string(),
            toxicity_score: None,
            topic_overlap: None,
            threat_score: None,
            threat_tier: Some("Insufficient Data".to_string()),
            posts_analyzed: stage1_sample.total_posts as u32,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            // Preserve the graph distance even on insufficient-data accounts —
            // it was computed by the caller and is independent of the post
            // sample. Downstream consumers (sweep ranking, UI) want it.
            graph_distance: graph_distance.map(|d| d.as_str().to_string()),
            fingerprint_quality: None,
            scoring_confidence: None,
        })));
    }

    // Quick ONNX scores for clean-pass check.
    //
    // Originals + quotes are scored solo against ONNX_CLEAN_THRESHOLD — those
    // are first-person posts and ONNX in isolation is a reliable "obviously
    // clean" filter for them. Reply texts in isolation are NOT reliable: a
    // benign-looking "I agree" only becomes hostile in conversation context,
    // and stage 1 has no parent texts available. Excluding replies from the
    // early-exit decision means reply-context-dependent toxicity makes it to
    // stage 2 where Zentropi can do pair classification with parent text.
    let stage1_texts: Vec<String> = stage1_sample
        .originals
        .iter()
        .map(|p| p.text.clone())
        .chain(stage1_sample.replies.iter().map(|r| r.post.text.clone()))
        .chain(stage1_sample.quotes.iter().map(|p| p.text.clone()))
        .collect();
    let stage1_onnx = scorer.score_batch(&stage1_texts).await?;
    let originals_count = stage1_sample.originals.len();
    let quotes_offset = originals_count + stage1_sample.replies.len();
    let stage1_clean_pass_scores: Vec<f64> = stage1_onnx
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            // Keep only originals (indices 0..originals_count) and quotes
            // (indices quotes_offset..). Skip replies (the middle range).
            if i < originals_count || i >= quotes_offset {
                Some(r.toxicity)
            } else {
                None
            }
        })
        .collect();

    // Preliminary topic overlap via TF-IDF (cheap, always available)
    let stage1_fp_texts: Vec<String> = if stage1_sample.originals.len() >= 15 {
        stage1_sample
            .originals
            .iter()
            .map(|p| p.text.clone())
            .collect()
    } else {
        stage1_texts.clone()
    };
    let stage1_overlap: Option<f64> = {
        let topic_extractor = TfIdfExtractor {
            top_n_keywords: 40,
            max_clusters: 7,
        };
        match topic_extractor.extract(&stage1_fp_texts) {
            Ok(fp) => Some(overlap::cosine_similarity(protected_fingerprint, &fp)),
            // TF-IDF extraction failed (e.g. no usable tokens). Treat overlap as
            // unknown rather than 0.0 — the prior `Err => 0.0` path inverted the
            // intent in the comment and let extraction failures slip through the
            // early-exit gate as if the account were topically irrelevant.
            Err(_) => None,
        }
    };

    // Early exit: all ONNX scores clean AND topic overlap below gate.
    // When overlap is unknown (extraction failed), do not early-exit.
    if should_early_exit_stage1(
        &stage1_clean_pass_scores,
        stage1_overlap,
        weights.overlap_gate_threshold,
    ) {
        info!(
            handle = target_handle,
            posts = stage1_sample.total_posts,
            overlap = format!("{:.3}", stage1_overlap.unwrap_or(0.0)),
            "Stage 1 early exit: clean and topically irrelevant"
        );

        let fp_quality = FingerprintQuality::from_counts(
            stage1_sample.originals.len(),
            stage1_sample.replies.len() + stage1_sample.quotes.len(),
        );

        return Ok(Stage1Outcome::Terminal(Box::new(AccountScore {
            did: target_did.to_string(),
            handle: target_handle.to_string(),
            toxicity_score: Some(0.0),
            topic_overlap: stage1_overlap,
            threat_score: Some(0.0),
            threat_tier: Some("Low".to_string()),
            posts_analyzed: stage1_sample.total_posts as u32,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            // Preserve the caller-computed graph distance — it's independent
            // of the post sample and downstream ranking still needs it.
            graph_distance: graph_distance.map(|d| d.as_str().to_string()),
            fingerprint_quality: Some(fp_quality.as_str().to_string()),
            scoring_confidence: Some("low".to_string()),
        })));
    }

    Ok(Stage1Outcome::Proceed { stage1_overlap })
}

/// Score an account from an already-fetched Stage 2 sample and its classifier
/// verdicts — the full pipeline *after* `classify_batch_with_contexts`.
///
/// This function does **not** fetch and does **not** call the classifier. It
/// takes the 50-post `stage2_sample`, the flattened `all_post_texts` (originals
/// ++ reply texts ++ quotes, in that order), the per-post `contexts` (parent
/// text for replies, `None` otherwise), and the aligned `verdicts`, then runs:
/// reply-weighted toxicity → topic overlap → behavioral signals → context/NLI
/// two-pass gate → graph distance → tier → final `AccountScore`.
///
/// `pile_on` is the precomputed `pile_on_dids.contains(target_did)` bool (the
/// caller owns the set). `stage1_overlap` is accepted for parity with the
/// staged-scan blob design but is not used by the scoring math — overlap and
/// fingerprint quality are recomputed here from the 50-post sample, identically
/// to the original monolithic `build_profile`.
/// Select the posts used to build an account's topic fingerprint / target
/// embedding from its Stage-2 `PostSample`.
///
/// Prefers originals (chosen topics, not inherited) when there are enough of
/// them (≥ 15); otherwise falls back to all posts — originals ++ reply texts
/// ++ quote texts, in that order.
///
/// This is the single source of truth for the selection. Phase C
/// (`score_from_sample`) uses it to build the fingerprint, and Phase A
/// (`gather`) uses it to feed the embedder when it precomputes the target
/// vector, so the two phases can never diverge on which posts represent the
/// account (#213).
pub fn select_fingerprint_posts(sample: &posts::PostSample) -> Vec<String> {
    if sample.originals.len() >= 15 {
        sample.originals.iter().map(|p| p.text.clone()).collect()
    } else {
        sample
            .originals
            .iter()
            .map(|p| p.text.clone())
            .chain(sample.replies.iter().map(|r| r.post.text.clone()))
            .chain(sample.quotes.iter().map(|p| p.text.clone()))
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn score_from_sample(
    stage2_sample: &posts::PostSample,
    all_post_texts: &[String],
    contexts: &[Option<String>],
    verdicts: &[BinaryVerdict],
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    embedder: Option<&SentenceEmbedder>,
    protected_embedding: Option<&[f64]>,
    // Target mean embedding precomputed in Phase A (gather), where the ONNX
    // work overlaps I/O instead of serializing in Phase C. When `Some`, the
    // overlap step uses it directly and never touches `embedder` — the whole
    // point of #213. `None` falls back to embedding here (old blobs) or TF-IDF.
    precomputed_target_embedding: Option<&[f64]>,
    median_engagement: f64,
    pile_on: bool,
    nli_scorer: Option<&NliScorer>,
    protected_posts_with_embeddings: Option<&[(String, Vec<f64>)]>,
    direct_pairs: Option<&[(String, String)]>,
    data_dir: Option<&std::path::Path>,
    graph_distance: Option<GraphDistance>,
    target_handle: &str,
    target_did: &str,
    _stage1_overlap: Option<f64>,
) -> Result<AccountScore> {
    let sample = stage2_sample;
    let _ = contexts; // contexts were consumed by the (already-run) classifier.

    // Step 2: Determine fingerprint quality and select posts for fingerprinting
    let fp_quality = FingerprintQuality::from_counts(
        sample.originals.len(),
        sample.replies.len() + sample.quotes.len(),
    );

    // Fingerprinting uses originals when available (chosen topics, not inherited).
    // Extracted into `select_fingerprint_posts` so Phase A (gather) can feed the
    // embedder EXACTLY these posts when it precomputes the target vector — one
    // shared selection makes producer/consumer divergence impossible (#213).
    let fingerprint_posts: Vec<String> = select_fingerprint_posts(sample);

    let all_posts_flat: Vec<&Post> = sample
        .originals
        .iter()
        .chain(sample.replies.iter().map(|r| &r.post))
        .chain(sample.quotes.iter())
        .collect();

    // Reply-weighted binary toxicity rate. Replies count 70% (where harassment
    // manifests), originals 30% (where stated views show). Quotes are bucketed
    // with originals — they are first-person commentary, not a reply pair.
    let originals_len = sample.originals.len();
    let replies_len = sample.replies.len();
    let quotes_len = sample.quotes.len();

    // Fail loud on misalignment instead of panicking on an out-of-bounds slice
    // (or silently mis-scoring). The verdicts and post texts must be 1:1 with
    // the originals+replies+quotes the sample contains, in that order.
    let expected_len = originals_len + replies_len + quotes_len;
    anyhow::ensure!(
        verdicts.len() == all_post_texts.len(),
        "Stage-2 misalignment: {} verdicts vs {} post texts",
        verdicts.len(),
        all_post_texts.len(),
    );
    anyhow::ensure!(
        verdicts.len() == expected_len,
        "Stage-2 misalignment: {} verdicts vs {} expected (originals {} + replies {} + quotes {})",
        verdicts.len(),
        expected_len,
        originals_len,
        replies_len,
        quotes_len,
    );

    let originals_verdicts = &verdicts[..originals_len];
    let replies_verdicts = &verdicts[originals_len..originals_len + replies_len];
    let quotes_verdicts = &verdicts[originals_len + replies_len..];

    let toxic_replies = replies_verdicts.iter().filter(|v| v.is_toxic).count();
    let toxic_originals = originals_verdicts.iter().filter(|v| v.is_toxic).count()
        + quotes_verdicts.iter().filter(|v| v.is_toxic).count();
    let total_originals = originals_len + quotes_len;
    let avg_toxicity = compute_reply_weighted_toxicity(
        toxic_replies,
        replies_len,
        toxic_originals,
        total_originals,
    );

    // Evidence: surface the worst-flagged posts (Zentropi-toxic, ranked by ONNX
    // score). When no posts are flagged, surface the top-3 highest-ONNX posts as
    // a "watchlist" so users still see *something* explanatory.
    let toxic_evidence: Vec<(&Post, f64)> = all_posts_flat
        .iter()
        .zip(verdicts.iter())
        .filter(|(_, v)| v.is_toxic)
        .map(|(p, v)| (*p, v.onnx_score))
        .collect();

    let evidence_pool: Vec<(&Post, f64)> = if !toxic_evidence.is_empty() {
        toxic_evidence
    } else {
        all_posts_flat
            .iter()
            .zip(verdicts.iter())
            .map(|(p, v)| (*p, v.onnx_score))
            .collect()
    };

    let mut scored_posts = evidence_pool;
    scored_posts.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let top_toxic_posts: Vec<ToxicPost> = scored_posts
        .iter()
        .take(3)
        .map(|(post, score)| ToxicPost {
            text: post.text.clone(),
            toxicity: *score,
            uri: post.uri.clone(),
        })
        .collect();

    // Step 3: Compute topic overlap with the protected user.
    //
    // Prefer sentence embeddings when available — they capture semantic
    // similarity ("fatphobia" ≈ "obesity") that keyword matching misses.
    // Fall back to TF-IDF keyword cosine when the embedding model isn't loaded.
    let topic_overlap = if let (Some(precomputed), Some(protected_emb)) =
        (precomputed_target_embedding, protected_embedding)
    {
        // Precomputed path (#213): Phase A already embedded `fingerprint_posts`
        // (the identical selection, via `select_fingerprint_posts`) and averaged
        // them, so the cosine here is byte-identical to the embed-at-finalize
        // path below — only the (expensive, mutex-serialized) embedding moved to
        // Phase A where it overlaps I/O.
        embeddings::cosine_similarity_embeddings(protected_emb, precomputed)
    } else if let (Some(emb), Some(protected_emb)) = (embedder, protected_embedding) {
        // Embedding path: embed target's posts, average, compare
        let target_embeddings = emb.embed_batch(&fingerprint_posts).await?;
        let target_mean = embeddings::mean_embedding(&target_embeddings);
        embeddings::cosine_similarity_embeddings(protected_emb, &target_mean)
    } else {
        // Fallback: TF-IDF keyword cosine similarity
        let topic_extractor = TfIdfExtractor {
            top_n_keywords: 40,
            max_clusters: 7,
        };
        let target_fingerprint = topic_extractor.extract(&fingerprint_posts)?;
        overlap::cosine_similarity(protected_fingerprint, &target_fingerprint)
    };

    // Step 4b: Compute behavioral signals (from PostSample — no separate API call)
    let quote_ratio = sample.quote_ratio;
    let reply_ratio = sample.reply_ratio;

    let avg_engagement = behavioral::compute_avg_engagement_refs(&all_posts_flat);
    // `pile_on` is supplied precomputed by the caller (the pile-on DID set is
    // owned upstream, matching the staged-scan blob design).

    // Step 5: Compute context score via NLI
    //
    // Two modes:
    // - Direct pairs (amplifiers): NLI-score the actual event texts
    // - Inferred pairs (followers): find top 3 most similar posts by embedding
    let context_score = if let Some(nli) = nli_scorer {
        if let Some(pairs) = direct_pairs {
            // Mode A: Direct pairs — score each real interaction
            if pairs.is_empty() {
                None
            } else {
                let mut pair_scores = Vec::new();
                for (original, response) in pairs {
                    match nli.score_pair(original, response).await {
                        Ok((score, hypothesis_scores)) => {
                            pair_scores.push(score);
                            info!(
                                target_did = target_did,
                                target_handle = target_handle,
                                pair_type = "direct",
                                hostility_score = format!("{:.3}", score),
                                "NLI audit"
                            );
                            if let Some(dir) = data_dir {
                                let event = crate::scoring::audit_log::AuditEvent::nli(
                                    crate::scoring::audit_log::NliFields {
                                        target_did: target_did.to_string(),
                                        target_handle: target_handle.to_string(),
                                        pair_type: "direct".to_string(),
                                        original_text: original.to_string(),
                                        response_text: response.to_string(),
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
                                        if let Err(e) = writer.record(event) {
                                            warn!(error = %e, "Failed to write NLI audit JSONL");
                                        }
                                    }
                                    Err(e) => {
                                        warn!(error = %e, "Failed to init NLI audit writer");
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "NLI scoring failed for direct pair");
                        }
                    }
                }
                crate::scoring::nli::avg_context_score(&pair_scores)
            }
        } else if let (Some(emb), Some(user_posts)) = (embedder, protected_posts_with_embeddings) {
            // Mode B: Inferred pairs — embedding-matched (existing logic)
            if user_posts.is_empty() {
                None
            } else {
                match emb.embed_batch(all_post_texts).await {
                    Ok(target_embeddings) => {
                        let target_with_emb: Vec<(String, Vec<f64>)> = all_post_texts
                            .iter()
                            .zip(target_embeddings)
                            .map(|(text, emb)| (text.clone(), emb))
                            .collect();

                        let user_mean: Vec<f64> = {
                            let dim = user_posts[0].1.len();
                            let mut mean = vec![0.0; dim];
                            for (_, emb) in user_posts {
                                for (i, v) in emb.iter().enumerate() {
                                    mean[i] += v;
                                }
                            }
                            let n = user_posts.len() as f64;
                            mean.iter_mut().for_each(|v| *v /= n);
                            mean
                        };
                        let top_target_posts = crate::scoring::context::find_most_similar_posts(
                            &user_mean,
                            &target_with_emb,
                            3,
                        );

                        let mut pair_scores = Vec::new();
                        for (target_text, similarity) in &top_target_posts {
                            let target_emb = target_with_emb
                                .iter()
                                .find(|(t, _)| t == target_text)
                                .map(|(_, e)| e.as_slice());

                            let user_text = target_emb.and_then(|emb| {
                                crate::scoring::context::find_best_matching_user_post(
                                    emb, user_posts,
                                )
                            });

                            let original = user_text.as_deref().unwrap_or("");
                            if original.is_empty() {
                                continue;
                            }

                            match nli.score_pair(original, target_text).await {
                                Ok((score, hypothesis_scores)) => {
                                    pair_scores.push(score);
                                    info!(
                                        target_did = target_did,
                                        target_handle = target_handle,
                                        pair_type = "inferred",
                                        hostility_score = format!("{:.3}", score),
                                        "NLI audit"
                                    );
                                    if let Some(dir) = data_dir {
                                        let event = crate::scoring::audit_log::AuditEvent::nli(
                                            crate::scoring::audit_log::NliFields {
                                                target_did: target_did.to_string(),
                                                target_handle: target_handle.to_string(),
                                                pair_type: "inferred".to_string(),
                                                original_text: original.to_string(),
                                                response_text: target_text.to_string(),
                                                hypothesis_scores,
                                                hostility_score: score,
                                                similarity: Some(*similarity),
                                            },
                                        );
                                        match crate::scoring::audit_log::AuditWriter::from_env(
                                            dir,
                                            crate::scoring::audit_log::EventKind::Nli,
                                        ) {
                                            Ok(writer) => {
                                                if let Err(e) = writer.record(event) {
                                                    warn!(error = %e, "Failed to write NLI audit JSONL");
                                                }
                                            }
                                            Err(e) => {
                                                warn!(error = %e, "Failed to init NLI audit writer");
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, "NLI scoring failed for inferred pair");
                                }
                            }
                        }
                        crate::scoring::nli::avg_context_score(&pair_scores)
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to embed target posts for NLI");
                        None
                    }
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // Step 6: Apply scoring formula in spec order:
    //   1. raw_score = tox * 70 * (1 + overlap * 1.5)
    //   2. score_with_behavioral = raw_score * behavioral_boost (via gate)
    //   3. context_multiplier = 1.0 + (context_score * 0.5)
    //   4. final_score = score_with_behavioral * context_multiplier
    let (raw_score, _) = threat::compute_threat_score(avg_toxicity, topic_overlap, weights);

    let (score_with_behavioral, benign_gate, gate_was_bypassed) =
        behavioral::apply_behavioral_modifier_contextual(
            raw_score,
            quote_ratio,
            reply_ratio,
            pile_on,
            avg_engagement,
            median_engagement,
            context_score,
        );

    // Only apply context multiplier if gate wasn't bypassed by context.
    // When the gate is bypassed due to context_score >= 0.5, context has
    // already done its work — don't multiply again on top of it.
    let context_multiplier = match (context_score, gate_was_bypassed) {
        (Some(ctx), false) => 1.0 + (ctx * 0.5), // normal: context boosts
        (Some(_), true) => 1.0,                  // gate bypass consumed context
        (None, _) => 1.0,
    };

    // Step 7: Apply graph distance weight
    // Strangers get amplified (1.2x), mutual follows get dampened (0.6x).
    // Applied AFTER benign gate so it cannot bypass ally protections.
    let distance_weight = graph_distance.map(|d| d.threat_weight()).unwrap_or(1.0);
    let final_score =
        (score_with_behavioral * context_multiplier * distance_weight).clamp(0.0, 100.0);

    let tier = crate::db::models::ThreatTier::from_score(final_score);

    let behavioral_boost = behavioral::compute_behavioral_boost(quote_ratio, reply_ratio, pile_on);
    let signals = behavioral::BehavioralSignals {
        quote_ratio,
        reply_ratio,
        avg_engagement,
        pile_on,
        benign_gate,
        behavioral_boost,
    };
    let signals_json = serde_json::to_string(&signals)?;

    info!(
        handle = target_handle,
        toxicity = format!("{:.2}", avg_toxicity),
        overlap = format!("{:.2}", topic_overlap),
        context = format!("{:?}", context_score),
        raw_score = format!("{:.1}", raw_score),
        threat = format!("{:.1}", final_score),
        tier = tier.as_str(),
        quote_ratio = format!("{:.2}", quote_ratio),
        reply_ratio = format!("{:.2}", reply_ratio),
        benign_gate = benign_gate,
        behavioral_boost = format!("{:.2}", behavioral_boost),
        posts = sample.total_posts,
        "Scored account"
    );

    Ok(AccountScore {
        did: target_did.to_string(),
        handle: target_handle.to_string(),
        toxicity_score: Some(avg_toxicity),
        topic_overlap: Some(topic_overlap),
        threat_score: Some(final_score),
        threat_tier: Some(tier.to_string()),
        posts_analyzed: sample.total_posts as u32,
        top_toxic_posts,
        scored_at: String::new(),
        behavioral_signals: Some(signals_json),
        context_score,
        graph_distance: graph_distance.map(|d| d.as_str().to_string()),
        fingerprint_quality: Some(fp_quality.as_str().to_string()),
        // Confidence reflects the *depth* of the analysis, not the score's
        // tier-boundary distance:
        //   - High: full pipeline (50 posts), reliable fingerprint, normal staleness
        //   - Standard: full pipeline but fingerprint quality degraded/unreliable
        //   - Low: stage 1 early-exit (set on the early-return branch)
        // Near-tier-boundary accounts are still re-scored sooner via
        // `should_continue_to_stage3`, but that signal is consumed elsewhere
        // (it gates additional work) and shouldn't override the depth signal.
        scoring_confidence: Some(
            match fp_quality {
                FingerprintQuality::Normal => "high",
                FingerprintQuality::Degraded | FingerprintQuality::Unreliable => "standard",
            }
            .to_string(),
        ),
    })
}

/// Minimum number of replies to use reply-weighted toxicity.
/// Below this, falls back to flat rate across all posts.
const MIN_REPLIES_FOR_WEIGHTING: usize = 5;

/// Compute reply-weighted toxicity rate.
///
/// Reply toxicity is weighted 70% and original toxicity 30%, because
/// hostile behavior manifests in replies — not original posts. An account
/// can post wholesome original content and be vicious in replies.
///
/// Falls back to flat rate when there are fewer than 5 replies (insufficient
/// interactive data to weight reliably).
///
/// Arguments are counts of toxic posts by type, not continuous scores.
/// When using ONNX only (pre-Zentropi), these counts come from
/// `weighted_toxicity()` exceeding 0.5 (the category-weighted threshold).
/// When Zentropi is active (Phase 5), counts come from Zentropi binary labels.
pub fn compute_reply_weighted_toxicity(
    toxic_replies: usize,
    total_replies: usize,
    toxic_originals: usize,
    total_originals: usize,
) -> f64 {
    let total = total_replies + total_originals;
    if total == 0 {
        return 0.0;
    }

    if total_replies < MIN_REPLIES_FOR_WEIGHTING {
        let toxic_total = toxic_replies + toxic_originals;
        return toxic_total as f64 / total as f64;
    }

    let reply_tox_rate = if total_replies > 0 {
        toxic_replies as f64 / total_replies as f64
    } else {
        0.0
    };

    let original_tox_rate = if total_originals > 0 {
        toxic_originals as f64 / total_originals as f64
    } else {
        0.0
    };

    reply_tox_rate * 0.7 + original_tox_rate * 0.3
}

// ============================================================
// Adaptive sampling — stage decision functions
// ============================================================

// Re-export the canonical clean-pass threshold so build_profile and the stage
// decision functions stay in lockstep with TwoStageToxicityScorer.
use crate::toxicity::ensemble::ONNX_CLEAN_THRESHOLD;

/// Minimum number of first-person posts (originals + quotes) required for the
/// Stage 1 clean-pass filter to be considered reliable. A reply-heavy account
/// with only 1–2 originals could otherwise vacuously pass even if the bulk of
/// their (un-checked) reply content is hostile.
pub const MIN_FIRST_PERSON_POSTS_FOR_EARLY_EXIT: usize = 5;

/// Check if an account can exit early at Stage 1 (25 posts).
///
/// Exits when ALL ONNX scores are below the clean threshold AND topic
/// overlap is below the gate threshold. This catches the ~50-60% of
/// sweep accounts that are clearly clean and topically irrelevant.
///
/// `onnx_scores` should be the ONNX toxicity values for first-person posts
/// (originals + quotes) only — reply texts in isolation aren't reliable for
/// the < 0.10 clean-pass and need conversation context. The function
/// requires at least `MIN_FIRST_PERSON_POSTS_FOR_EARLY_EXIT` scores so a
/// reply-heavy sample with 0–1 originals cannot vacuously pass.
///
/// `topic_overlap` is `None` when TF-IDF extraction failed — in that case
/// we cannot judge topical relevance and should NOT early-exit, lest a
/// sparse-vocabulary account silently slip through.
///
/// ONNX is ONLY reliable for low scores. A low score genuinely means
/// no hostile language or identity terms. High scores are NOT trustworthy
/// (keyword triggering on identity terms).
pub fn should_early_exit_stage1(
    onnx_scores: &[f64],
    topic_overlap: Option<f64>,
    overlap_gate_threshold: f64,
) -> bool {
    if onnx_scores.len() < MIN_FIRST_PERSON_POSTS_FOR_EARLY_EXIT {
        return false;
    }
    let Some(overlap) = topic_overlap else {
        return false;
    };
    overlap < overlap_gate_threshold && onnx_scores.iter().all(|&s| s < ONNX_CLEAN_THRESHOLD)
}

/// Tier boundary proximity thresholds.
const TIER_BOUNDARIES: [f64; 3] = [8.0, 15.0, 35.0]; // Watch, Elevated, High
const BOUNDARY_MARGIN: f64 = 5.0;

/// Check if a Stage 2 score is near a tier boundary and needs Stage 3.
///
/// Returns true if the score is within ±5 points of any tier boundary,
/// meaning more data could change the tier classification.
pub fn should_continue_to_stage3(score: f64) -> bool {
    TIER_BOUNDARIES
        .iter()
        .any(|&boundary| (score - boundary).abs() <= BOUNDARY_MARGIN)
}
