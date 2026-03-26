// Graph distance classification via AT Protocol getRelationships API.
//
// Classifies the social relationship between the protected user and another
// account into one of four categories: MutualFollow, InboundFollow,
// OutboundFollow, or Stranger. Each category carries a threat_weight()
// multiplier used in the final scoring step.

use std::collections::HashMap;
use std::fmt;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::client::PublicAtpClient;

/// Maximum number of DIDs per getRelationships API call.
const BATCH_SIZE: usize = 30;

/// Social graph distance between the protected user and another account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GraphDistance {
    MutualFollow,
    InboundFollow,
    OutboundFollow,
    Stranger,
}

impl GraphDistance {
    /// Human-readable label for the graph distance.
    pub fn as_str(&self) -> &'static str {
        match self {
            GraphDistance::MutualFollow => "Mutual follow",
            GraphDistance::InboundFollow => "Follows you",
            GraphDistance::OutboundFollow => "You follow",
            GraphDistance::Stranger => "Stranger",
        }
    }

    /// Threat weight multiplier applied to the final score.
    ///
    /// Strangers get amplified (more suspicious — no social connection to
    /// the protected user). Mutual follows get dampened (existing relationship
    /// suggests non-hostile intent). Applied after the benign gate so allies
    /// stay protected.
    pub fn threat_weight(&self) -> f64 {
        match self {
            GraphDistance::MutualFollow => 0.6,
            GraphDistance::InboundFollow => 0.8,
            GraphDistance::OutboundFollow => 0.9,
            GraphDistance::Stranger => 1.2,
        }
    }
}

impl fmt::Display for GraphDistance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Parse the raw JSON response from `app.bsky.graph.getRelationships`.
///
/// Uses manual JSON field extraction because the AT Protocol `$type`
/// discriminator can be tricky with serde's tagged enum support. Returns
/// a map from DID to GraphDistance.
pub fn parse_relationships_response(
    json: &serde_json::Value,
) -> Result<HashMap<String, GraphDistance>> {
    let mut result = HashMap::new();
    let relationships = json["relationships"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Missing relationships array"))?;

    for entry in relationships {
        let did = entry["did"].as_str().unwrap_or_default().to_string();
        if did.is_empty() {
            continue;
        }

        let type_str = entry["$type"].as_str().unwrap_or_default();
        if type_str == "app.bsky.graph.defs#notFoundActor" {
            result.insert(did, GraphDistance::Stranger);
            continue;
        }

        let has_following = entry.get("following").and_then(|v| v.as_str()).is_some();
        let has_followed_by = entry.get("followedBy").and_then(|v| v.as_str()).is_some();

        let distance = match (has_following, has_followed_by) {
            (true, true) => GraphDistance::MutualFollow,
            (false, true) => GraphDistance::InboundFollow,
            (true, false) => GraphDistance::OutboundFollow,
            (false, false) => GraphDistance::Stranger,
        };
        result.insert(did, distance);
    }

    Ok(result)
}

/// Classify the social graph distance for a batch of DIDs relative to the
/// protected user. Calls `app.bsky.graph.getRelationships` in chunks of 30
/// (API limit). Returns a map from DID to GraphDistance.
pub async fn classify_relationships(
    client: &PublicAtpClient,
    protected_did: &str,
    target_dids: &[&str],
) -> Result<HashMap<String, GraphDistance>> {
    let mut all_results = HashMap::new();

    for chunk in target_dids.chunks(BATCH_SIZE) {
        let mut params: Vec<(&str, &str)> = vec![("actor", protected_did)];
        for did in chunk {
            params.push(("others", did));
        }

        match client
            .xrpc_get::<serde_json::Value>("app.bsky.graph.getRelationships", &params)
            .await
        {
            Ok(json) => match parse_relationships_response(&json) {
                Ok(batch) => {
                    debug!(
                        count = batch.len(),
                        "Classified {} relationship(s)",
                        batch.len()
                    );
                    all_results.extend(batch);
                }
                Err(e) => {
                    warn!(error = %e, "Failed to parse relationships response");
                }
            },
            Err(e) => {
                warn!(error = %e, "getRelationships API call failed, defaulting to Stranger");
                for did in chunk {
                    all_results.insert(did.to_string(), GraphDistance::Stranger);
                }
            }
        }
    }

    Ok(all_results)
}
