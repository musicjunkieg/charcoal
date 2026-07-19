// Unit tests for the extracted, fetch-free scoring cores in
// `scoring::profile`: `stage1_outcome` and `score_from_sample`.
//
// These functions were extracted from the monolithic `build_profile` in Task
// 2.2 (#208) as a behavior-identical refactor. The existing suite (composition,
// scoring, behavioral, ensemble) proves end-to-end behavior is preserved; these
// tests exercise the new seams directly against canned `PostSample`s + a fixed
// scorer, with no network or classifier calls.

use anyhow::Result;
use async_trait::async_trait;

use charcoal::bluesky::posts::{Post, PostSample, ReplyPost};
use charcoal::scoring::profile::{score_from_sample, stage1_outcome, Stage1Outcome};
use charcoal::scoring::threat::ThreatWeights;
use charcoal::topics::fingerprint::{TopicCluster, TopicFingerprint};
use charcoal::toxicity::traits::{
    BinaryVerdict, ToxicityAttributes, ToxicityResult, ToxicityScorer,
};

/// A scorer that returns a fixed continuous toxicity for any text. Used as the
/// ONNX-equivalent primary scorer for the Stage 1 clean-pass check.
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

fn post(uri: &str, text: &str) -> Post {
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

/// A fingerprint about wholly unrelated topics, so canned target posts about
/// food/weather have ~zero overlap (drives the early-exit gate).
fn unrelated_fingerprint() -> TopicFingerprint {
    TopicFingerprint {
        clusters: vec![TopicCluster {
            label: "astrophysics".to_string(),
            keywords: vec![
                "quasar".to_string(),
                "nebula".to_string(),
                "redshift".to_string(),
                "telescope".to_string(),
            ],
            weight: 1.0,
        }],
        post_count: 100,
    }
}

fn binary_verdict(is_toxic: bool, onnx: f64) -> BinaryVerdict {
    BinaryVerdict {
        is_toxic,
        onnx_score: onnx,
        onnx_attributes: ToxicityAttributes::default(),
    }
}

// ============================================================
// stage1_outcome — terminal branch: < 5 posts → "Insufficient Data"
// ============================================================

#[tokio::test]
async fn stage1_insufficient_data_when_fewer_than_five_posts() {
    let sample = PostSample {
        originals: vec![post("at://a/1", "hello"), post("at://a/2", "world")],
        replies: vec![],
        quotes: vec![],
        reply_ratio: 0.0,
        quote_ratio: 0.0,
        total_posts: 2,
    };
    let scorer = FixedScorer(0.0);
    let weights = ThreatWeights::default();
    let fp = unrelated_fingerprint();

    let outcome = stage1_outcome(
        &sample,
        &scorer,
        "target.bsky.social",
        "did:plc:target",
        &fp,
        &weights,
        None,
    )
    .await
    .expect("stage1_outcome should not error");

    match outcome {
        Stage1Outcome::Terminal(score) => {
            assert_eq!(score.threat_tier.as_deref(), Some("Insufficient Data"));
            assert_eq!(score.posts_analyzed, 2);
            assert!(score.toxicity_score.is_none());
            assert!(score.threat_score.is_none());
            assert!(score.fingerprint_quality.is_none());
            assert!(score.scoring_confidence.is_none());
            assert_eq!(score.did, "did:plc:target");
        }
        Stage1Outcome::Proceed { .. } => panic!("expected Terminal for <5 posts"),
    }
}

// ============================================================
// stage1_outcome — terminal branch: clean + irrelevant → early-exit "Low"
// ============================================================

#[tokio::test]
async fn stage1_early_exit_low_when_clean_and_irrelevant() {
    // 6 first-person originals (>= MIN_FIRST_PERSON_POSTS_FOR_EARLY_EXIT), all
    // scored 0.0 by the fixed scorer (< ONNX_CLEAN_THRESHOLD 0.10), and topics
    // that don't overlap the astrophysics fingerprint (< overlap gate 0.15).
    let originals = vec![
        post("at://a/1", "had a lovely sandwich for lunch today"),
        post("at://a/2", "the weather is sunny and warm this morning"),
        post("at://a/3", "watering my tomato plants in the garden"),
        post("at://a/4", "made fresh coffee and read a paperback novel"),
        post("at://a/5", "took the dog for a walk around the park"),
        post("at://a/6", "baking bread this weekend, smells wonderful"),
    ];
    let sample = PostSample {
        originals,
        replies: vec![],
        quotes: vec![],
        reply_ratio: 0.0,
        quote_ratio: 0.0,
        total_posts: 6,
    };
    let scorer = FixedScorer(0.0);
    let weights = ThreatWeights::default();
    let fp = unrelated_fingerprint();

    let outcome = stage1_outcome(
        &sample,
        &scorer,
        "clean.bsky.social",
        "did:plc:clean",
        &fp,
        &weights,
        None,
    )
    .await
    .expect("stage1_outcome should not error");

    match outcome {
        Stage1Outcome::Terminal(score) => {
            assert_eq!(score.threat_tier.as_deref(), Some("Low"));
            assert_eq!(score.threat_score, Some(0.0));
            assert_eq!(score.toxicity_score, Some(0.0));
            assert_eq!(score.posts_analyzed, 6);
            assert_eq!(score.scoring_confidence.as_deref(), Some("low"));
            assert!(score.fingerprint_quality.is_some());
        }
        Stage1Outcome::Proceed { .. } => {
            panic!("expected Terminal early-exit for clean + irrelevant account")
        }
    }
}

// ============================================================
// stage1_outcome — Proceed branch: toxic content survives early exit
// ============================================================

#[tokio::test]
async fn stage1_proceeds_when_not_clean() {
    // Same irrelevant topics, but the scorer reports high toxicity (0.9), so the
    // clean-pass fails and the account must go to Stage 2.
    let originals = vec![
        post("at://a/1", "had a lovely sandwich for lunch today"),
        post("at://a/2", "the weather is sunny and warm this morning"),
        post("at://a/3", "watering my tomato plants in the garden"),
        post("at://a/4", "made fresh coffee and read a paperback novel"),
        post("at://a/5", "took the dog for a walk around the park"),
        post("at://a/6", "baking bread this weekend, smells wonderful"),
    ];
    let sample = PostSample {
        originals,
        replies: vec![],
        quotes: vec![],
        reply_ratio: 0.0,
        quote_ratio: 0.0,
        total_posts: 6,
    };
    let scorer = FixedScorer(0.9);
    let weights = ThreatWeights::default();
    let fp = unrelated_fingerprint();

    let outcome = stage1_outcome(
        &sample,
        &scorer,
        "loud.bsky.social",
        "did:plc:loud",
        &fp,
        &weights,
        None,
    )
    .await
    .expect("stage1_outcome should not error");

    assert!(
        matches!(outcome, Stage1Outcome::Proceed { .. }),
        "high-toxicity account should proceed to Stage 2"
    );
}

// ============================================================
// score_from_sample — survivor: no NLI/embedder, deterministic math
// ============================================================

#[tokio::test]
async fn score_from_sample_survivor_no_context() {
    // A survivor: 6 originals + 6 replies, half the replies flagged toxic.
    // No embedder, no NLI scorer, no direct pairs → context_score is None and
    // the math is fully deterministic (TF-IDF overlap path, no graph distance).
    let originals: Vec<Post> = (0..6)
        .map(|i| {
            post(
                &format!("at://o/{i}"),
                "talking about quasars and nebula redshift",
            )
        })
        .collect();
    let replies: Vec<ReplyPost> = (0..6)
        .map(|i| ReplyPost {
            post: post(
                &format!("at://r/{i}"),
                "you are completely wrong about telescopes",
            ),
            parent_uri: format!("at://parent/{i}"),
        })
        .collect();

    let sample = PostSample {
        originals,
        replies,
        quotes: vec![],
        reply_ratio: 0.5,
        quote_ratio: 0.0,
        total_posts: 12,
    };

    // all_post_texts: 6 originals ++ 6 reply texts (order matters).
    let all_post_texts: Vec<String> = sample
        .originals
        .iter()
        .map(|p| p.text.clone())
        .chain(sample.replies.iter().map(|r| r.post.text.clone()))
        .collect();
    // contexts align with all_post_texts; consumed by the (already-run)
    // classifier, so any aligned values work here.
    let contexts: Vec<Option<String>> = vec![None; all_post_texts.len()];

    // Verdicts: originals clean, 3 of 6 replies flagged toxic.
    let mut verdicts: Vec<BinaryVerdict> = Vec::new();
    for _ in 0..6 {
        verdicts.push(binary_verdict(false, 0.05));
    }
    for i in 0..6 {
        verdicts.push(binary_verdict(i < 3, if i < 3 { 0.8 } else { 0.1 }));
    }

    let weights = ThreatWeights::default();
    let fp = unrelated_fingerprint();

    let score = score_from_sample(
        &sample,
        &all_post_texts,
        &contexts,
        &verdicts,
        &fp,
        &weights,
        None,  // embedder
        None,  // protected_embedding
        None,  // precomputed_target_embedding
        0.0,   // median_engagement
        false, // pile_on
        None,  // nli_scorer
        None,  // protected_posts_with_embeddings
        None,  // direct_pairs
        None,  // data_dir
        None,  // graph_distance
        "survivor.bsky.social",
        "did:plc:survivor",
        None, // stage1_overlap
    )
    .await
    .expect("score_from_sample should not error");

    assert_eq!(score.did, "did:plc:survivor");
    assert_eq!(score.posts_analyzed, 12);
    // A full-pipeline score must populate toxicity, overlap, threat, and tier.
    assert!(score.toxicity_score.is_some());
    assert!(score.topic_overlap.is_some());
    assert!(score.threat_score.is_some());
    assert!(score.threat_tier.is_some());
    assert!(score.behavioral_signals.is_some());
    // No NLI scorer → no context score.
    assert!(score.context_score.is_none());
    // Reply-weighted toxicity: 3/6 toxic replies (weight 0.7) + 0/6 toxic
    // originals (weight 0.3) = 0.5 * 0.7 = 0.35.
    let tox = score.toxicity_score.unwrap();
    assert!(
        (tox - 0.35).abs() < 1e-9,
        "expected reply-weighted toxicity 0.35, got {tox}"
    );
    // Fingerprint quality on 6 originals + 6 reply/quote = Degraded (>=15 total
    // is false here → 12 total, originals < 15, originals > 0 → Unreliable).
    assert!(score.fingerprint_quality.is_some());
}

// ============================================================
// Task 1 (#213): topic-overlap embedding moves out of finalize into gather.
// select_fingerprint_posts is the ONE shared selection used by both phases,
// so gather feeds the embedder exactly the posts finalize would have.
// ============================================================

use charcoal::scoring::profile::select_fingerprint_posts;
use charcoal::topics::embeddings::cosine_similarity_embeddings;

fn reply(uri: &str, text: &str) -> ReplyPost {
    ReplyPost {
        post: post(uri, text),
        parent_uri: "at://parent/1".to_string(),
    }
}

#[test]
fn select_fingerprint_posts_uses_only_originals_when_at_least_15() {
    let originals: Vec<Post> = (0..15)
        .map(|i| post(&format!("at://o/{i}"), &format!("original {i}")))
        .collect();
    let sample = PostSample {
        originals,
        replies: vec![reply("at://r/1", "a reply")],
        quotes: vec![post("at://q/1", "a quote")],
        reply_ratio: 0.0,
        quote_ratio: 0.0,
        total_posts: 17,
    };

    let selected = select_fingerprint_posts(&sample);

    // 15 originals ≥ 15 → originals only; the reply and quote are excluded.
    assert_eq!(selected.len(), 15);
    assert_eq!(selected[0], "original 0");
    assert_eq!(selected[14], "original 14");
    assert!(!selected.iter().any(|t| t == "a reply" || t == "a quote"));
}

#[test]
fn select_fingerprint_posts_falls_back_to_all_when_fewer_than_15_originals() {
    let sample = PostSample {
        originals: vec![post("at://o/1", "orig one"), post("at://o/2", "orig two")],
        replies: vec![reply("at://r/1", "reply one")],
        quotes: vec![post("at://q/1", "quote one")],
        reply_ratio: 0.0,
        quote_ratio: 0.0,
        total_posts: 4,
    };

    let selected = select_fingerprint_posts(&sample);

    // < 15 originals → originals ++ replies ++ quotes, in that order.
    assert_eq!(
        selected,
        vec![
            "orig one".to_string(),
            "orig two".to_string(),
            "reply one".to_string(),
            "quote one".to_string(),
        ]
    );
}

#[tokio::test]
async fn score_from_sample_uses_precomputed_target_embedding_without_an_embedder() {
    // The precomputed target vector (from Phase A gather) must drive topic
    // overlap directly — no embedder call, exact cosine of protected vs target.
    let originals: Vec<Post> = (0..5)
        .map(|i| post(&format!("at://o/{i}"), &format!("original {i}")))
        .collect();
    let all_post_texts: Vec<String> = originals.iter().map(|p| p.text.clone()).collect();
    let contexts: Vec<Option<String>> = vec![None; 5];
    let verdicts: Vec<_> = (0..5).map(|_| binary_verdict(false, 0.1)).collect();

    let sample = PostSample {
        originals,
        replies: vec![],
        quotes: vec![],
        reply_ratio: 0.0,
        quote_ratio: 0.0,
        total_posts: 5,
    };

    let weights = ThreatWeights::default();
    let fp = unrelated_fingerprint();

    let protected: Vec<f64> = vec![1.0, 2.0, 2.0];
    let precomputed: Vec<f64> = vec![2.0, 2.0, 1.0];
    let expected_overlap = cosine_similarity_embeddings(&protected, &precomputed);

    let score = score_from_sample(
        &sample,
        &all_post_texts,
        &contexts,
        &verdicts,
        &fp,
        &weights,
        None,               // embedder — deliberately absent; precomputed must win
        Some(&protected),   // protected_embedding
        Some(&precomputed), // precomputed_target_embedding (NEW, from gather)
        0.0,                // median_engagement
        false,              // pile_on
        None,               // nli_scorer
        None,               // protected_posts_with_embeddings
        None,               // direct_pairs
        None,               // data_dir
        None,               // graph_distance
        "survivor.bsky.social",
        "did:plc:survivor",
        None, // stage1_overlap
    )
    .await
    .expect("score_from_sample should not error");

    let overlap = score
        .topic_overlap
        .expect("overlap should be Some via precomputed path");
    assert!(
        (overlap - expected_overlap).abs() < 1e-12,
        "precomputed overlap {overlap} != expected cosine {expected_overlap}"
    );
}
