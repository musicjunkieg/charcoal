// Golden baseline test for `stage1_outcome` + `score_from_sample`.
//
// This file is the BEHAVIOR CONTRACT for #208 Chunks 3-6. It locks today's
// scoring behavior so that when the phased scan restructures the pipeline,
// any drift turns these tests red. It MUST pass against current code and is
// intended to stay green throughout the rest of the plan.
//
// Four golden cases:
// (a) < 5 posts → Terminal "Insufficient Data"
// (b) Early-exit → Terminal "Low", toxicity_score=0, scoring_confidence="low"
// (c) Stage-2 survivor (no NLI) → exact AccountScore field-by-field snapshot
// (d) NLI two-pass gate triggered → structure-only asserts (model-gated)

use anyhow::Result;
use async_trait::async_trait;

use charcoal::bluesky::posts::{Post, PostSample, ReplyPost};
use charcoal::bluesky::relationships::GraphDistance;
use charcoal::scoring::profile::{score_from_sample, stage1_outcome, Stage1Outcome};
use charcoal::scoring::threat::ThreatWeights;
use charcoal::topics::fingerprint::{TopicCluster, TopicFingerprint};
use charcoal::toxicity::traits::{
    BinaryVerdict, ToxicityAttributes, ToxicityResult, ToxicityScorer,
};

// ── Test fixtures ────────────────────────────────────────────────────────────

/// Scorer that returns a fixed continuous toxicity score for every text.
/// Matches the FixedScorer pattern from unit_profile.rs.
struct FixedScorer(f64);

#[async_trait]
impl ToxicityScorer for FixedScorer {
    async fn score_text(&self, _text: &str) -> Result<ToxicityResult> {
        Ok(ToxicityResult {
            toxicity: self.0,
            attributes: ToxicityAttributes::default(),
        })
    }
}

fn make_post(uri: &str, text: &str) -> Post {
    Post {
        uri: uri.to_string(),
        text: text.to_string(),
        created_at: None,
        like_count: 0,
        repost_count: 0,
        quote_count: 0,
        is_quote: false,
    }
}

fn make_reply(uri: &str, text: &str, parent_uri: &str) -> ReplyPost {
    ReplyPost {
        post: make_post(uri, text),
        parent_uri: parent_uri.to_string(),
    }
}

/// Fingerprint about astrophysics — wholly unrelated to food/weather posts.
/// Low TF-IDF overlap (< overlap_gate_threshold 0.15) when target posts discuss
/// everyday topics (sandwiches, weather, gardens).
fn astrophysics_fingerprint() -> TopicFingerprint {
    TopicFingerprint {
        clusters: vec![TopicCluster {
            label: "astrophysics".to_string(),
            keywords: vec![
                "quasar".to_string(),
                "nebula".to_string(),
                "redshift".to_string(),
                "telescope".to_string(),
                "pulsar".to_string(),
                "photon".to_string(),
                "galaxy".to_string(),
                "cosmology".to_string(),
            ],
            weight: 1.0,
        }],
        post_count: 200,
    }
}

/// Fingerprint containing the SAME keywords used in the target's posts for
/// case (d), so that TF-IDF overlap is >= overlap_gate_threshold (0.15).
fn toxicology_fingerprint() -> TopicFingerprint {
    TopicFingerprint {
        clusters: vec![TopicCluster {
            label: "toxicology".to_string(),
            keywords: vec![
                "toxic".to_string(),
                "poison".to_string(),
                "venom".to_string(),
                "lethal".to_string(),
                "hazard".to_string(),
                "dangerous".to_string(),
                "contamination".to_string(),
                "exposure".to_string(),
            ],
            weight: 1.0,
        }],
        post_count: 200,
    }
}

fn binary_verdict(is_toxic: bool, onnx: f64) -> BinaryVerdict {
    BinaryVerdict {
        is_toxic,
        onnx_score: onnx,
        onnx_attributes: ToxicityAttributes::default(),
    }
}

// ── Case (a): < 5 posts → Terminal "Insufficient Data" ───────────────────────

#[tokio::test]
async fn golden_a_insufficient_data_terminal() {
    // Golden snapshot: a 3-post sample never reaches Stage 2.
    // All score fields must be absent; tier must be "Insufficient Data".
    let sample = PostSample {
        originals: vec![
            make_post("at://golden/a/1", "first post"),
            make_post("at://golden/a/2", "second post"),
            make_post("at://golden/a/3", "third post"),
        ],
        replies: vec![],
        quotes: vec![],
        reply_ratio: 0.0,
        quote_ratio: 0.0,
        total_posts: 3,
    };

    let scorer = FixedScorer(0.0);
    let weights = ThreatWeights::default();
    let fp = astrophysics_fingerprint();

    let outcome = stage1_outcome(
        &sample,
        &scorer,
        "golden-a.bsky.social",
        "did:plc:golden-a",
        &fp,
        &weights,
        None, // no graph_distance
    )
    .await
    .expect("stage1_outcome should not error on case (a)");

    match outcome {
        Stage1Outcome::Terminal(score) => {
            // ── GOLDEN SNAPSHOT ──
            assert_eq!(score.did, "did:plc:golden-a", "did mismatch");
            assert_eq!(score.handle, "golden-a.bsky.social", "handle mismatch");
            assert_eq!(
                score.threat_tier.as_deref(),
                Some("Insufficient Data"),
                "tier mismatch: expected Insufficient Data"
            );
            assert_eq!(
                score.posts_analyzed, 3,
                "posts_analyzed must equal sample size"
            );
            // All continuous fields are absent for insufficient-data accounts.
            assert!(
                score.toxicity_score.is_none(),
                "toxicity_score must be None"
            );
            assert!(score.topic_overlap.is_none(), "topic_overlap must be None");
            assert!(score.threat_score.is_none(), "threat_score must be None");
            assert!(
                score.behavioral_signals.is_none(),
                "behavioral_signals must be None"
            );
            assert!(score.context_score.is_none(), "context_score must be None");
            assert!(
                score.fingerprint_quality.is_none(),
                "fingerprint_quality must be None"
            );
            assert!(
                score.scoring_confidence.is_none(),
                "scoring_confidence must be None"
            );
            // graph_distance is None because we passed None.
            assert!(
                score.graph_distance.is_none(),
                "graph_distance must be None when not supplied"
            );
            // top_toxic_posts is empty for insufficient-data accounts.
            assert!(
                score.top_toxic_posts.is_empty(),
                "no toxic posts for insufficient-data"
            );
        }
        Stage1Outcome::Proceed { .. } => {
            panic!("golden case (a): expected Terminal for < 5 posts, got Proceed");
        }
    }
}

// Case (a) with graph_distance passthrough — verifies graph_distance is threaded
// through even for terminal insufficient-data accounts.
#[tokio::test]
async fn golden_a_insufficient_data_with_graph_distance() {
    let sample = PostSample {
        originals: vec![make_post("at://golden/a2/1", "solo post")],
        replies: vec![],
        quotes: vec![],
        reply_ratio: 0.0,
        quote_ratio: 0.0,
        total_posts: 1,
    };

    let scorer = FixedScorer(0.0);
    let weights = ThreatWeights::default();
    let fp = astrophysics_fingerprint();

    let outcome = stage1_outcome(
        &sample,
        &scorer,
        "golden-a2.bsky.social",
        "did:plc:golden-a2",
        &fp,
        &weights,
        Some(GraphDistance::Stranger),
    )
    .await
    .expect("stage1_outcome should not error");

    match outcome {
        Stage1Outcome::Terminal(score) => {
            // graph_distance must be threaded through even in the Terminal branch.
            assert_eq!(
                score.graph_distance.as_deref(),
                Some("Stranger"),
                "graph_distance must be preserved on Terminal insufficient-data"
            );
            assert_eq!(score.threat_tier.as_deref(), Some("Insufficient Data"));
        }
        Stage1Outcome::Proceed { .. } => panic!("expected Terminal for 1-post sample"),
    }
}

// ── Case (b): Early-exit → Terminal "Low" ────────────────────────────────────

#[tokio::test]
async fn golden_b_early_exit_terminal_low() {
    // Golden snapshot: 6 first-person originals, all scored 0.0 (< ONNX_CLEAN_THRESHOLD
    // of 0.10), and topically irrelevant to the astrophysics fingerprint.
    // The early-exit gate fires and produces a Terminal "Low" score.
    //
    // Snapshotted values:
    //   threat_tier = "Low"
    //   toxicity_score = Some(0.0)   (set explicitly by the early-exit branch)
    //   threat_score = Some(0.0)     (set explicitly by the early-exit branch)
    //   scoring_confidence = Some("low")
    //   fingerprint_quality = Some(<non-None>)  (derived from originals/replies count)
    //   context_score = None
    //   behavioral_signals = None
    //   posts_analyzed = 6
    let originals = vec![
        make_post("at://golden/b/1", "had a lovely sandwich for lunch today"),
        make_post(
            "at://golden/b/2",
            "the weather is sunny and warm this morning",
        ),
        make_post("at://golden/b/3", "watering my tomato plants in the garden"),
        make_post(
            "at://golden/b/4",
            "made fresh coffee and read a paperback novel",
        ),
        make_post("at://golden/b/5", "took the dog for a walk around the park"),
        make_post(
            "at://golden/b/6",
            "baking bread this weekend, smells wonderful",
        ),
    ];
    let sample = PostSample {
        originals,
        replies: vec![],
        quotes: vec![],
        reply_ratio: 0.0,
        quote_ratio: 0.0,
        total_posts: 6,
    };

    let scorer = FixedScorer(0.0); // All ONNX scores are 0.0 → below 0.10 clean threshold
    let weights = ThreatWeights::default();
    let fp = astrophysics_fingerprint(); // Wholly unrelated → overlap < 0.15

    let outcome = stage1_outcome(
        &sample,
        &scorer,
        "golden-b.bsky.social",
        "did:plc:golden-b",
        &fp,
        &weights,
        None,
    )
    .await
    .expect("stage1_outcome should not error on case (b)");

    match outcome {
        Stage1Outcome::Terminal(score) => {
            // ── GOLDEN SNAPSHOT ──
            assert_eq!(score.did, "did:plc:golden-b");
            assert_eq!(score.handle, "golden-b.bsky.social");
            assert_eq!(
                score.threat_tier.as_deref(),
                Some("Low"),
                "early-exit tier must be Low"
            );
            // The early-exit branch explicitly sets toxicity=0.0 and threat=0.0.
            assert_eq!(
                score.toxicity_score,
                Some(0.0),
                "early-exit toxicity_score must be exactly 0.0"
            );
            assert_eq!(
                score.threat_score,
                Some(0.0),
                "early-exit threat_score must be exactly 0.0"
            );
            assert_eq!(score.posts_analyzed, 6, "posts_analyzed must match sample");
            // Confidence is set to "low" because this is a Stage-1 early exit.
            assert_eq!(
                score.scoring_confidence.as_deref(),
                Some("low"),
                "early-exit scoring_confidence must be low"
            );
            // fingerprint_quality is populated even for early exits (FingerprintQuality::from_counts).
            // 6 originals, 0 replies+quotes → originals>=15 is false, total=6<15 → Unreliable.
            assert_eq!(
                score.fingerprint_quality.as_deref(),
                Some("unreliable"),
                "early-exit fingerprint_quality for 6 originals, 0 replies = Unreliable"
            );
            // NLI and behavioral signals are NOT run on early-exit accounts.
            assert!(
                score.context_score.is_none(),
                "context_score must be None for early exit"
            );
            assert!(
                score.behavioral_signals.is_none(),
                "behavioral_signals must be None for early exit"
            );
            // No graph distance supplied.
            assert!(
                score.graph_distance.is_none(),
                "graph_distance must be None"
            );
            // topic_overlap is the TF-IDF overlap captured during Stage 1.
            // It will be Some(value) where value < 0.15 (the overlap gate threshold).
            match score.topic_overlap {
                Some(overlap) => {
                    assert!(
                        overlap < 0.15,
                        "early-exit overlap must be below gate threshold 0.15, got {overlap}"
                    );
                }
                None => {
                    // TF-IDF extraction may return None on a sparse vocabulary sample.
                    // Acceptable: the None case causes the early-exit NOT to fire, so
                    // if we got Terminal here, overlap must actually be Some. Panic.
                    panic!("early-exit Terminal has None topic_overlap, which is inconsistent");
                }
            }
        }
        Stage1Outcome::Proceed { .. } => {
            panic!("golden case (b): expected Terminal early-exit, got Proceed");
        }
    }
}

// ── Case (c): Stage-2 survivor (no NLI) — full deterministic snapshot ────────

#[tokio::test]
async fn golden_c_stage2_survivor_no_nli() {
    // Golden snapshot: a full Stage-2 score with no embedder, no NLI, no
    // graph distance. Inputs are engineered so that every value is computable
    // by hand, letting us assert exact field values.
    //
    // Sample design:
    //   6 originals: food/weather text (topically irrelevant to astrophysics fp)
    //   6 replies:   3 flagged toxic (is_toxic=true), 3 clean
    //   0 quotes
    //   reply_ratio = 6/12 = 0.5 (6 replies, 12 total_posts)
    //   quote_ratio = 0.0
    //
    // Verdicts:
    //   6 originals: all clean (is_toxic=false, onnx=0.05)
    //   3 replies: toxic (is_toxic=true, onnx=0.85)
    //   3 replies: clean (is_toxic=false, onnx=0.08)
    //
    // Reply-weighted toxicity:
    //   replies_len=6, originals_len=6, quotes_len=0
    //   toxic_replies = 3, toxic_originals = 0 (quotes also 0)
    //   total_replies = 6 >= MIN_REPLIES_FOR_WEIGHTING (5) → weighted path
    //   reply_tox_rate = 3/6 = 0.5
    //   original_tox_rate = 0/6 = 0.0
    //   avg_toxicity = 0.5 * 0.7 + 0.0 * 0.3 = 0.35
    //
    // TF-IDF overlap path (no embedder):
    //   fingerprint_posts = originals only? NO: 6 originals < 15 → use all posts
    //   All posts are about food/weather → negligible overlap with astrophysics fp
    //   overlap < 0.15 → gate applies
    //
    // Raw score (gated):
    //   raw_score = avg_toxicity * gate_max_score = 0.35 * 25.0 = 8.75
    //   (min'd with gate_max_score = 25.0 → 8.75)
    //
    // Behavioral modifier:
    //   avg_engagement = 0.0 (all like_count=0, repost_count=0)
    //   median_engagement = 0.0 (caller passes 0.0)
    //   is_behaviorally_benign check:
    //     quote_ratio (0.0) < 0.15 ✓
    //     reply_ratio (0.5) < 0.30 ✗  → NOT benign (reply_ratio too high)
    //   behavioral_boost = 1.0 + 0.0*0.20 + 0.5*0.15 + 0 = 1.075
    //   score_with_behavioral = 8.75 * 1.075 = 9.40625
    //
    // Context: None → multiplier = 1.0
    // Graph distance: None → weight = 1.0
    //
    // final_score = 9.40625 → clamped → 9.40625
    // tier = Watch (8.0 ≤ 9.40625 < 15.0)
    //
    // FingerprintQuality: originals=6, replies+quotes=6
    //   originals (6) >= 15? NO. originals (6) == 0? NO.
    //   total (12) >= 15? NO → Unreliable → scoring_confidence = "standard"
    //
    // top_toxic_posts: 3 toxic posts (highest onnx=0.85), then up to 3.

    let originals: Vec<Post> = (0..6)
        .map(|i| {
            make_post(
                &format!("at://golden/c/o/{i}"),
                "baking bread and drinking coffee",
            )
        })
        .collect();
    let replies: Vec<ReplyPost> = (0..6)
        .map(|i| {
            make_reply(
                &format!("at://golden/c/r/{i}"),
                "you are completely wrong about this",
                &format!("at://parent/c/{i}"),
            )
        })
        .collect();

    let sample = PostSample {
        originals,
        replies,
        quotes: vec![],
        reply_ratio: 0.5, // 6 replies / 12 total
        quote_ratio: 0.0,
        total_posts: 12,
    };

    // all_post_texts: originals (0..6) ++ reply texts (0..6), in that order.
    let all_post_texts: Vec<String> = sample
        .originals
        .iter()
        .map(|p| p.text.clone())
        .chain(sample.replies.iter().map(|r| r.post.text.clone()))
        .collect();
    // Contexts align with all_post_texts; consumed by the (already-run) classifier.
    let contexts: Vec<Option<String>> = vec![None; all_post_texts.len()];

    // Verdicts: 6 originals clean, first 3 replies toxic, last 3 replies clean.
    let mut verdicts: Vec<BinaryVerdict> = Vec::new();
    for _ in 0..6 {
        verdicts.push(binary_verdict(false, 0.05)); // originals: clean
    }
    for i in 0..6 {
        if i < 3 {
            verdicts.push(binary_verdict(true, 0.85)); // replies 0-2: toxic
        } else {
            verdicts.push(binary_verdict(false, 0.08)); // replies 3-5: clean
        }
    }

    let weights = ThreatWeights::default();
    let fp = astrophysics_fingerprint(); // unrelated → low overlap → gate fires

    let score = score_from_sample(
        &sample,
        &all_post_texts,
        &contexts,
        &verdicts,
        &fp,
        &weights,
        None,  // embedder
        None,  // protected_embedding
        0.0,   // median_engagement
        false, // pile_on
        None,  // nli_scorer
        None,  // protected_posts_with_embeddings
        None,  // direct_pairs
        None,  // data_dir
        None,  // graph_distance
        "golden-c.bsky.social",
        "did:plc:golden-c",
        None, // stage1_overlap: ignored by score_from_sample; carried only for staged-scan blob parity
    )
    .await
    .expect("score_from_sample should not error on case (c)");

    // ── GOLDEN SNAPSHOT ──
    assert_eq!(score.did, "did:plc:golden-c");
    assert_eq!(score.handle, "golden-c.bsky.social");
    assert_eq!(score.posts_analyzed, 12);

    // Reply-weighted toxicity: exactly 0.35 (computed above).
    let tox = score.toxicity_score.expect("toxicity_score must be Some");
    assert!(
        (tox - 0.35).abs() < 1e-9,
        "golden(c) toxicity_score: expected 0.35, got {tox}"
    );

    // Topic overlap: Some value below the gate threshold 0.15.
    let overlap = score.topic_overlap.expect("topic_overlap must be Some");
    assert!(
        overlap < 0.15,
        "golden(c) overlap: expected < 0.15 (gate fires), got {overlap}"
    );

    // Threat score: 0.35 * 25.0 (gate) = 8.75; behavioral boost 1.075 → 9.40625.
    // The exact float depends on TF-IDF's computed overlap (the gate path uses
    // tox * gate_max, so it's overlap-independent). Assert exact.
    let threat = score.threat_score.expect("threat_score must be Some");
    assert!(
        (threat - 9.40625).abs() < 1e-6,
        "golden(c) threat_score: expected 9.40625, got {threat}"
    );

    // Tier: Watch (8.0 ≤ 9.40625 < 15.0).
    assert_eq!(
        score.threat_tier.as_deref(),
        Some("Watch"),
        "golden(c) tier: expected Watch"
    );

    // No NLI scorer → context_score is None.
    assert!(
        score.context_score.is_none(),
        "golden(c) context_score must be None"
    );

    // No graph_distance → graph_distance field is None.
    assert!(
        score.graph_distance.is_none(),
        "golden(c) graph_distance must be None"
    );

    // behavioral_signals must be populated (full pipeline runs behavioral logic).
    let signals_json = score
        .behavioral_signals
        .expect("behavioral_signals must be Some");
    let signals: charcoal::scoring::behavioral::BehavioralSignals =
        serde_json::from_str(&signals_json).expect("behavioral_signals must be valid JSON");
    assert!(
        (signals.quote_ratio - 0.0).abs() < 1e-9,
        "golden(c) quote_ratio: expected 0.0, got {}",
        signals.quote_ratio
    );
    assert!(
        (signals.reply_ratio - 0.5).abs() < 1e-9,
        "golden(c) reply_ratio: expected 0.5, got {}",
        signals.reply_ratio
    );
    assert!(!signals.pile_on, "golden(c) pile_on must be false");
    // Benign gate did NOT fire (reply_ratio 0.5 > BENIGN_REPLY_RATIO_MAX 0.30).
    assert!(
        !signals.benign_gate,
        "golden(c) benign_gate must be false (reply_ratio too high)"
    );
    assert!(
        (signals.behavioral_boost - 1.075).abs() < 1e-9,
        "golden(c) behavioral_boost: expected 1.075, got {}",
        signals.behavioral_boost
    );

    // fingerprint_quality: 6 originals, 6 replies+quotes → Unreliable (total < 15).
    assert_eq!(
        score.fingerprint_quality.as_deref(),
        Some("unreliable"),
        "golden(c) fingerprint_quality: expected unreliable"
    );

    // scoring_confidence: Unreliable → "standard".
    assert_eq!(
        score.scoring_confidence.as_deref(),
        Some("standard"),
        "golden(c) scoring_confidence: expected standard"
    );

    // top_toxic_posts: the 3 toxic replies (onnx=0.85) should appear, and
    // there should be at most 3 posts (the code takes 3).
    assert_eq!(
        score.top_toxic_posts.len(),
        3,
        "golden(c) top_toxic_posts: expected 3 (the 3 toxic replies)"
    );
    for tp in &score.top_toxic_posts {
        assert!(
            (tp.toxicity - 0.85).abs() < 1e-9,
            "golden(c) each top toxic post must have onnx_score 0.85, got {}",
            tp.toxicity
        );
    }
}

// Case (c) variant: benign gate fires (low reply_ratio, high engagement).
#[tokio::test]
async fn golden_c_benign_gate_fires() {
    // The benign gate caps score at 12.0 when:
    //   quote_ratio < 0.15, reply_ratio < 0.30, no pile_on, avg_engagement > median
    // We engineer a sample where raw_score > 12.0 but the gate caps it to 12.0.
    //
    // Sample: 20 originals, 5 replies, 0 quotes → reply_ratio=5/25=0.2 < 0.30 ✓
    // All originals have high like_count=100 → avg_engagement >> median (0.0).
    // Verdicts: all 5 replies toxic, 0 originals toxic.
    // toxic_replies=5, total_replies=5 >= 5 → weighted path
    // reply_tox_rate = 5/5 = 1.0; original_tox_rate = 0/20 = 0.0
    // avg_toxicity = 1.0*0.7 + 0.0*0.3 = 0.7
    // overlap < 0.15 → raw_score = 0.7 * 25.0 = 17.5 > BENIGN_GATE_CAP (12.0)
    // avg_engagement = (20*100 + 5*0) / 25 = 80.0 > median_engagement (0.0)
    // → benign gate FIRES → score capped at 12.0 → tier Watch.

    let originals: Vec<Post> = (0..20)
        .map(|i| Post {
            uri: format!("at://golden/c2/o/{i}"),
            text: "baking bread and drinking tea by the garden".to_string(),
            created_at: None,
            like_count: 100, // high engagement
            repost_count: 0,
            quote_count: 0,
            is_quote: false,
        })
        .collect();
    let replies: Vec<ReplyPost> = (0..5)
        .map(|i| {
            make_reply(
                &format!("at://golden/c2/r/{i}"),
                "this is completely unacceptable and wrong",
                &format!("at://parent/c2/{i}"),
            )
        })
        .collect();

    let sample = PostSample {
        originals,
        replies,
        quotes: vec![],
        reply_ratio: 5.0 / 25.0, // 0.2
        quote_ratio: 0.0,
        total_posts: 25,
    };

    let all_post_texts: Vec<String> = sample
        .originals
        .iter()
        .map(|p| p.text.clone())
        .chain(sample.replies.iter().map(|r| r.post.text.clone()))
        .collect();
    let contexts: Vec<Option<String>> = vec![None; all_post_texts.len()];

    // 20 originals: clean; 5 replies: all toxic
    let mut verdicts: Vec<BinaryVerdict> = Vec::new();
    for _ in 0..20 {
        verdicts.push(binary_verdict(false, 0.04));
    }
    for _ in 0..5 {
        verdicts.push(binary_verdict(true, 0.90));
    }

    let weights = ThreatWeights::default();
    let fp = astrophysics_fingerprint();

    let score = score_from_sample(
        &sample,
        &all_post_texts,
        &contexts,
        &verdicts,
        &fp,
        &weights,
        None,  // embedder
        None,  // protected_embedding
        0.0,   // median_engagement — our avg(80.0) > 0.0 → benign fires
        false, // pile_on
        None,  // nli_scorer
        None,  // protected_posts_with_embeddings
        None,  // direct_pairs
        None,  // data_dir
        None,  // graph_distance
        "golden-c2.bsky.social",
        "did:plc:golden-c2",
        None,
    )
    .await
    .expect("score_from_sample should not error");

    // ── GOLDEN SNAPSHOT ──
    let threat = score.threat_score.expect("threat_score must be Some");
    // Benign gate caps at 12.0 exactly (raw was 17.5 > 12.0 → capped).
    assert!(
        (threat - 12.0).abs() < 1e-9,
        "golden(c2) benign gate: expected threat_score=12.0, got {threat}"
    );

    // Fix 3: assert exact toxicity_score (deterministic from canned verdicts).
    // Reply-weighted formula: 5 replies all toxic, 20 originals all clean.
    //   total_replies=5 >= MIN_REPLIES_FOR_WEIGHTING(5) → weighted path
    //   reply_tox_rate  = 5/5  = 1.0
    //   original_tox_rate = 0/20 = 0.0
    //   avg_toxicity = 1.0 * 0.7 + 0.0 * 0.3 = 0.7
    let tox = score
        .toxicity_score
        .expect("golden(c2) toxicity_score must be Some");
    assert_eq!(
        tox, 0.7,
        "golden(c2) toxicity_score: expected exactly 0.7 from reply-weighted formula, got {tox}"
    );
    assert_eq!(
        score.threat_tier.as_deref(),
        Some("Watch"),
        "golden(c2) tier: 12.0 is Watch (8.0 ≤ 12.0 < 15.0)"
    );

    let signals_json = score
        .behavioral_signals
        .expect("behavioral_signals must be Some");
    let signals: charcoal::scoring::behavioral::BehavioralSignals =
        serde_json::from_str(&signals_json).expect("valid JSON");
    assert!(signals.benign_gate, "golden(c2) benign_gate must be true");

    // fingerprint_quality: 20 originals ≥ 15 → Normal → scoring_confidence = "high"
    assert_eq!(
        score.fingerprint_quality.as_deref(),
        Some("normal"),
        "golden(c2) fingerprint_quality: 20 originals ≥ 15 → normal"
    );
    assert_eq!(
        score.scoring_confidence.as_deref(),
        Some("high"),
        "golden(c2) scoring_confidence: Normal fp → high"
    );
}

// ── Case (d): NLI two-pass gate (model-gated) ────────────────────────────────

#[tokio::test]
async fn golden_d_nli_two_pass_gate() {
    // This case exercises the NLI direct-pairs path inside `score_from_sample`.
    // It requires the real DeBERTa ONNX model, so we gate on its presence.
    //
    // When the model IS present:
    //   - We assert context_score.is_some()
    //   - We assert structural fields are populated
    //   - For NLI-derived floats, we use a tolerance (not exact) because the
    //     quantized ONNX model output is deterministic on a given machine but
    //     we cannot hard-code it here without running inference first.
    //   - We do assert tier string and that threat_score is within a range.
    //
    // When the model is NOT present:
    //   - We print a skip message and return immediately (test passes).

    let model_base = std::env::var("CHARCOAL_MODEL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| charcoal::toxicity::download::default_model_dir());

    if !charcoal::toxicity::download::nli_files_present(&model_base) {
        eprintln!(
            "SKIP golden case (d): NLI model not present at {model_base:?} — \
             run `charcoal download-model` to enable this case"
        );
        return;
    }

    // NliScorer::load(model_dir) calls nli_model_dir(model_dir) internally, so
    // pass model_base (the top-level models dir) — not the already-appended subdir.
    let nli = charcoal::scoring::nli::NliScorer::load(&model_base)
        .expect("NLI model should load when files are present");

    // Sample design: enough posts with high topic overlap to push raw_score >= 8.0.
    // We use the toxicology_fingerprint so that target posts about toxicology
    // terms produce overlap >= 0.15, then pair with high toxicity (all flagged).
    //
    // 20 originals + 5 replies → Normal fingerprint quality.
    // All originals use keywords matching the toxicology fingerprint.
    // Verdicts: all 20 originals AND all 5 replies toxic → avg_toxicity high.
    //
    // Approximate math (overlap will vary by TF-IDF extraction):
    //   avg_toxicity ≈ 1.0 (all flagged)
    //   overlap probably ≥ 0.2 (same keywords as fingerprint)
    //   raw_score = 1.0 * 70 * (1 + overlap * 1.5) >> 8.0 ✓

    let originals: Vec<Post> = (0..20)
        .map(|i| {
            make_post(
                &format!("at://golden/d/o/{i}"),
                "toxic poison venom lethal hazard dangerous contamination exposure",
            )
        })
        .collect();
    let replies: Vec<ReplyPost> = (0..5)
        .map(|i| {
            make_reply(
                &format!("at://golden/d/r/{i}"),
                "this toxic hazard is dangerous and lethal to all",
                &format!("at://parent/d/{i}"),
            )
        })
        .collect();

    let sample = PostSample {
        originals,
        replies,
        quotes: vec![],
        reply_ratio: 5.0 / 25.0,
        quote_ratio: 0.0,
        total_posts: 25,
    };

    let all_post_texts: Vec<String> = sample
        .originals
        .iter()
        .map(|p| p.text.clone())
        .chain(sample.replies.iter().map(|r| r.post.text.clone()))
        .collect();
    let contexts: Vec<Option<String>> = vec![None; all_post_texts.len()];

    // All verdicts flagged toxic.
    let verdicts: Vec<BinaryVerdict> = (0..25).map(|_| binary_verdict(true, 0.92)).collect();

    // Direct pairs: one clear hostile interaction (original→response).
    let direct_pairs: Vec<(String, String)> = vec![(
        "This community values mutual respect and kindness.".to_string(),
        "That is absolute garbage, these people are toxic poison.".to_string(),
    )];

    // Use a unique temp dir for data_dir (audit log writes are silently skipped
    // on error). `tempfile::tempdir()` gives a per-test directory so parallel
    // test runs don't clobber each other's dirs; it auto-cleans on drop.
    let data_dir_guard = tempfile::tempdir().expect("create temp data dir");
    let data_dir = data_dir_guard.path().to_path_buf();

    let fp = toxicology_fingerprint(); // overlapping keywords → overlap ≥ 0.15
    let weights = ThreatWeights::default();

    let score = score_from_sample(
        &sample,
        &all_post_texts,
        &contexts,
        &verdicts,
        &fp,
        &weights,
        None,  // embedder (TF-IDF overlap path)
        None,  // protected_embedding
        0.0,   // median_engagement
        false, // pile_on
        Some(&nli),
        None, // protected_posts_with_embeddings (direct pairs mode, not inferred)
        Some(&direct_pairs),
        Some(&data_dir),
        None, // graph_distance
        "golden-d.bsky.social",
        "did:plc:golden-d",
        None,
    )
    .await
    .expect("score_from_sample should not error on case (d)");

    // ── GOLDEN SNAPSHOT (structure + tolerance) ──
    // NLI model produced a context_score → must be Some.
    assert!(
        score.context_score.is_some(),
        "golden(d) context_score must be Some when NLI model is present and pairs are supplied"
    );
    let ctx = score.context_score.unwrap();
    assert!(
        (0.0..=1.0).contains(&ctx),
        "golden(d) context_score must be in [0,1], got {ctx}"
    );

    // Fix 1: assert the overlap gate is NOT firing — overlap must be >= 0.15 so the
    // FULL multiplicative formula path runs (tox * 70 * (1 + overlap*1.5)), not the
    // gated fallback path (tox * 25). Without this, a TF-IDF change that drops overlap
    // below 0.15 would silently switch formulas and the golden wouldn't notice.
    // The toxicology_fingerprint shares exact keywords with the post text, so
    // overlap should be very high (observed ~1.0 on this machine).
    let overlap = score
        .topic_overlap
        .expect("golden(d) must have Some topic_overlap");
    assert!(
        overlap >= 0.15,
        "golden(d) topic_overlap must be >= 0.15 so the FULL multiplicative formula path runs, \
         not the gated path; got {overlap}"
    );

    // Fix 2: with all-toxic verdicts + full multiplicative overlap formula, threat_score
    // is much higher than 8.0. Observed value on this machine: 100.0 (clamped max).
    // Assert >= 40.0 — meaningful enough to catch any regression that halves the score,
    // with safe margin against formula/quantization variation across machines.
    let threat = score.threat_score.expect("threat_score must be Some");
    assert!(
        threat >= 40.0,
        "golden(d) threat_score must be >= 40.0 (observed ~100.0 on reference machine); got {threat}"
    );

    // The tier must be one of the valid tiers.
    let tier = score
        .threat_tier
        .as_deref()
        .expect("threat_tier must be Some");
    assert!(
        matches!(tier, "Watch" | "Elevated" | "High"),
        "golden(d) tier: expected Watch/Elevated/High, got {tier}"
    );

    // Structural checks: all pipeline fields must be present.
    assert!(
        score.toxicity_score.is_some(),
        "golden(d) toxicity_score must be Some"
    );
    assert!(
        score.topic_overlap.is_some(),
        "golden(d) topic_overlap must be Some"
    );
    assert!(
        score.behavioral_signals.is_some(),
        "golden(d) behavioral_signals must be Some"
    );
    assert_eq!(score.posts_analyzed, 25, "golden(d) posts_analyzed");

    // fingerprint_quality: 20 originals ≥ 15 → Normal → confidence "high".
    assert_eq!(
        score.fingerprint_quality.as_deref(),
        Some("normal"),
        "golden(d) fingerprint_quality"
    );
    assert_eq!(
        score.scoring_confidence.as_deref(),
        Some("high"),
        "golden(d) scoring_confidence"
    );

    // `data_dir_guard` auto-removes the temp dir on drop — no manual cleanup.
    drop(data_dir_guard);
}
