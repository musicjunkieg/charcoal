// Threat-graph expansion — targeted follower walk from known-hostile accounts.
//
// Secondary discovery mechanism. When an account scores High or Elevated,
// their followers are higher-signal than random second-degree followers.
// Hostile accounts cluster: a person who follows three known-High accounts
// is more likely to be a threat.

use crate::db::models::ThreatTier;

/// Filter accounts to only those worth expanding (High or Elevated tier).
pub fn filter_expansion_candidates<'a>(accounts: &'a [(&'a str, ThreatTier)]) -> Vec<&'a str> {
    accounts
        .iter()
        .filter(|(_, tier)| matches!(tier, ThreatTier::High | ThreatTier::Elevated))
        .map(|(did, _)| *did)
        .collect()
}
