// Profile resolution — batch DID-to-handle lookups via public API.
//
// Used to resolve DIDs from Constellation backlinks into human-readable
// handles. The `app.bsky.actor.getProfiles` endpoint accepts up to 25
// actors per request.

use anyhow::Result;
use std::collections::HashMap;
use tracing::{debug, warn};

use super::client::PublicAtpClient;

/// Resolve a batch of DIDs to their current handles.
///
/// Returns a map of DID -> handle. DIDs that fail to resolve are omitted
/// from the result (the caller should fall back to using the DID itself).
/// Requests are batched in groups of 25 (the API maximum).
pub async fn resolve_dids_to_handles(
    client: &PublicAtpClient,
    dids: &[String],
) -> Result<HashMap<String, String>> {
    let mut result = HashMap::new();

    for chunk in dids.chunks(25) {
        // Build repeated "actors" query params for the batch
        let query_params: Vec<(&str, &str)> =
            chunk.iter().map(|did| ("actors", did.as_str())).collect();

        if query_params.is_empty() {
            continue;
        }

        match client
            .xrpc_get::<atrium_api::app::bsky::actor::get_profiles::Output>(
                "app.bsky.actor.getProfiles",
                &query_params,
            )
            .await
        {
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
