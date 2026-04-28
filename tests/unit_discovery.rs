// tests/unit_discovery.rs
//
// Tests for topic-first discovery pipeline.

#[test]
fn extract_search_keywords_from_fingerprint() {
    use charcoal::discovery::topic_search;
    use charcoal::topics::fingerprint::{TopicCluster, TopicFingerprint};

    let fingerprint = TopicFingerprint {
        clusters: vec![
            TopicCluster {
                label: "fat liberation".to_string(),
                keywords: vec![
                    "fat".to_string(),
                    "liberation".to_string(),
                    "body".to_string(),
                ],
                weight: 0.8,
            },
            TopicCluster {
                label: "queer identity".to_string(),
                keywords: vec![
                    "queer".to_string(),
                    "identity".to_string(),
                    "trans".to_string(),
                ],
                weight: 0.6,
            },
            TopicCluster {
                label: "community".to_string(),
                keywords: vec!["community".to_string(), "governance".to_string()],
                weight: 0.3,
            },
        ],
        post_count: 50,
    };

    let search_terms = topic_search::extract_search_keywords(&fingerprint, 3);
    assert!(!search_terms.is_empty());
    assert!(search_terms.len() <= 3);
    // The top cluster's keywords should be represented
    assert!(search_terms
        .iter()
        .any(|k| k.contains("fat") || k.contains("liberation")));
}

#[test]
fn extract_search_keywords_respects_limit() {
    use charcoal::discovery::topic_search;
    use charcoal::topics::fingerprint::{TopicCluster, TopicFingerprint};

    let fingerprint = TopicFingerprint {
        clusters: vec![
            TopicCluster {
                label: "topic1".to_string(),
                keywords: vec!["keyword1".to_string()],
                weight: 0.9,
            },
            TopicCluster {
                label: "topic2".to_string(),
                keywords: vec!["keyword2".to_string()],
                weight: 0.5,
            },
            TopicCluster {
                label: "topic3".to_string(),
                keywords: vec!["keyword3".to_string()],
                weight: 0.3,
            },
        ],
        post_count: 30,
    };

    let terms = topic_search::extract_search_keywords(&fingerprint, 2);
    assert_eq!(terms.len(), 2);
}

#[test]
fn deduplicate_author_dids() {
    use charcoal::discovery::topic_search;

    let raw_dids = vec![
        "did:plc:aaa".to_string(),
        "did:plc:bbb".to_string(),
        "did:plc:aaa".to_string(), // duplicate
        "did:plc:ccc".to_string(),
    ];
    let already_scored = vec!["did:plc:bbb".to_string()]
        .into_iter()
        .collect::<std::collections::HashSet<_>>();

    let new_dids = topic_search::deduplicate_dids(&raw_dids, &already_scored);
    assert_eq!(new_dids.len(), 2); // aaa and ccc (bbb already scored)
    assert!(new_dids.contains(&"did:plc:aaa".to_string()));
    assert!(new_dids.contains(&"did:plc:ccc".to_string()));
}

#[test]
fn only_expand_from_high_and_elevated() {
    use charcoal::db::models::ThreatTier;
    use charcoal::discovery::threat_expansion::filter_expansion_candidates;

    let accounts = vec![
        ("did:plc:high1", ThreatTier::High),
        ("did:plc:elevated1", ThreatTier::Elevated),
        ("did:plc:watch1", ThreatTier::Watch),
        ("did:plc:low1", ThreatTier::Low),
    ];

    let candidates = filter_expansion_candidates(&accounts);
    assert_eq!(candidates.len(), 2);
    assert!(candidates.contains(&"did:plc:high1"));
    assert!(candidates.contains(&"did:plc:elevated1"));
}

#[test]
fn expand_empty_list() {
    use charcoal::discovery::threat_expansion::filter_expansion_candidates;

    let accounts: Vec<(&str, charcoal::db::models::ThreatTier)> = vec![];
    let candidates = filter_expansion_candidates(&accounts);
    assert!(candidates.is_empty());
}

#[test]
fn extract_keywords_skips_short_terms() {
    use charcoal::discovery::topic_search;
    use charcoal::topics::fingerprint::{TopicCluster, TopicFingerprint};

    let fingerprint = TopicFingerprint {
        clusters: vec![TopicCluster {
            label: "test".to_string(),
            keywords: vec!["ab".to_string(), "long_keyword".to_string()],
            weight: 0.9,
        }],
        post_count: 10,
    };

    let terms = topic_search::extract_search_keywords(&fingerprint, 5);
    // "ab" should be filtered out (< 3 chars), only "long_keyword" remains
    assert_eq!(terms.len(), 1);
    assert_eq!(terms[0], "long_keyword");
}
