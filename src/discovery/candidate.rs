// Candidate accounts — the merged output of the harvesting sources.
//
// The firehose sampler and the seed-keyword harvester each produce a list of
// author DIDs. This module unions them into a deduplicated set of `Candidate`s,
// recording which source(s) each DID came from. Provenance matters downstream:
// firehose-only candidates represent the activity-weighted baseline, topic-only
// candidates the targeted at-risk population, and accounts found by *both* are
// the strongest signal of an active account in a sensitive topic area.

use std::collections::BTreeMap;

use serde::Serialize;

/// Which harvesting source(s) surfaced a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CandidateSource {
    /// Sampled from the Jetstream firehose only (active poster).
    Firehose,
    /// Found via topic-keyword search only (sensitive topic area).
    Topic,
    /// Surfaced by both sources.
    Both,
}

impl CandidateSource {
    /// Combine two source attributions for the same DID. Seeing a DID from both
    /// a single source twice keeps that source; seeing it from different sources
    /// promotes it to `Both`.
    fn merge(self, other: CandidateSource) -> CandidateSource {
        if self == other {
            self
        } else {
            CandidateSource::Both
        }
    }
}

/// A candidate account to (later) measure for Zentropi call volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub did: String,
    pub source: CandidateSource,
}

/// Merge firehose and topic-search DID lists into a deduplicated candidate set.
///
/// Input lists may contain duplicates internally; the output has exactly one
/// entry per DID with its combined source attribution. Order is deterministic
/// (sorted by DID) so repeated runs and snapshot tests are stable.
pub fn merge_candidates(firehose: &[String], topic: &[String]) -> Vec<Candidate> {
    let mut by_did: BTreeMap<String, CandidateSource> = BTreeMap::new();

    for did in firehose {
        by_did
            .entry(did.clone())
            .and_modify(|s| *s = s.merge(CandidateSource::Firehose))
            .or_insert(CandidateSource::Firehose);
    }
    for did in topic {
        by_did
            .entry(did.clone())
            .and_modify(|s| *s = s.merge(CandidateSource::Topic))
            .or_insert(CandidateSource::Topic);
    }

    by_did
        .into_iter()
        .map(|(did, source)| Candidate { did, source })
        .collect()
}

/// Count candidates by source — a quick summary for the harvest report.
/// Returns `(firehose_only, topic_only, both)`.
pub fn source_breakdown(candidates: &[Candidate]) -> (usize, usize, usize) {
    let mut firehose_only = 0;
    let mut topic_only = 0;
    let mut both = 0;
    for c in candidates {
        match c.source {
            CandidateSource::Firehose => firehose_only += 1,
            CandidateSource::Topic => topic_only += 1,
            CandidateSource::Both => both += 1,
        }
    }
    (firehose_only, topic_only, both)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn merges_disjoint_sources() {
        let c = merge_candidates(&s(&["did:a"]), &s(&["did:b"]));
        assert_eq!(c.len(), 2);
        // Sorted by DID.
        assert_eq!(c[0].did, "did:a");
        assert_eq!(c[0].source, CandidateSource::Firehose);
        assert_eq!(c[1].did, "did:b");
        assert_eq!(c[1].source, CandidateSource::Topic);
    }

    #[test]
    fn overlapping_did_becomes_both() {
        let c = merge_candidates(&s(&["did:x"]), &s(&["did:x"]));
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].source, CandidateSource::Both);
    }

    #[test]
    fn deduplicates_within_a_single_source() {
        let c = merge_candidates(&s(&["did:a", "did:a", "did:a"]), &[]);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].source, CandidateSource::Firehose);
    }

    #[test]
    fn breakdown_counts_each_source() {
        let c = merge_candidates(&s(&["did:a", "did:c"]), &s(&["did:b", "did:c"]));
        // a=firehose, b=topic, c=both
        assert_eq!(source_breakdown(&c), (1, 1, 1));
    }

    #[test]
    fn empty_inputs_produce_no_candidates() {
        assert!(merge_candidates(&[], &[]).is_empty());
    }

    #[test]
    fn candidate_serializes_source_lowercase() {
        let c = Candidate {
            did: "did:a".to_string(),
            source: CandidateSource::Both,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains(r#""source":"both""#), "got: {json}");
    }
}
