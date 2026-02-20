// Composition tests — verifying that pure functions chain together correctly.
//
// These tests exercise the data flow between modules:
//   TF-IDF -> Fingerprint -> Overlap -> Threat Score
// without any network calls, database access, or filesystem side effects
// (except report generation which writes to /tmp).

use charcoal::db::models::{AccountScore, AmplificationEvent, ThreatTier, ToxicPost};
use charcoal::output::truncate_chars;
use charcoal::scoring::threat::{compute_threat_score, ThreatWeights};
use charcoal::topics::fingerprint::{TopicCluster, TopicFingerprint};
use charcoal::topics::overlap::{cosine_from_weights, cosine_similarity};
use charcoal::topics::tfidf::TfIdfExtractor;
use charcoal::topics::traits::TopicExtractor;

// ============================================================
// Chain: TF-IDF -> Fingerprint -> Overlap
// ============================================================

#[test]
fn similar_post_sets_have_meaningful_overlap() {
    let extractor = TfIdfExtractor {
        top_n_keywords: 20,
        max_clusters: 5,
    };

    let posts_a = vec![
        "Fat liberation is a civil rights movement that challenges weight stigma and diet culture"
            .to_string(),
        "Body positivity community continues fighting fatphobia in healthcare settings today"
            .to_string(),
        "Weight stigma causes real harm to fat patients seeking medical care from providers"
            .to_string(),
        "Fat activism promotes body autonomy and challenges anti-fat bias in society broadly"
            .to_string(),
        "Diet culture perpetuates harmful myths about weight and health outcomes for everyone"
            .to_string(),
        "Healthcare providers need training on weight stigma and size-inclusive care practices"
            .to_string(),
    ];

    let posts_b = vec![
        "Fat liberation activists challenge medical weight bias in clinical healthcare settings"
            .to_string(),
        "Weight stigma research shows systemic discrimination against fat bodies in medicine"
            .to_string(),
        "Body liberation movement fights against diet culture and pervasive fatphobia daily"
            .to_string(),
        "Fat patients face weight stigma when seeking healthcare from their medical providers"
            .to_string(),
        "Anti-fat bias in medicine causes documented harm to patients of all body sizes"
            .to_string(),
        "Size acceptance community promotes body autonomy and challenges weight based stigma"
            .to_string(),
    ];

    let fp_a = extractor.extract(&posts_a).unwrap();
    let fp_b = extractor.extract(&posts_b).unwrap();

    let overlap = cosine_similarity(&fp_a, &fp_b);
    assert!(
        overlap > 0.1,
        "Similar topic posts should have meaningful overlap, got {overlap}"
    );
}

#[test]
fn different_topic_sets_have_low_overlap() {
    let extractor = TfIdfExtractor {
        top_n_keywords: 20,
        max_clusters: 5,
    };

    let social_justice = vec![
        "Fat liberation is a civil rights movement that challenges weight stigma and diet culture"
            .to_string(),
        "Trans rights are human rights and queer identity deserves celebration not erasure"
            .to_string(),
        "DEI equity programs face backlash but anti-racism work remains essential for justice"
            .to_string(),
        "Body positivity community continues fighting fatphobia in healthcare settings today"
            .to_string(),
        "Weight stigma in medical settings causes real harm to fat patients seeking care"
            .to_string(),
        "Community governance moderation requires trust accountability and transparency always"
            .to_string(),
    ];

    let devops = vec![
        "Kubernetes container orchestration enables scalable microservice deployment strategies"
            .to_string(),
        "Docker multi-stage builds reduce container image size and improve security posture"
            .to_string(),
        "Terraform infrastructure as code manages cloud resources declaratively across providers"
            .to_string(),
        "Prometheus metrics collection with Grafana dashboards enables comprehensive monitoring"
            .to_string(),
        "Jenkins CI/CD pipelines automate build testing and deployment for continuous delivery"
            .to_string(),
        "Ansible configuration management automates server provisioning and application deployment"
            .to_string(),
    ];

    let fp_sj = extractor.extract(&social_justice).unwrap();
    let fp_do = extractor.extract(&devops).unwrap();

    let overlap = cosine_similarity(&fp_sj, &fp_do);
    assert!(
        overlap < 0.3,
        "Unrelated topics should have low overlap, got {overlap}"
    );
}

#[test]
fn self_overlap_is_approximately_one() {
    let extractor = TfIdfExtractor {
        top_n_keywords: 20,
        max_clusters: 5,
    };

    let posts = vec![
        "Fat liberation is a civil rights movement challenging weight stigma fundamentally"
            .to_string(),
        "Trans rights are human rights and queer identity deserves celebration always".to_string(),
        "Community governance requires trust accountability and transparent moderation practices"
            .to_string(),
        "DEI programs face backlash but equity work remains essential for justice today"
            .to_string(),
        "Atlassian Forge development requires understanding the app platform architecture deeply"
            .to_string(),
        "Weight stigma in medical settings causes real harm to fat patients regularly".to_string(),
    ];

    let fp = extractor.extract(&posts).unwrap();
    let overlap = cosine_similarity(&fp, &fp);
    assert!(
        (overlap - 1.0).abs() < 0.001,
        "Self-overlap should be ~1.0, got {overlap}"
    );
}

// ============================================================
// Chain: Fingerprint -> keyword_weights -> cosine_from_weights
// (manual construction to verify the pipeline math)
// ============================================================

#[test]
fn fingerprint_to_cosine_manual_pipeline() {
    let fp_a = TopicFingerprint {
        clusters: vec![
            TopicCluster {
                label: "fat liberation".to_string(),
                keywords: vec![
                    "fat".to_string(),
                    "liberation".to_string(),
                    "stigma".to_string(),
                ],
                weight: 0.5,
            },
            TopicCluster {
                label: "queer identity".to_string(),
                keywords: vec!["queer".to_string(), "trans".to_string()],
                weight: 0.3,
            },
            TopicCluster {
                label: "governance".to_string(),
                keywords: vec!["governance".to_string(), "moderation".to_string()],
                weight: 0.2,
            },
        ],
        post_count: 100,
    };

    let fp_b = TopicFingerprint {
        clusters: vec![
            TopicCluster {
                label: "body politics".to_string(),
                keywords: vec!["fat".to_string(), "body".to_string(), "stigma".to_string()],
                weight: 0.6,
            },
            TopicCluster {
                label: "gaming".to_string(),
                keywords: vec!["gaming".to_string(), "esports".to_string()],
                weight: 0.4,
            },
        ],
        post_count: 50,
    };

    // Step 1: Verify keyword_weights produces expected maps
    let weights_a = fp_a.keyword_weights();
    let weights_b = fp_b.keyword_weights();

    assert!(weights_a.contains_key("fat"));
    assert!(weights_b.contains_key("fat"));
    assert!(weights_a.contains_key("stigma"));
    assert!(weights_b.contains_key("stigma"));
    // "queer" only in fp_a
    assert!(weights_a.contains_key("queer"));
    assert!(!weights_b.contains_key("queer"));

    // Step 2: cosine_from_weights should produce non-trivial result
    let sim = cosine_from_weights(&weights_a, &weights_b);
    assert!(sim > 0.0, "Should have overlap via shared keywords");
    assert!(sim < 1.0, "Should not be identical");

    // Step 3: cosine_similarity convenience wrapper should give same result
    let sim_direct = cosine_similarity(&fp_a, &fp_b);
    assert!(
        (sim - sim_direct).abs() < 1e-10,
        "Manual pipeline and convenience wrapper should match"
    );
}

// ============================================================
// Chain: Overlap -> Threat Score -> Tier
// ============================================================

#[test]
fn high_overlap_high_toxicity_yields_high_tier() {
    let weights = ThreatWeights::default();
    let (score, tier) = compute_threat_score(0.75, 0.4, &weights);
    // 0.75 * 70 * (1 + 0.4 * 1.5) = 52.5 * 1.6 = 84.0
    assert!((score - 84.0).abs() < 0.1);
    assert_eq!(tier, ThreatTier::High);
}

#[test]
fn low_overlap_gates_even_high_toxicity() {
    let weights = ThreatWeights::default();
    let (score, tier) = compute_threat_score(0.95, 0.01, &weights);
    // Gated (0.01 < 0.15): 0.95 * 25 = 23.75
    assert!((score - 23.75).abs() < 0.1);
    assert_eq!(tier, ThreatTier::Elevated);
    // Key insight: 95% toxicity without topic overlap stays below High
    assert!(score < 35.0);
}

#[test]
fn tier_round_trip_all_tiers() {
    let cases = [
        (5.0, ThreatTier::Low, "Low"),
        (10.0, ThreatTier::Watch, "Watch"),
        (20.0, ThreatTier::Elevated, "Elevated"),
        (50.0, ThreatTier::High, "High"),
    ];
    for (score, expected_tier, expected_str) in cases {
        let tier = ThreatTier::from_score(score);
        assert_eq!(tier, expected_tier);
        assert_eq!(tier.as_str(), expected_str);
        assert_eq!(tier.to_string(), expected_str);
    }
}

// ============================================================
// Chain: Full pipeline — hostile, ally, and irrelevant accounts
// ============================================================

fn protected_fingerprint() -> TopicFingerprint {
    let extractor = TfIdfExtractor {
        top_n_keywords: 20,
        max_clusters: 5,
    };
    let posts = vec![
        "Fat liberation is a civil rights movement challenging weight stigma and diet culture"
            .to_string(),
        "Trans rights are human rights and queer identity deserves celebration not erasure"
            .to_string(),
        "Community governance moderation requires trust accountability and transparency always"
            .to_string(),
        "DEI equity programs face backlash but anti-racism work remains essential for justice"
            .to_string(),
        "Body positivity community continues fighting fatphobia in healthcare settings today"
            .to_string(),
        "Weight stigma in medical settings causes real harm to fat patients seeking care"
            .to_string(),
    ];
    extractor.extract(&posts).unwrap()
}

#[test]
fn full_pipeline_hostile_account() {
    let weights = ThreatWeights::default();
    let protected_fp = protected_fingerprint();

    let extractor = TfIdfExtractor {
        top_n_keywords: 20,
        max_clusters: 5,
    };
    let hostile_posts = vec![
        "Fat acceptance is dangerous health misinformation promoting obesity epidemic crisis".to_string(),
        "These body positivity activists are delusional about weight and health science reality".to_string(),
        "DEI diversity programs are discriminatory reverse racism against qualified candidates merit".to_string(),
        "Trans ideology threatens women biological sex based rights and sports fairness protections".to_string(),
        "Social justice warriors destroying academia with identity politics radical indoctrination".to_string(),
        "Weight stigma research is junk science funded by obesity promotion lobbyists agenda".to_string(),
    ];
    let hostile_fp = extractor.extract(&hostile_posts).unwrap();

    let overlap = cosine_similarity(&protected_fp, &hostile_fp);
    assert!(
        overlap > 0.0,
        "Hostile account should have some topic overlap: {overlap}"
    );

    // Simulate high toxicity (in real pipeline this comes from ONNX scorer)
    let (score, tier) = compute_threat_score(0.7, overlap, &weights);
    assert!(
        score > 15.0,
        "Hostile account with overlap should score > 15, got {score}"
    );
    assert!(
        tier == ThreatTier::Elevated || tier == ThreatTier::High,
        "Expected Elevated or High, got {tier}"
    );
}

#[test]
fn full_pipeline_ally_account() {
    let weights = ThreatWeights::default();
    let protected_fp = protected_fingerprint();

    let extractor = TfIdfExtractor {
        top_n_keywords: 20,
        max_clusters: 5,
    };
    let ally_posts = vec![
        "Fat liberation movement inspires me to challenge weight stigma in my healthcare practice".to_string(),
        "Supporting trans rights and queer identity is fundamental to building inclusive communities".to_string(),
        "Effective community governance requires listening to marginalized voices and building trust".to_string(),
        "DEI programs create equitable workplaces where everyone can thrive and contribute fully".to_string(),
        "Body liberation activism challenges systemic fatphobia in medicine and daily interactions".to_string(),
        "Weight stigma awareness training should be mandatory in medical education curricula".to_string(),
    ];
    let ally_fp = extractor.extract(&ally_posts).unwrap();

    let overlap = cosine_similarity(&protected_fp, &ally_fp);
    assert!(overlap > 0.0, "Ally should have topic overlap: {overlap}");

    // Ally has LOW toxicity
    let (score, _) = compute_threat_score(0.05, overlap, &weights);
    // Low toxicity keeps score manageable even with overlap
    assert!(
        score < 50.0,
        "Ally with low toxicity shouldn't score extremely high: {score}"
    );
}

#[test]
fn full_pipeline_irrelevant_account() {
    let weights = ThreatWeights::default();
    let protected_fp = protected_fingerprint();

    let extractor = TfIdfExtractor {
        top_n_keywords: 20,
        max_clusters: 5,
    };
    let devops_posts = vec![
        "Kubernetes container orchestration enables scalable microservice deployment strategies"
            .to_string(),
        "Docker multi-stage builds reduce container image size and improve security posture"
            .to_string(),
        "Terraform infrastructure code manages cloud resources declaratively across providers"
            .to_string(),
        "Prometheus metrics collection with Grafana dashboards enables comprehensive monitoring"
            .to_string(),
        "Jenkins CI/CD pipelines automate build testing and deployment for continuous delivery"
            .to_string(),
        "Ansible configuration management automates server provisioning and application deployment"
            .to_string(),
    ];
    let devops_fp = extractor.extract(&devops_posts).unwrap();

    let overlap = cosine_similarity(&protected_fp, &devops_fp);

    // Even with high toxicity, gate should cap the score if overlap is low
    let (score, _) = compute_threat_score(0.8, overlap, &weights);
    if overlap < weights.overlap_gate_threshold {
        assert!(score <= 25.0, "Gated score should be <= 25, got {score}");
    }
}

// ============================================================
// Chain: Report generation with synthesized data
// ============================================================

fn make_account(handle: &str, score: f64, tier: &str, toxicity: f64, overlap: f64) -> AccountScore {
    AccountScore {
        did: format!("did:plc:{handle}"),
        handle: handle.to_string(),
        toxicity_score: Some(toxicity),
        topic_overlap: Some(overlap),
        threat_score: Some(score),
        threat_tier: Some(tier.to_string()),
        posts_analyzed: 20,
        top_toxic_posts: if score >= 15.0 {
            vec![ToxicPost {
                text: format!("Sample toxic post from {handle}"),
                toxicity,
                uri: format!("at://{handle}/post/1"),
            }]
        } else {
            vec![]
        },
        scored_at: "2026-02-16".to_string(),
        behavioral_signals: None,
    }
}

#[test]
fn report_includes_all_tier_counts() {
    let accounts = vec![
        make_account("high.bsky.social", 65.0, "High", 0.85, 0.4),
        make_account("elevated.bsky.social", 20.0, "Elevated", 0.5, 0.2),
        make_account("watch.bsky.social", 12.0, "Watch", 0.3, 0.1),
        make_account("low.bsky.social", 3.0, "Low", 0.1, 0.02),
    ];

    let tmp_path = "/tmp/charcoal_test_all_tiers.md";
    let result = charcoal::output::markdown::generate_report(&accounts, None, &[], tmp_path);
    assert!(result.is_ok());

    let content = std::fs::read_to_string(tmp_path).unwrap();
    assert!(content.contains("| High | 1 |"));
    assert!(content.contains("| Elevated | 1 |"));
    assert!(content.contains("| Watch | 1 |"));
    assert!(content.contains("| Low | 1 |"));
    assert!(content.contains("| **Total** | **4** |"));

    let _ = std::fs::remove_file(tmp_path);
}

#[test]
fn report_empty_accounts() {
    let tmp_path = "/tmp/charcoal_test_empty_accounts.md";
    let result = charcoal::output::markdown::generate_report(&[], None, &[], tmp_path);
    assert!(result.is_ok());

    let content = std::fs::read_to_string(tmp_path).unwrap();
    assert!(content.contains("# Charcoal Threat Report"));
    assert!(content.contains("| **Total** | **0** |"));
    assert!(!content.contains("## Evidence"));

    let _ = std::fs::remove_file(tmp_path);
}

#[test]
fn report_includes_fingerprint_section() {
    let fp = TopicFingerprint {
        clusters: vec![
            TopicCluster {
                label: "fat liberation".to_string(),
                keywords: vec!["fat".to_string(), "liberation".to_string()],
                weight: 0.6,
            },
            TopicCluster {
                label: "queer identity".to_string(),
                keywords: vec!["queer".to_string(), "trans".to_string()],
                weight: 0.4,
            },
        ],
        post_count: 50,
    };

    let tmp_path = "/tmp/charcoal_test_fp_section.md";
    let result = charcoal::output::markdown::generate_report(&[], Some(&fp), &[], tmp_path);
    assert!(result.is_ok());

    let content = std::fs::read_to_string(tmp_path).unwrap();
    assert!(content.contains("## Protected User Topic Fingerprint"));
    assert!(content.contains("50 recent posts"));
    assert!(content.contains("fat liberation"));
    assert!(content.contains("queer identity"));

    let _ = std::fs::remove_file(tmp_path);
}

#[test]
fn report_quotes_but_not_reposts() {
    let events = vec![
        AmplificationEvent {
            id: 1,
            event_type: "quote".to_string(),
            amplifier_did: "did:plc:troll".to_string(),
            amplifier_handle: "troll.bsky.social".to_string(),
            original_post_uri: "at://did:plc:protected/post/1".to_string(),
            amplifier_post_uri: Some("at://did:plc:troll/post/2".to_string()),
            amplifier_text: Some("look at this delusional person lmao".to_string()),
            detected_at: "2026-02-15".to_string(),
            followers_fetched: false,
            followers_scored: false,
        },
        // Repost — no quote text, should NOT appear in quotes table
        AmplificationEvent {
            id: 2,
            event_type: "repost".to_string(),
            amplifier_did: "did:plc:other".to_string(),
            amplifier_handle: "other.bsky.social".to_string(),
            original_post_uri: "at://did:plc:protected/post/1".to_string(),
            amplifier_post_uri: None,
            amplifier_text: None,
            detected_at: "2026-02-15".to_string(),
            followers_fetched: false,
            followers_scored: false,
        },
    ];

    let tmp_path = "/tmp/charcoal_test_events_filter.md";
    let result = charcoal::output::markdown::generate_report(&[], None, &events, tmp_path);
    assert!(result.is_ok());

    let content = std::fs::read_to_string(tmp_path).unwrap();
    assert!(content.contains("## Amplification Events"));
    assert!(content.contains("troll.bsky.social"));
    assert!(content.contains("delusional"));
    // Repost without text should not be in quotes table
    assert!(!content.contains("other.bsky.social"));

    let _ = std::fs::remove_file(tmp_path);
}

#[test]
fn report_escapes_pipe_in_quote_text() {
    let events = vec![AmplificationEvent {
        id: 1,
        event_type: "quote".to_string(),
        amplifier_did: "did:plc:troll".to_string(),
        amplifier_handle: "troll.bsky.social".to_string(),
        original_post_uri: "at://post/1".to_string(),
        amplifier_post_uri: Some("at://post/2".to_string()),
        amplifier_text: Some("this | breaks | markdown tables".to_string()),
        detected_at: "2026-02-15".to_string(),
        followers_fetched: false,
        followers_scored: false,
    }];

    let tmp_path = "/tmp/charcoal_test_pipe_escape.md";
    let result = charcoal::output::markdown::generate_report(&[], None, &events, tmp_path);
    assert!(result.is_ok());

    let content = std::fs::read_to_string(tmp_path).unwrap();
    assert!(
        content.contains("\\|"),
        "Pipe chars should be escaped in markdown tables"
    );

    let _ = std::fs::remove_file(tmp_path);
}

// ============================================================
// Chain: truncate_chars in report context
// ============================================================

#[test]
fn truncation_works_in_report_pipeline() {
    let long_text = "a".repeat(200);
    let truncated = truncate_chars(&long_text, 100);
    assert_eq!(truncated.chars().count(), 103); // 100 + "..."
    assert!(truncated.ends_with("..."));
}

// ============================================================
// NoopScorer — always errors
// ============================================================

#[tokio::test]
async fn noop_scorer_always_errors() {
    use charcoal::toxicity::traits::{NoopScorer, ToxicityScorer};
    let scorer = NoopScorer;
    let result = scorer.score_text("hello").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("NoopScorer"));
}

#[tokio::test]
async fn noop_scorer_batch_also_errors() {
    use charcoal::toxicity::traits::{NoopScorer, ToxicityScorer};
    let scorer = NoopScorer;
    let texts = vec!["hello".to_string(), "world".to_string()];
    let result = scorer.score_batch(&texts).await;
    assert!(result.is_err());
}
