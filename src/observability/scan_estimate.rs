// What a full scan would cost on the RunPod bill, from how many posts it
// would send to the GPU (#424).
//
// `charcoal estimate` counts those posts for free (it runs the real scan up to
// the classification burst and stops). This turns the count into a dollar
// range for budgeting.
//
// Calibration (RunPod's own bill, not Charcoal's meter, which reads 3-5x low):
//   - 2026-11-03, staging, 1,329 posts:  $0.357 (after #423's single cold boot)
//   - 2026-10-30, staging, 15,503 posts: $1.62
//   - 2026-11-01, staging, 20,195 posts: $2.37
// A straight line through these needs a fixed part (waking one GPU worker
// and its idle tail) plus a per-post part, and no single line fits all three,
// so this returns a LOW and a HIGH bound rather than one number. Re-fit both
// when a new scan's bill falls outside them.

/// Cost of waking the GPU for a scan, in cents: low and high bound.
const FIXED_CENTS: (u64, u64) = (20, 30);
/// Cost per 1,000 posts classified, in cents: low and high bound.
const PER_THOUSAND_CENTS: (u64, u64) = (7, 12);

/// The billed cost range, in whole cents, for a scan that sends `posts` posts
/// to the GPU. Zero posts costs nothing: the GPU is only contacted on the first
/// post that needs it (#394/#423), so a scan with nothing to classify never
/// wakes a worker.
pub fn estimated_cost_cents(posts: u64) -> (u64, u64) {
    if posts == 0 {
        return (0, 0);
    }
    let low = FIXED_CENTS
        .0
        .saturating_add(posts.saturating_mul(PER_THOUSAND_CENTS.0) / 1000);
    // Round the high bound UP so it never understates a partial thousand.
    let high = FIXED_CENTS
        .1
        .saturating_add(posts.saturating_mul(PER_THOUSAND_CENTS.1).div_ceil(1000));
    (low, high)
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        // Invariant: the range is a real range: low never exceeds high.
        #[test]
        fn low_never_exceeds_high(posts in any::<u64>()) {
            let (low, high) = estimated_cost_cents(posts);
            prop_assert!(low <= high);
        }

        // Invariant: more posts never cost less, at either bound.
        #[test]
        fn more_posts_never_cost_less(a in 0u64..10_000_000, extra in 0u64..10_000_000) {
            let (la, ha) = estimated_cost_cents(a);
            let (lb, hb) = estimated_cost_cents(a + extra);
            prop_assert!(lb >= la && hb >= ha);
        }

        // Invariant: any scan that classifies at least one post pays at least
        // the fixed cost of waking the GPU, and the high bound pays at least
        // the high fixed cost plus every started thousand at the high rate.
        #[test]
        fn any_classification_pays_the_wake_up(posts in 1u64..10_000_000) {
            let (low, high) = estimated_cost_cents(posts);
            prop_assert!(low >= FIXED_CENTS.0);
            prop_assert!(high >= FIXED_CENTS.1 + posts * PER_THOUSAND_CENTS.1 / 1000);
            prop_assert!(high > FIXED_CENTS.1, "a started thousand is never free at the high bound");
        }
    }

    // Invariant: a scan with nothing to classify never wakes the GPU, so it
    // costs nothing. Not a property over inputs: zero is the one input.
    #[test]
    fn nothing_to_classify_costs_nothing() {
        assert_eq!(estimated_cost_cents(0), (0, 0));
    }

    // Invariant: the three calibration bills fall inside the range. If a
    // re-fit of the constants pushes one outside, this fails and says which.
    #[test]
    fn the_calibration_bills_fall_inside_the_range() {
        for (posts, billed_cents) in [(1_329u64, 36u64), (15_503, 162), (20_195, 237)] {
            let (low, high) = estimated_cost_cents(posts);
            assert!(
                low <= billed_cents && billed_cents <= high,
                "{posts} posts billed {billed_cents}c, outside {low}-{high}c"
            );
        }
    }
}
