// Profile resolution — batch DID-to-handle lookups.
//
// Used to resolve DIDs from Constellation backlinks into human-readable
// handles. The `app.bsky.actor.getProfiles` endpoint accepts up to 25
// actors per request.

use anyhow::Result;
use bsky_sdk::BskyAgent;
use std::collections::HashMap;
use tracing::{debug, warn};

/// Resolve a batch of DIDs to their current handles.
///
/// Returns a map of DID → handle. DIDs that fail to resolve are omitted
/// from the result (the caller should fall back to using the DID itself).
/// Requests are batched in groups of 25 (the API maximum).
pub async fn resolve_dids_to_handles(
    agent: &BskyAgent,
    dids: &[String],
) -> Result<HashMap<String, String>> {
    let mut result = HashMap::new();

    for chunk in dids.chunks(25) {
        let actors: Vec<atrium_api::types::string::AtIdentifier> =
            chunk.iter().filter_map(|did| did.parse().ok()).collect();

        if actors.is_empty() {
            continue;
        }

        let params = atrium_api::app::bsky::actor::get_profiles::ParametersData { actors };

        match agent.api.app.bsky.actor.get_profiles(params.into()).await {
            Ok(output) => {
                for profile in &output.profiles {
                    result.insert(
                        profile.did.as_str().to_string(),
                        profile.handle.as_str().to_string(),
                    );
                }
                debug!(
                    resolved = output.profiles.len(),
                    requested = chunk.len(),
                    "Resolved DIDs to handles"
                );
            }
            Err(e) => {
                warn!(error = %e, batch_size = chunk.len(), "Failed to resolve DID batch");
            }
        }
    }

    Ok(result)
}
