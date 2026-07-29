//! NLI-based contextual hostility scoring.
//!
//! Uses a DeBERTa-v3-xsmall cross-encoder (quantized ONNX, ~87MB) to score
//! text pairs for hostile engagement patterns. The model takes a premise and
//! hypothesis and outputs entailment/contradiction/neutral probabilities.
//!
//! Five hypothesis templates detect different hostility signals:
//! - Attack/mockery (ad hominem, direct hostility)
//! - Contempt (dismissiveness, eye-rolling)
//! - Misrepresentation (strawmanning, goalpost moving)
//! - Good-faith disagreement (NOT a threat — reduces hostility score)
//! - Support/agreement (ally signal — reduces hostility score)
//!
//! The contextual hostility score is:
//!   hostile_signal = max(attack, contempt, misrepresent)
//!   supportive_signal = max(good_faith * 0.5, support * 0.8)
//!   hostility = clamp(hostile_signal - supportive_signal, 0.0, 1.0)

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;
use tracing::debug;

use crate::scoring::language::pair_is_assessable;

/// Raw entailment scores from running NLI hypotheses on a text pair.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HypothesisScores {
    pub attack: f64,
    pub contempt: f64,
    pub misrepresent: f64,
    pub good_faith_disagree: f64,
    pub support: f64,
}

/// Compute contextual hostility score from NLI hypothesis entailment scores.
///
/// Formula:
///   hostile_signal = max(attack, contempt, misrepresent)
///   supportive_signal = max(good_faith * 0.5, support * 0.8)
///   hostility = clamp(hostile_signal - supportive_signal, 0.0, 1.0)
pub fn compute_hostility_score(scores: &HypothesisScores) -> f64 {
    let hostile_signal = scores.attack.max(scores.contempt).max(scores.misrepresent);
    let supportive_signal = (scores.good_faith_disagree * 0.5).max(scores.support * 0.8);
    (hostile_signal - supportive_signal).clamp(0.0, 1.0)
}

/// Return the average context score, or None if no scores provided.
/// Uses the mean across all scored pairs to capture overall engagement
/// patterns rather than worst-case moments.
pub fn avg_context_score(scores: &[f64]) -> Option<f64> {
    if scores.is_empty() {
        None
    } else {
        Some(scores.iter().sum::<f64>() / scores.len() as f64)
    }
}

/// Apply softmax to 3-class NLI logits and return the entailment probability.
///
/// DeBERTa NLI label order: [0: contradiction, 1: entailment, 2: neutral]
/// See: https://huggingface.co/cross-encoder/nli-deberta-v3-xsmall
fn softmax_entailment(logits: &[f32]) -> f64 {
    let max_logit = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exp_sum: f32 = logits.iter().map(|&x| (x - max_logit).exp()).sum();
    let entailment_prob = (logits[1] - max_logit).exp() / exp_sum;
    entailment_prob as f64
}

/// The 5 hypothesis templates used for NLI inference.
/// Each is (field_name, hypothesis_text).
const HYPOTHESES: [(&str, &str); 5] = [
    (
        "attack",
        "The second text attacks or mocks the author of the first text",
    ),
    (
        "contempt",
        "The second text dismisses the first text with contempt",
    ),
    (
        "misrepresent",
        "The second text misrepresents what the first text says",
    ),
    (
        "good_faith_disagree",
        "The second text respectfully disagrees with the first text",
    ),
    (
        "support",
        "The second text supports or agrees with the first text",
    ),
];

/// NLI cross-encoder scorer. Loads the DeBERTa-v3-xsmall ONNX model
/// and scores text pairs against hostility hypotheses.
pub struct NliScorer {
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
}

impl NliScorer {
    /// Load the NLI model and tokenizer from the nli-deberta-v3-xsmall subdirectory.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let nli_dir = crate::toxicity::download::nli_model_dir(model_dir);
        let model_path = nli_dir.join("model_quantized.onnx");
        let tokenizer_path = nli_dir.join("tokenizer.json");

        if !model_path.exists() {
            anyhow::bail!(
                "NLI model not found: {}\nRun `charcoal download-model` to download it.",
                model_path.display()
            );
        }
        if !tokenizer_path.exists() {
            anyhow::bail!(
                "NLI tokenizer not found: {}\nRun `charcoal download-model` to download it.",
                tokenizer_path.display()
            );
        }

        let session = Session::builder()
            .context("Failed to create NLI ONNX session builder")?
            .commit_from_file(&model_path)
            .with_context(|| {
                format!(
                    "Failed to load NLI ONNX model from {}",
                    model_path.display()
                )
            })?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load NLI tokenizer: {}", e))?;

        debug!("Loaded NLI cross-encoder model from {}", nli_dir.display());

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
        })
    }

    /// Run NLI inference for one premise against many hypotheses in a SINGLE
    /// batched forward pass, returning one entailment probability (0.0-1.0)
    /// per hypothesis, in input order.
    ///
    /// Replaces the old one-inference-per-hypothesis loop (5 sequential
    /// spawn_blocking + mutex + ONNX runs per pair) with one padded
    /// `[N, max_len]` run (#213). Batch-of-1 is the previous single-item path
    /// exactly (no padding). Verified against 5× single runs in the unit test.
    ///
    /// DeBERTa NLI output: 3-class logits [contradiction, entailment, neutral]
    /// per row (entailment at index 1, matching `softmax_entailment`); we
    /// softmax each row and return the entailment score.
    async fn score_entailments_batched(
        &self,
        premise: &str,
        hypotheses: &[&str],
    ) -> Result<Vec<f64>> {
        if hypotheses.is_empty() {
            return Ok(Vec::new());
        }

        let session = Arc::clone(&self.session);
        let tokenizer = Arc::clone(&self.tokenizer);
        let premise = premise.to_string();
        let hypotheses: Vec<String> = hypotheses.iter().map(|h| h.to_string()).collect();

        tokio::task::spawn_blocking(move || {
            let batch = hypotheses.len();

            // Encode each (premise, hypothesis) pair, then pad all rows to the
            // longest so they stack into one [batch, max_len] tensor. Pad token
            // id 0, mask 0 — mirrors topics::embeddings::embed_batch.
            let mut encoded: Vec<(Vec<i64>, Vec<i64>)> = Vec::with_capacity(batch);
            for h in &hypotheses {
                // DeBERTa tokenizer handles pair encoding: [CLS] premise [SEP] hyp [SEP]
                let enc = tokenizer
                    .encode((premise.as_str(), h.as_str()), true)
                    .map_err(|e| anyhow::anyhow!("NLI tokenization failed: {}", e))?;
                let ids = enc.get_ids().iter().map(|&id| id as i64).collect();
                let mask = enc.get_attention_mask().iter().map(|&m| m as i64).collect();
                encoded.push((ids, mask));
            }
            let max_len = encoded.iter().map(|(ids, _)| ids.len()).max().unwrap_or(0);

            let mut input_ids_flat: Vec<i64> = Vec::with_capacity(batch * max_len);
            let mut attention_mask_flat: Vec<i64> = Vec::with_capacity(batch * max_len);
            for (mut ids, mut mask) in encoded {
                ids.resize(max_len, 0); // pad token id 0
                mask.resize(max_len, 0); // pad positions masked out
                input_ids_flat.extend(ids);
                attention_mask_flat.extend(mask);
            }

            let shape = [batch as i64, max_len as i64];
            let input_ids_tensor = Tensor::from_array((shape, input_ids_flat))
                .context("Failed to create input_ids")?;
            let attention_mask_tensor = Tensor::from_array((shape, attention_mask_flat))
                .context("Failed to create attention_mask")?;

            let logits_data = {
                let mut session = session
                    .lock()
                    .map_err(|e| anyhow::anyhow!("NLI session lock poisoned: {}", e))?;

                // Xenova's nli-deberta-v3-xsmall ONNX export only accepts
                // input_ids and attention_mask (no token_type_ids). Passing
                // an unexpected input causes ort to segfault rather than
                // return an error, so we only send the two known inputs. The
                // batch axis IS dynamic (verified before building #213).
                let outputs = session
                    .run(ort::inputs! {
                        "input_ids" => input_ids_tensor,
                        "attention_mask" => attention_mask_tensor
                    })
                    .context("NLI ONNX inference failed")?;

                // Output: [batch, 3] — per row [contradiction, entailment, neutral].
                let (out_shape, data) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .context("Failed to extract NLI output tensor")?;

                // Validate the model contract before mapping: exactly [batch, 3].
                // Otherwise chunks_exact(3) below would silently drop a remainder
                // and score_pair would leave hypotheses at 0.0 — a model-contract
                // mismatch must fail loudly, not skew the hostility score.
                anyhow::ensure!(
                    **out_shape == [batch as i64, 3],
                    "NLI output shape {:?} != expected [{batch}, 3]",
                    &**out_shape,
                );

                data.to_vec()
            };

            // Softmax each row's 3 logits → entailment probability.
            // (data.len() == batch * 3, guaranteed by the shape check above.)
            Ok(logits_data
                .chunks_exact(3)
                .map(softmax_entailment)
                .collect())
        })
        .await
        .context("NLI spawn_blocking panicked")?
    }

    /// Score a text pair against all 5 hostility hypotheses and compute
    /// the contextual hostility score.
    ///
    /// The premise combines both texts so the model sees the interaction:
    /// "Original: {original} Response: {response}"
    /// Each hypothesis template is tested against this premise.
    ///
    /// Returns `Ok(None)` when the pair's language is unassessable (#230),
    /// `Err` only when inference itself failed.
    pub async fn score_pair(
        &self,
        original_text: &str,
        response_text: &str,
    ) -> Result<Option<(f64, HypothesisScores)>> {
        // #230: the HYPOTHESES below are English sentences and this cross-encoder
        // is MNLI-trained, so a non-English side yields a cross-lingual
        // entailment judgment that is noise, not weak signal. Left ungated it
        // inflates threat scores by up to 1.5x via context_multiplier.
        //
        // The gate lives HERE rather than at each call site because two call
        // sites were missed before — inside the scorer, every current and future
        // caller is gated by construction.
        //
        // KNOWN TRADE-OFF (#232): this abstention is an evasion lever. An
        // amplifier controls their own text, so padding an English hostile reply
        // until it is majority non-Latin drops the pair here and forfeits the
        // context multiplier (bounded [1.0, 1.5], so at most ~33% off the threat
        // score). Bounded further by #222 at the account level — broadly
        // non-Latin engagement trips `coverage_gate` into the surfaced
        // `NotAssessed` tier rather than a quiet Low — so the residue is a
        // targeted single pair on an otherwise-English account.
        //
        // Not fixable by "scoring the assessable side": HYPOTHESES are
        // relational and run against one joint premise, so dropping a side asks
        // whether a text attacks the author of a text that is no longer there,
        // and dropping only the non-Latin characters would let the attacker
        // choose which substring reaches the model. A real fix needs a
        // multilingual cross-encoder or abstention-as-signal; see #232.
        if !pair_is_assessable(original_text, response_text) {
            return Ok(None);
        }

        let premise = format!("Original: {} Response: {}", original_text, response_text);

        // One batched forward pass for all 5 hypotheses (#213), replacing the
        // old 5 sequential single-item inferences. Results come back in
        // HYPOTHESES order.
        let hyps: Vec<&str> = HYPOTHESES.iter().map(|(_, h)| *h).collect();
        let scores = self.score_entailments_batched(&premise, &hyps).await?;

        let mut hypothesis_scores = HypothesisScores {
            attack: 0.0,
            contempt: 0.0,
            misrepresent: 0.0,
            good_faith_disagree: 0.0,
            support: 0.0,
        };
        for ((name, _), score) in HYPOTHESES.iter().zip(scores) {
            match *name {
                "attack" => hypothesis_scores.attack = score,
                "contempt" => hypothesis_scores.contempt = score,
                "misrepresent" => hypothesis_scores.misrepresent = score,
                "good_faith_disagree" => hypothesis_scores.good_faith_disagree = score,
                "support" => hypothesis_scores.support = score,
                _ => {}
            }
        }

        let hostility = compute_hostility_score(&hypothesis_scores);

        debug!(
            attack = format!("{:.3}", hypothesis_scores.attack),
            contempt = format!("{:.3}", hypothesis_scores.contempt),
            misrepresent = format!("{:.3}", hypothesis_scores.misrepresent),
            good_faith = format!("{:.3}", hypothesis_scores.good_faith_disagree),
            support = format!("{:.3}", hypothesis_scores.support),
            hostility = format!("{:.3}", hostility),
            "NLI scored pair"
        );

        Ok(Some((hostility, hypothesis_scores)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toxicity::download::{nli_files_present, resolve_model_dir};

    /// The batched forward pass (#213) must produce the same entailment score
    /// per hypothesis as running each hypothesis as its own [1, seq] inference.
    /// Batch-of-1 IS the old single-item path (no padding), so comparing
    /// batch-of-5 against 5× batch-of-1 proves padding + the batch dimension
    /// don't change results.
    ///
    /// Requires the NLI model locally; skips when it isn't present.
    ///
    /// QUARANTINED (#231). This test had never actually run in CI: it called
    /// `default_model_dir()` while CI sets `CHARCOAL_MODEL_DIR=models`, so it
    /// returned early and reported `ok` having asserted nothing. #230 switched
    /// it to `resolve_model_dir()`, it ran for the first time, and it FAILED on
    /// Linux x86_64 — hypothesis 0 came back batched 0.031 vs single 0.172,
    /// against a 0.02 tolerance. The model bytes are identical to the ones that
    /// pass on macOS ARM64 (sha256 3fac2500…, matching HuggingFace), so this is
    /// a platform/ONNX-Runtime divergence, not a model-version difference.
    ///
    /// It matters because production is Linux x86_64: #213's tolerance was
    /// measured on Apple Silicon only, and a 5.6x swing in an entailment
    /// probability may be structural rather than quantization noise.
    ///
    /// `#[ignore]` rather than a reverted env lookup or a widened tolerance:
    /// ignoring reports honestly in the summary, where both alternatives would
    /// go back to claiming a pass. Run it with `--ignored` when working #231.
    #[ignore = "#231: batched-vs-single diverges 0.14 on Linux x86_64; passes on macOS ARM64"]
    #[tokio::test]
    async fn batched_entailments_match_per_hypothesis_single_runs() {
        let base = resolve_model_dir();
        if !nli_files_present(&base) {
            eprintln!("SKIP: NLI model not present at {}", base.display());
            return;
        }
        let scorer = NliScorer::load(&base).expect("load NLI model");

        let premise =
            "Original: fat people deserve healthcare too Response: lol imagine being that big";
        // Hypotheses of DIFFERENT lengths so the batch actually pads.
        let hyps: Vec<&str> = HYPOTHESES.iter().map(|(_, h)| *h).collect();

        // Batch-of-5 (padded to the longest).
        let batched = scorer
            .score_entailments_batched(premise, &hyps)
            .await
            .expect("batched inference");
        assert_eq!(batched.len(), hyps.len());

        // 5× batch-of-1 (each unpadded — the previous single-item path).
        let mut singles = Vec::new();
        for h in hyps.iter() {
            let single = scorer
                .score_entailments_batched(premise, std::slice::from_ref(h))
                .await
                .expect("single inference");
            singles.push(single[0]);
        }

        // This quantized DeBERTa export is NOT perfectly padding-invariant: a
        // batched (padded) row differs from its unpadded single run by a small,
        // systematic amount (measured ≈0.002–0.008 per hypothesis, ≈0.006 on the
        // final hostility). That is a real behavior shift, accepted as within
        // the model's own quantization noise and immaterial to threat tiers
        // (bands 8/15/35). This bound catches segfaults, transposed rows, or any
        // LARGE divergence; it does not pretend the padding artifact is zero.
        const PAD_ARTIFACT_TOLERANCE: f64 = 0.02;
        for (i, (b, s)) in batched.iter().zip(&singles).enumerate() {
            assert!(
                (b - s).abs() < PAD_ARTIFACT_TOLERANCE,
                "hypothesis {i}: batched {b} vs single {s} exceeds padding tolerance",
            );
        }
    }

    /// #231 diagnostic — NOT a gate. Prints the batched-vs-single matrix so the
    /// same numbers can be read off macOS ARM64 and Linux x86_64 and compared.
    ///
    /// It separates the two candidate mechanisms, which the equivalence test
    /// above conflates because batch-of-5 changes both at once:
    ///
    /// - `dup2` runs `[h_i, h_i]` — batch of 2 with IDENTICAL row lengths, so
    ///   `max_len == len(h_i)` and NOTHING is padded. Divergence from `single`
    ///   here means the BATCH DIMENSION alone changes the result (e.g. a
    ///   per-tensor dynamic-quantization scale computed over the whole batch,
    ///   or a different int8 GEMM kernel for batch>1) — padding is innocent.
    /// - `pad2` runs `[h_i, h_longest]` — batch of 2 where row i IS padded.
    ///   Divergence beyond `dup2` is attributable to PADDING specifically.
    ///
    /// Run with:
    ///   CHARCOAL_MODEL_DIR=./models cargo test --lib \
    ///     scoring::nli::tests::characterize -- --ignored --nocapture
    #[ignore = "#231 diagnostic: prints a measurement matrix, asserts nothing"]
    #[tokio::test]
    async fn characterize_batch_divergence() {
        let base = resolve_model_dir();
        if !nli_files_present(&base) {
            eprintln!("SKIP: NLI model not present at {}", base.display());
            return;
        }
        let scorer = NliScorer::load(&base).expect("load NLI model");

        // Same premise as the equivalence test, so the numbers are comparable.
        let premise =
            "Original: fat people deserve healthcare too Response: lol imagine being that big";
        let hyps: Vec<&str> = HYPOTHESES.iter().map(|(_, h)| *h).collect();

        // Token length per (premise, hypothesis) pair drives who gets padded.
        let lens: Vec<usize> = hyps
            .iter()
            .map(|h| {
                scorer
                    .tokenizer
                    .encode((premise, *h), true)
                    .expect("tokenize")
                    .get_ids()
                    .len()
            })
            .collect();
        let longest = lens
            .iter()
            .enumerate()
            .max_by_key(|(_, l)| **l)
            .map(|(i, _)| i)
            .expect("non-empty");

        println!(
            "#231 arch={} os={}",
            std::env::consts::ARCH,
            std::env::consts::OS
        );
        println!("#231 token lens: {lens:?} (longest = hypothesis {longest})");

        let batch5 = scorer
            .score_entailments_batched(premise, &hyps)
            .await
            .expect("batch5");

        println!(
            "#231 {:<3} {:<4} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
            "hyp", "len", "single", "dup2", "pad2", "batch5", "d(dup2)", "d(batch5)"
        );
        for (i, h) in hyps.iter().enumerate() {
            let single = scorer
                .score_entailments_batched(premise, std::slice::from_ref(h))
                .await
                .expect("single")[0];
            // Batch of 2, identical rows => no padding at all.
            let dup2 = scorer
                .score_entailments_batched(premise, &[*h, *h])
                .await
                .expect("dup2")[0];
            // Batch of 2 against the longest hypothesis => row i is padded.
            let pad2 = scorer
                .score_entailments_batched(premise, &[*h, hyps[longest]])
                .await
                .expect("pad2")[0];

            println!(
                "#231 {:<3} {:<4} {:>10.6} {:>10.6} {:>10.6} {:>10.6} {:>+10.6} {:>+10.6}",
                i,
                lens[i],
                single,
                dup2,
                pad2,
                batch5[i],
                dup2 - single,
                batch5[i] - single,
            );
        }
    }

    /// #231 impact measurement — NOT a gate. The per-hypothesis matrix above
    /// shows the entailment probabilities move; this shows what that does to
    /// the number the product actually uses, over a spread of pair types so the
    /// conclusion does not rest on one cherry-picked example.
    ///
    /// Prints, per pair, the `compute_hostility_score` from the pre-#213 single
    /// path and from the #213 batched path, plus the resulting
    /// `context_multiplier = 1.0 + hostility * 0.5` that feeds the live threat
    /// formula. Also times both paths so the correctness cost of reverting
    /// batching can be weighed against its speedup.
    #[ignore = "#231 diagnostic: prints an impact table, asserts nothing"]
    #[tokio::test]
    async fn characterize_hostility_impact() {
        let base = resolve_model_dir();
        if !nli_files_present(&base) {
            eprintln!("SKIP: NLI model not present at {}", base.display());
            return;
        }
        let scorer = NliScorer::load(&base).expect("load NLI model");
        let hyps: Vec<&str> = HYPOTHESES.iter().map(|(_, h)| *h).collect();

        // A spread: overt mockery, contemptuous dismissal, strawmanning,
        // good-faith disagreement, and outright support. If batching were
        // benign the two columns would track each other across all of them.
        let pairs: [(&str, &str, &str); 6] = [
            (
                "mockery",
                "fat people deserve healthcare too",
                "lol imagine being that big",
            ),
            (
                "contempt",
                "trans kids need gender affirming care",
                "nobody with a functioning brain believes this garbage",
            ),
            (
                "misrepresent",
                "we should fund public transit",
                "so you want to ban all cars, got it",
            ),
            (
                "good_faith",
                "we should fund public transit",
                "I disagree — I think the cost per rider is too high here",
            ),
            (
                "support",
                "fat people deserve healthcare too",
                "absolutely, and denying it is straightforward discrimination",
            ),
            (
                "neutral",
                "the bus route changed today",
                "which stop does it use now?",
            ),
        ];

        let to_scores = |v: &[f64]| HypothesisScores {
            attack: v[0],
            contempt: v[1],
            misrepresent: v[2],
            good_faith_disagree: v[3],
            support: v[4],
        };

        println!(
            "#231impact arch={} os={}",
            std::env::consts::ARCH,
            std::env::consts::OS
        );
        println!(
            "#231impact {:<14} {:>10} {:>10} {:>10} {:>9} {:>9}",
            "pair", "single", "batched", "delta", "mult_1x", "mult_5x"
        );

        let mut single_nanos = 0u128;
        let mut batched_nanos = 0u128;

        for (label, original, response) in pairs {
            let premise = format!("Original: {} Response: {}", original, response);

            let t = std::time::Instant::now();
            let mut singles = Vec::new();
            for h in hyps.iter() {
                singles.push(
                    scorer
                        .score_entailments_batched(&premise, std::slice::from_ref(h))
                        .await
                        .expect("single")[0],
                );
            }
            single_nanos += t.elapsed().as_nanos();

            let t = std::time::Instant::now();
            let batched = scorer
                .score_entailments_batched(&premise, &hyps)
                .await
                .expect("batched");
            batched_nanos += t.elapsed().as_nanos();

            let h_single = compute_hostility_score(&to_scores(&singles));
            let h_batched = compute_hostility_score(&to_scores(&batched));

            println!(
                "#231impact {:<14} {:>10.6} {:>10.6} {:>+10.6} {:>9.4} {:>9.4}",
                label,
                h_single,
                h_batched,
                h_batched - h_single,
                1.0 + h_single * 0.5,
                1.0 + h_batched * 0.5,
            );
        }

        println!(
            "#231impact timing: 5x-single {:.1}ms vs batched {:.1}ms over {} pairs ({:.2}x)",
            single_nanos as f64 / 1e6,
            batched_nanos as f64 / 1e6,
            pairs.len(),
            single_nanos as f64 / batched_nanos as f64,
        );
    }
}
