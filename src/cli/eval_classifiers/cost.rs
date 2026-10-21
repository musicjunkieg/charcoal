//! Cost estimates and the reservation budget. A model nobody priced must not
//! run as if it were free (Codex review). For CoPE-B, real spend is bounded
//! by the RunPod client's own cost meter (models.rs); this budget bounds
//! priced-per-token models and keeps the printed estimate honest.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    #[serde(default)]
    pub usd_per_post: Option<f64>,
    #[serde(default)]
    pub usd_per_million_input_tokens: Option<f64>,
    /// Characters every request carries besides the post — e.g. the policy a
    /// decision model is given as its instruction.
    #[serde(default)]
    pub overhead_chars: usize,
}

impl Price {
    /// Does this model cost money at all? Decided from the configured price,
    /// NOT from one batch's estimate: a per-token model's estimate for an
    /// empty post can come out as $0, yet calling it still wakes a paid
    /// backend. Mirrors `estimate_usd`, where a per-post price wins.
    pub fn is_paid(&self) -> bool {
        match self.usd_per_post {
            Some(per_post) => per_post > 0.0,
            None => self.usd_per_million_input_tokens.unwrap_or(0.0) > 0.0,
        }
    }
}

pub fn load_pricing(path: &Path) -> Result<HashMap<String, Price>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let prices: HashMap<String, Price> =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    for (model, p) in &prices {
        if p.usd_per_post.is_none() && p.usd_per_million_input_tokens.is_none() {
            bail!(
                "{}: {model:?} has no price (use usd_per_post: 0.0 for a free model)",
                path.display()
            );
        }
        // Validate all present prices are finite and non-negative.
        if let Some(price) = p.usd_per_post {
            if !price.is_finite() || price < 0.0 {
                bail!(
                    "{}: {model:?} has invalid usd_per_post ({price}); must be finite and >= 0.0",
                    path.display()
                );
            }
        }
        if let Some(price) = p.usd_per_million_input_tokens {
            if !price.is_finite() || price < 0.0 {
                bail!(
                    "{}: {model:?} has invalid usd_per_million_input_tokens ({price}); must be finite and >= 0.0",
                    path.display()
                );
            }
        }
    }
    Ok(prices)
}

pub fn price_for<'a>(prices: &'a HashMap<String, Price>, model: &str) -> Result<&'a Price> {
    prices.get(model).with_context(|| {
        format!("{model:?} has no entry in pricing.json — refusing to run an unpriced model")
    })
}

/// ~4 characters per token: a rough English average.
pub fn estimate_usd(price: &Price, input_chars: usize) -> f64 {
    if let Some(per_post) = price.usd_per_post {
        return per_post;
    }
    let tokens = (input_chars + price.overhead_chars) as f64 / 4.0;
    price.usd_per_million_input_tokens.unwrap_or(0.0) * tokens / 1_000_000.0
}

#[derive(Debug, Clone)]
pub struct Budget {
    cap_usd: f64,
    spent_usd: f64,
    /// Set by [`Budget::exhaust`]. Kept apart from `spent_usd` so the
    /// estimate the CLI prints stays the real reservations (Codex review #4).
    exhausted: bool,
}

impl Budget {
    pub fn new(cap_usd: f64) -> Self {
        // Clamp non-finite or negative caps to 0.0 so they admit only $0 calls.
        // This ensures the budget fails closed: a bad cap never allows positive spend.
        let cap_usd = if cap_usd.is_finite() && cap_usd >= 0.0 {
            cap_usd
        } else {
            0.0
        };
        Self {
            cap_usd,
            spent_usd: 0.0,
            exhausted: false,
        }
    }

    /// Would `try_spend(usd)` succeed right now? Reserves nothing. The runner
    /// asks this before waking a paid backend, so a budget that cannot cover
    /// even the first batch never pays for the wake-up itself.
    pub fn can_afford(&self, usd: f64) -> bool {
        // Fail closed: negative or non-finite amounts are never affordable.
        if !(usd.is_finite() && usd >= 0.0) {
            return false;
        }
        // Once exhausted, only free calls pass.
        if self.exhausted && usd > 0.0 {
            return false;
        }
        self.spent_usd + usd <= self.cap_usd
    }

    /// Reserve `usd` before a call. `false` means the call must not be made.
    /// Fails closed: negative or non-finite amounts are always refused.
    pub fn try_spend(&mut self, usd: f64) -> bool {
        // One rule set, shared with `can_afford`, so the two cannot drift.
        if !self.can_afford(usd) {
            return false;
        }
        self.spent_usd += usd;
        true
    }

    /// Mark the whole cap as used, without changing the estimate in
    /// [`Budget::spent`]. Called when a model stops at the cap:
    /// this budget only tracks ESTIMATES, and a backend's real meter (CoPE-B
    /// counts actual GPU seconds) can reach the cap while the estimates are
    /// still small. Exhausting it means no later paid model can be woken on
    /// the strength of headroom that may not really exist; $0 models (local
    /// Detoxify) still pass `can_afford(0.0)`.
    pub fn exhaust(&mut self) {
        self.exhausted = true;
    }

    /// Is there no headroom left at all? A paid model is refused once this is
    /// true, even for a call estimated at $0 (see [`Price::is_paid`]). A cap
    /// of $0 counts as exhausted from the start: it allows free models only.
    pub fn is_exhausted(&self) -> bool {
        self.exhausted || self.spent_usd >= self.cap_usd
    }

    /// The estimated cost of the calls reserved so far. Exhaustion does not
    /// change it: this is what the CLI reports as estimated spend.
    pub fn spent(&self) -> f64 {
        self.spent_usd
    }

    /// How much of the cap is no longer available: the estimate so far, or
    /// the whole cap once exhausted. A later model's real meter is sized from
    /// this, so an exhausted budget leaves it no headroom.
    pub fn committed(&self) -> f64 {
        if self.exhausted {
            self.cap_usd
        } else {
            self.spent_usd
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn per_tok(price: f64, overhead: usize) -> Price {
        Price {
            usd_per_post: None,
            usd_per_million_input_tokens: Some(price),
            overhead_chars: overhead,
        }
    }

    #[test]
    fn estimates_cover_the_whole_request() {
        let per_post = Price {
            usd_per_post: Some(0.001),
            usd_per_million_input_tokens: None,
            overhead_chars: 0,
        };
        assert!((estimate_usd(&per_post, 4_000) - 0.001).abs() < 1e-12);
        // 4,000 chars of post + 12,000 chars of policy = 16,000 chars ≈ 4,000 tokens.
        assert!((estimate_usd(&per_tok(0.042, 12_000), 4_000) - 0.000_168).abs() < 1e-12);
    }

    #[test]
    fn the_budget_refuses_a_call_that_would_cross_the_cap() {
        let mut b = Budget::new(1.0);
        assert!(b.try_spend(0.6));
        assert!(!b.try_spend(0.5));
        assert!((b.spent() - 0.6).abs() < 1e-12);
        assert!(b.try_spend(0.4));
    }

    /// Codex review: an unpriced model must be refused, never run as free.
    #[test]
    fn unpriced_models_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pricing.json");
        std::fs::write(
            &p,
            r#"{"cope-b":{"usd_per_post":0.00013},"detoxify":{"usd_per_post":0.0}}"#,
        )
        .unwrap();
        let prices = load_pricing(&p).unwrap();
        assert!(price_for(&prices, "cope-b").is_ok());
        assert!(
            price_for(&prices, "detoxify").is_ok(),
            "explicitly free is fine"
        );
        let err = format!("{:#}", price_for(&prices, "jev").unwrap_err());
        assert!(err.contains("jev") && err.contains("pricing.json"), "{err}");
        std::fs::write(&p, r#"{"x":{}}"#).unwrap();
        assert!(
            load_pricing(&p).is_err(),
            "an entry with no price is a mistake, not free"
        );
    }

    #[test]
    fn pricing_rejects_negative_usd_per_post() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pricing.json");
        std::fs::write(&p, r#"{"model":{"usd_per_post":-0.001}}"#).unwrap();
        let err = format!("{:#}", load_pricing(&p).unwrap_err());
        assert!(
            err.contains("model") && (err.contains("invalid") || err.contains("negative")),
            "negative usd_per_post must be rejected: {err}"
        );
    }

    #[test]
    fn pricing_rejects_negative_usd_per_million_input_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pricing.json");
        std::fs::write(&p, r#"{"model":{"usd_per_million_input_tokens":-0.001}}"#).unwrap();
        let err = format!("{:#}", load_pricing(&p).unwrap_err());
        assert!(
            err.contains("model") && (err.contains("invalid") || err.contains("negative")),
            "negative usd_per_million_input_tokens must be rejected: {err}"
        );
    }

    #[test]
    fn budget_spend_to_exactly_the_cap_is_accepted() {
        let mut b = Budget::new(1.0);
        assert!(
            b.try_spend(1.0),
            "spending exactly the cap should be accepted"
        );
        assert!((b.spent() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn budget_zero_spend_at_cap_is_accepted() {
        let mut b = Budget::new(1.0);
        assert!(b.try_spend(1.0));
        assert!(
            b.try_spend(0.0),
            "$0 reservation at the cap should be accepted (e.g., Detoxify)"
        );
        assert!((b.spent() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn budget_refuses_single_reservation_larger_than_cap() {
        let mut b = Budget::new(1.0);
        assert!(
            !b.try_spend(2.0),
            "reservation larger than cap must be refused"
        );
        assert_eq!(b.spent(), 0.0, "spent must remain unchanged after refusal");
    }

    #[test]
    fn budget_refuses_negative_reservation() {
        let mut b = Budget::new(1.0);
        assert!(!b.try_spend(-0.1), "negative reservation must be refused");
        assert_eq!(b.spent(), 0.0, "spent must remain unchanged");
    }

    #[test]
    fn budget_refuses_nan_reservation() {
        let mut b = Budget::new(1.0);
        assert!(!b.try_spend(f64::NAN), "NaN reservation must be refused");
        assert_eq!(b.spent(), 0.0, "spent must remain unchanged");
    }

    #[test]
    fn budget_with_nan_cap_admits_no_positive_spend() {
        let mut b = Budget::new(f64::NAN);
        assert!(!b.try_spend(0.001), "NaN cap must refuse positive spend");
        assert_eq!(b.spent(), 0.0);
    }

    #[test]
    fn budget_with_negative_cap_admits_no_positive_spend() {
        let mut b = Budget::new(-1.0);
        assert!(
            !b.try_spend(0.001),
            "negative cap must refuse positive spend"
        );
        assert_eq!(b.spent(), 0.0);
    }

    /// Final review fix 1: the runner asks "would the first batch fit?" before
    /// waking a paid GPU, without reserving anything. The answer must follow
    /// exactly the same fail-closed rules as `try_spend`.
    #[test]
    fn an_exhausted_budget_admits_only_free_calls() {
        let mut b = Budget::new(1.0);
        assert!(b.try_spend(0.1));
        b.exhaust();
        assert!(!b.can_afford(0.0001), "no paid call after exhaustion");
        assert!(!b.try_spend(0.0001), "no paid reservation either");
        assert!(b.can_afford(0.0), "a free model still runs");
    }

    /// Codex review #4: exhausting the budget must not rewrite the estimate.
    /// `spent()` is what the CLI prints as "estimated spend", so it stays the
    /// real reservations; `committed()` (what later meters are sized from)
    /// reports the whole cap as gone.
    #[test]
    fn exhausting_keeps_the_real_estimate() {
        let mut b = Budget::new(1.0);
        assert!(b.try_spend(0.8));
        assert!((b.committed() - 0.8).abs() < 1e-12, "not exhausted yet");
        b.exhaust();
        assert!((b.spent() - 0.8).abs() < 1e-12, "the estimate is unchanged");
        assert_eq!(b.committed(), 1.0, "no headroom for a later meter");
        assert!(b.is_exhausted());
    }

    /// Codex review #2, item 1: "paid" comes from the configured price; a
    /// per-post price wins over a per-token one, as in `estimate_usd`.
    #[test]
    fn paid_is_decided_by_the_configured_price() {
        assert!(!Price::default().is_paid(), "no price at all = free");
        assert!(per_tok(0.042, 0).is_paid());
        assert!(!per_tok(0.0, 0).is_paid());
        let free_per_post = Price {
            usd_per_post: Some(0.0),
            ..per_tok(5.0, 0)
        };
        assert!(!free_per_post.is_paid(), "the per-post price wins");
        assert!(Budget::new(0.0).is_exhausted(), "a $0 cap admits free only");
        let mut b = Budget::new(1.0);
        assert!(!b.is_exhausted());
        b.exhaust();
        assert!(b.is_exhausted());
    }

    #[test]
    fn can_afford_applies_try_spends_rules_without_reserving() {
        let mut b = Budget::new(1.0);
        assert!(b.can_afford(1.0), "exactly the cap fits");
        assert!(!b.can_afford(1.01), "over the cap does not");
        assert!(!b.can_afford(-0.1), "negative refused");
        assert!(!b.can_afford(f64::NAN), "NaN refused");
        assert!(!b.can_afford(f64::INFINITY), "infinity refused");
        assert_eq!(b.spent(), 0.0, "asking reserves nothing");
        assert!(b.try_spend(0.6));
        assert!(!b.can_afford(0.5));
        assert!(b.can_afford(0.4));
        assert!((b.spent() - 0.6).abs() < 1e-12);
        let nan_cap = Budget::new(f64::NAN);
        assert!(!nan_cap.can_afford(0.001));
        assert!(nan_cap.can_afford(0.0));
        // Agreement with try_spend across a spread of amounts.
        for usd in [0.0, 0.1, 0.4, 0.40001, 1.0, -1.0, f64::NAN] {
            let mut probe = b.clone();
            assert_eq!(b.can_afford(usd), probe.try_spend(usd), "amount {usd}");
        }
    }

    #[test]
    fn budget_with_nan_cap_allows_zero_spend() {
        let mut b = Budget::new(f64::NAN);
        assert!(b.try_spend(0.0), "even NaN cap must allow $0 spend");
    }

    #[test]
    fn pricing_rejects_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pricing.json");
        std::fs::write(
            &p,
            r#"{"model":{"usd_per_post":0.001,"overhead_char":100}}"#,
        )
        .unwrap();
        let err = format!("{:#}", load_pricing(&p).unwrap_err());
        assert!(
            err.contains("unknown field"),
            "typo like overhead_char (not overhead_chars) must be rejected: {err}"
        );
    }

    #[test]
    fn pricing_still_loads_known_fields() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pricing.json");
        std::fs::write(
            &p,
            r#"{"cope-b":{"usd_per_post":0.00013,"overhead_chars":500}}"#,
        )
        .unwrap();
        let prices = load_pricing(&p).expect("valid pricing should load");
        let price = &prices["cope-b"];
        assert_eq!(price.usd_per_post, Some(0.00013));
        assert_eq!(price.overhead_chars, 500);
    }
}
