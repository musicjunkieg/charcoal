// Seed-keyword harvester — find candidate protected users by topic.
//
// The firehose sampler (jetstream.rs) gives an activity-weighted baseline of
// active accounts. This module complements it with a *targeted* draw: the
// people who post in the topic areas that attract organized harassment are the
// population most likely to need Charcoal. We search for posts about those
// topics and collect the authors.
//
// This is a thin wrapper over the existing `topic_search::search_posts_for_authors`,
// driven by a fixed seed keyword set rather than a per-user fingerprint (we
// don't have a fingerprint for candidates we haven't discovered yet).

use std::collections::HashSet;

use anyhow::{anyhow, Result};
use tracing::{info, warn};

use crate::bluesky::client::PublicAtpClient;
use crate::discovery::topic_search::{deduplicate_dids, search_posts_for_authors};

/// Seed keywords spanning the sensitive topic areas from `SPEC.md` — the
/// communities Charcoal's protected users tend to live in. This is deliberately
/// a fixed, broad list (not exhaustive): it's a sampling instrument for finding
/// *candidate* accounts, not a per-user fingerprint. Keep terms specific enough
/// that `searchPosts` returns people active in the community rather than generic
/// chatter.
pub const SEED_KEYWORDS: &[&str] = &[
    // Fat liberation / body politics
    "fat liberation",
    "fatphobia",
    "anti-fat bias",
    // Queer & trans identity
    "trans rights",
    "transgender healthcare",
    "queer community",
    "nonbinary",
    // DEI / anti-racism
    "anti-racism",
    "racial justice",
    "DEI",
    // Disability justice
    "disability justice",
    "chronic illness",
    // AI / LLM discourse
    "AI ethics",
    "generative AI",
    // Community governance / cybernetics
    "community governance",
    "mutual aid",
];

/// Harvest candidate author DIDs by searching `searchPosts` for each keyword.
///
/// Collects up to `per_keyword` unique authors per term, then deduplicates
/// across all terms. Individual keyword failures are logged and skipped; the
/// call only errors if *every* keyword search failed (an API outage), so a
/// single bad keyword can't sink the whole harvest.
pub async fn harvest_by_keywords(
    client: &PublicAtpClient,
    keywords: &[&str],
    per_keyword: usize,
) -> Result<Vec<String>> {
    let mut all_dids: Vec<String> = Vec::new();
    let mut successful = 0usize;
    let mut last_err: Option<anyhow::Error> = None;

    for keyword in keywords {
        match search_posts_for_authors(client, keyword, per_keyword).await {
            Ok(dids) => {
                all_dids.extend(dids);
                successful += 1;
            }
            Err(e) => {
                warn!(keyword, error = %e, "Seed keyword search failed, skipping");
                last_err = Some(e);
            }
        }
    }

    if successful == 0 && !keywords.is_empty() {
        return Err(last_err.unwrap_or_else(|| {
            anyhow!(
                "All seed keyword searches failed ({} keywords)",
                keywords.len()
            )
        }));
    }

    // No prior set to exclude against — pass an empty filter so we just dedup
    // authors who appeared under multiple keywords.
    let deduped = deduplicate_dids(&all_dids, &HashSet::new());
    info!(
        keywords = keywords.len(),
        successful,
        raw = all_dids.len(),
        unique = deduped.len(),
        "Seed-keyword harvest complete"
    );
    Ok(deduped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_keywords_are_present_and_nontrivial() {
        assert!(!SEED_KEYWORDS.is_empty());
        // Every keyword should be long enough to survive the >= 3 char filter
        // in extract_search_keywords / be a meaningful query.
        assert!(SEED_KEYWORDS.iter().all(|k| k.chars().count() >= 3));
    }

    #[test]
    fn seed_keywords_have_no_duplicates() {
        let unique: HashSet<&&str> = SEED_KEYWORDS.iter().collect();
        assert_eq!(unique.len(), SEED_KEYWORDS.len());
    }
}
