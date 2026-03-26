use charcoal::bluesky::relationships::GraphDistance;

// ============================================================
// GraphDistance enum basics
// ============================================================

#[test]
fn graph_distance_as_str() {
    assert_eq!(GraphDistance::MutualFollow.as_str(), "Mutual follow");
    assert_eq!(GraphDistance::InboundFollow.as_str(), "Follows you");
    assert_eq!(GraphDistance::OutboundFollow.as_str(), "You follow");
    assert_eq!(GraphDistance::Stranger.as_str(), "Stranger");
}

#[test]
fn graph_distance_display() {
    assert_eq!(format!("{}", GraphDistance::Stranger), "Stranger");
    assert_eq!(format!("{}", GraphDistance::MutualFollow), "Mutual follow");
}

// ============================================================
// Threat weights — risk ordering
// ============================================================

#[test]
fn threat_weight_ordering() {
    // Strangers are highest risk, mutual follows lowest
    assert!(
        GraphDistance::Stranger.threat_weight() > GraphDistance::OutboundFollow.threat_weight()
    );
    assert!(
        GraphDistance::OutboundFollow.threat_weight()
            > GraphDistance::InboundFollow.threat_weight()
    );
    assert!(
        GraphDistance::InboundFollow.threat_weight() > GraphDistance::MutualFollow.threat_weight()
    );
}

#[test]
fn threat_weight_stranger_amplifies() {
    assert!(
        GraphDistance::Stranger.threat_weight() > 1.0,
        "Strangers should amplify score"
    );
}

#[test]
fn threat_weight_mutual_dampens() {
    assert!(
        GraphDistance::MutualFollow.threat_weight() < 1.0,
        "Mutual follows should dampen score"
    );
}

#[test]
fn threat_weight_specific_values() {
    assert!((GraphDistance::MutualFollow.threat_weight() - 0.6).abs() < 0.001);
    assert!((GraphDistance::InboundFollow.threat_weight() - 0.8).abs() < 0.001);
    assert!((GraphDistance::OutboundFollow.threat_weight() - 0.9).abs() < 0.001);
    assert!((GraphDistance::Stranger.threat_weight() - 1.2).abs() < 0.001);
}

// ============================================================
// Serde roundtrip
// ============================================================

#[test]
fn graph_distance_serde_roundtrip() {
    let original = GraphDistance::Stranger;
    let json = serde_json::to_string(&original).unwrap();
    let restored: GraphDistance = serde_json::from_str(&json).unwrap();
    assert_eq!(original, restored);
}

// ============================================================
// Response parsing
// ============================================================

#[test]
fn parse_relationship_mutual_follow() {
    let json = serde_json::json!({
        "relationships": [{
            "$type": "app.bsky.graph.defs#relationship",
            "did": "did:plc:abc123",
            "following": "at://did:plc:protected/app.bsky.graph.follow/1",
            "followedBy": "at://did:plc:abc123/app.bsky.graph.follow/2"
        }]
    });
    let result = charcoal::bluesky::relationships::parse_relationships_response(&json).unwrap();
    assert_eq!(
        result.get("did:plc:abc123"),
        Some(&GraphDistance::MutualFollow)
    );
}

#[test]
fn parse_relationship_inbound_only() {
    let json = serde_json::json!({
        "relationships": [{
            "$type": "app.bsky.graph.defs#relationship",
            "did": "did:plc:abc123",
            "followedBy": "at://did:plc:abc123/app.bsky.graph.follow/2"
        }]
    });
    let result = charcoal::bluesky::relationships::parse_relationships_response(&json).unwrap();
    assert_eq!(
        result.get("did:plc:abc123"),
        Some(&GraphDistance::InboundFollow)
    );
}

#[test]
fn parse_relationship_outbound_only() {
    let json = serde_json::json!({
        "relationships": [{
            "$type": "app.bsky.graph.defs#relationship",
            "did": "did:plc:abc123",
            "following": "at://did:plc:protected/app.bsky.graph.follow/1"
        }]
    });
    let result = charcoal::bluesky::relationships::parse_relationships_response(&json).unwrap();
    assert_eq!(
        result.get("did:plc:abc123"),
        Some(&GraphDistance::OutboundFollow)
    );
}

#[test]
fn parse_relationship_no_connection() {
    let json = serde_json::json!({
        "relationships": [{
            "$type": "app.bsky.graph.defs#relationship",
            "did": "did:plc:abc123"
        }]
    });
    let result = charcoal::bluesky::relationships::parse_relationships_response(&json).unwrap();
    assert_eq!(result.get("did:plc:abc123"), Some(&GraphDistance::Stranger));
}

#[test]
fn parse_relationship_not_found_actor() {
    let json = serde_json::json!({
        "relationships": [{
            "$type": "app.bsky.graph.defs#notFoundActor",
            "did": "did:plc:abc123"
        }]
    });
    let result = charcoal::bluesky::relationships::parse_relationships_response(&json).unwrap();
    assert_eq!(result.get("did:plc:abc123"), Some(&GraphDistance::Stranger));
}

#[test]
fn parse_relationship_multiple() {
    let json = serde_json::json!({
        "relationships": [
            {
                "$type": "app.bsky.graph.defs#relationship",
                "did": "did:plc:mutual",
                "following": "at://x/y/1",
                "followedBy": "at://x/y/2"
            },
            {
                "$type": "app.bsky.graph.defs#relationship",
                "did": "did:plc:stranger"
            },
            {
                "$type": "app.bsky.graph.defs#notFoundActor",
                "did": "did:plc:gone"
            }
        ]
    });
    let result = charcoal::bluesky::relationships::parse_relationships_response(&json).unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(result["did:plc:mutual"], GraphDistance::MutualFollow);
    assert_eq!(result["did:plc:stranger"], GraphDistance::Stranger);
    assert_eq!(result["did:plc:gone"], GraphDistance::Stranger);
}

// ============================================================
// Scoring integration — graph distance applied to threat scores
// ============================================================

use charcoal::scoring::threat::{compute_threat_score, ThreatWeights};

#[test]
fn stranger_amplifies_score() {
    let weights = ThreatWeights::default();
    let (base, _) = compute_threat_score(0.5, 0.3, &weights);
    let amplified = base * GraphDistance::Stranger.threat_weight();
    assert!(
        amplified > base,
        "Stranger should amplify: {amplified} > {base}"
    );
}

#[test]
fn mutual_dampens_score() {
    let weights = ThreatWeights::default();
    let (base, _) = compute_threat_score(0.5, 0.3, &weights);
    let dampened = base * GraphDistance::MutualFollow.threat_weight();
    assert!(
        dampened < base,
        "Mutual follow should dampen: {dampened} < {base}"
    );
}

#[test]
fn no_distance_is_neutral() {
    let weight: f64 = None::<GraphDistance>
        .map(|d| d.threat_weight())
        .unwrap_or(1.0);
    assert!((weight - 1.0).abs() < 0.001, "None distance = 1.0 weight");
}

#[test]
fn stranger_toxic_in_topic_gets_boosted() {
    let weights = ThreatWeights::default();
    // High toxicity + topic overlap + stranger = max danger
    let (base, _) = compute_threat_score(0.7, 0.5, &weights);
    let final_score = (base * GraphDistance::Stranger.threat_weight()).clamp(0.0, 100.0);
    assert!(final_score >= 35.0, "Should be High tier: {final_score}");
}

#[test]
fn mutual_follow_moderate_tox_stays_low() {
    let weights = ThreatWeights::default();
    // Moderate toxicity + overlap + mutual follow = dampened
    let (base, _) = compute_threat_score(0.3, 0.3, &weights);
    let final_score = (base * GraphDistance::MutualFollow.threat_weight()).clamp(0.0, 100.0);
    // base = 0.3 * 70 * (1 + 0.3 * 1.5) = 21 * 1.45 = 30.45
    // dampened = 30.45 * 0.6 = 18.27 → Elevated, not High
    assert!(
        final_score < 35.0,
        "Mutual follow should stay below High: {final_score}"
    );
}
