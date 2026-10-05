//! #358 phase 1 — evidence, not a fix.
//!
//! Stage 1 (`stage1_outcome`) ONNX-scores originals, replies AND quotes, but the
//! clean-pass gate that authorizes the early exit only looks at originals and
//! quotes. So an account that posts 15+ benign, off-topic originals and is
//! hostile only in its REPLIES exits as Terminal "Low" and its replies never
//! reach the Stage-2 classifier. Replies are one of the two harassment vectors
//! Charcoal exists to catch, so whether that shortcut is acceptable is a policy
//! decision for the maintainer — these tests only pin what the code does TODAY.
//!
//! Every fixture is driven through the real `stage1_outcome` (no
//! reimplementation of the gate). Tests whose name starts with
//! `current_false_negative_` document the bug: they PASS because the code is
//! wrong, and they are expected to flip (and be rewritten) if option (a) or (b)
//! from #358 is adopted.
//!
//! The three options from #358 are also evaluated per fixture, but those are
//! HYPOTHETICAL predicates defined in this file (`option_a_would_exit`,
//! `option_b_would_exit`) — they are not production code and exist only to make
//! the "what would each option do" table reproducible:
//!
//! - (a) any assessable reply with ONNX >= ONNX_CLEAN_THRESHOLD blocks the exit
//! - (b) an account whose Stage-1 sample has >= `OPTION_B_MIN_REPLIES`
//!   assessable replies may not early-exit (its replies must be judged in
//!   context, in Stage 2) — the reading of "require a minimum reply count" that
//!   does not need solo reply scores to be trustworthy
//! - (c) keep the shortcut — identical to current behaviour by definition
//!
//! Two layers:
//! 1. `ScriptedScorer` — deterministic, model-free; always runs.
//! 2. `real_onnx_*` — the same fixtures through the real Detoxify ONNX model.
//!    Model-gated: prints the `SKIP:` sentinel and asserts nothing when the
//!    model is absent. Run with `CHARCOAL_MODEL_DIR=./models`.
//!
//! Synthetic fixtures prove the MECHANISM, not a rate. Do not read calibration
//! into them (#135).

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;

use charcoal::bluesky::posts::{Post, PostSample, ReplyPost};
use charcoal::scoring::language::partition_assessable;
use charcoal::scoring::profile::{stage1_outcome, Stage1Outcome};
use charcoal::scoring::threat::ThreatWeights;
use charcoal::topics::fingerprint::{TopicCluster, TopicFingerprint};
use charcoal::toxicity::ensemble::ONNX_CLEAN_THRESHOLD;
use charcoal::toxicity::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

// ============================================================
// Fixture text
// ============================================================

/// 15 benign, topically unrelated originals: >= 15 makes the fingerprint
/// quality `Normal` (the strongest early-exit authorization) and they share no
/// vocabulary with the astrophysics protected fingerprint below.
const BENIGN_ORIGINALS: [&str; 15] = [
    "had a lovely sandwich for lunch today",
    "the weather is sunny and warm this morning",
    "watering my tomato plants in the garden",
    "made fresh coffee and read a paperback novel",
    "took the dog for a walk around the park",
    "baking bread this weekend, smells wonderful",
    "reorganized my bookshelf by color this afternoon",
    "tried a new recipe for vegetable soup",
    "went for a bike ride along the river trail",
    "planted some basil and thyme in the window box",
    "finished knitting a scarf for the winter",
    "cleaned out the garage over the weekend",
    "watched the sunset from the back porch",
    "made pancakes for breakfast with the kids",
    "repainted the fence a nice shade of blue",
];

/// Replies that are hostile ON THEIR OWN — a solo ONNX pass can see them.
const HOSTILE_REPLIES: [&str; 6] = [
    "You're a fucking idiot, go kill yourself",
    "Shut up, you worthless piece of trash",
    "Fat people like you are disgusting and should be ashamed",
    "I hope you die, you stupid bitch",
    "Nobody wants you here, you pathetic loser",
    "God you are so stupid, shut the fuck up",
];

/// Replies whose hostility lives in the PARENT — "piling on" agreement with a
/// hostile post. Benign in isolation; only Stage 2's parent+reply envelope can
/// judge them. This is the case the profile.rs:321-327 comment worries about.
const CONTEXT_DEPENDENT_REPLIES: [&str; 6] = [
    "Exactly. Couldn't have said it better.",
    "Yep, this one right here.",
    "100%. Every single word.",
    "Thank you for saying what we're all thinking.",
    "Someone finally said it.",
    "This. So much this.",
];

/// Friendly replies — the negative control. Any option that sends these to
/// Stage 2 is paying GPU for nothing.
const BENIGN_REPLIES: [&str; 6] = [
    "congrats, that looks amazing!",
    "thank you so much for sharing this",
    "happy birthday, hope it's a great one",
    "oh that recipe sounds delicious",
    "good luck with the move this weekend",
    "love this photo of the lake",
];

/// German translations of the hostile replies, tagged `langs: ["de"]`. The #222
/// language gate drops these BEFORE any scoring, because our models are
/// English-only and would read them as clean.
const GERMAN_HOSTILE_REPLIES: [&str; 6] = [
    "Du bist ein verdammter Idiot, bring dich um",
    "Halt die Klappe, du wertloses Stück Müll",
    "Fette Leute wie du sind ekelhaft und sollten sich schämen",
    "Ich hoffe, du stirbst, du dumme Schlampe",
    "Niemand will dich hier, du erbärmlicher Verlierer",
    "Gott, bist du dumm, halt endlich die Fresse",
];

const HOSTILE_ORIGINAL: &str = "Fat people are disgusting and deserve every bit of hate they get";

// ============================================================
// Builders
// ============================================================

fn post(uri: &str, text: &str, langs: &[&str]) -> Post {
    Post {
        uri: uri.to_string(),
        text: text.to_string(),
        created_at: None,
        like_count: 0,
        repost_count: 0,
        quote_count: 0,
        is_quote: false,
        langs: langs.iter().map(|l| l.to_string()).collect(),
    }
}

fn reply(i: usize, text: &str, langs: &[&str]) -> ReplyPost {
    ReplyPost {
        post: post(
            &format!("at://did:plc:t/app.bsky.feed.post/r{i}"),
            text,
            langs,
        ),
        parent_uri: format!("at://did:plc:p/app.bsky.feed.post/p{i}"),
    }
}

fn sample(originals: &[&str], replies: Vec<ReplyPost>) -> PostSample {
    let originals: Vec<Post> = originals
        .iter()
        .enumerate()
        .map(|(i, t)| {
            post(
                &format!("at://did:plc:t/app.bsky.feed.post/o{i}"),
                t,
                &["en"],
            )
        })
        .collect();
    let total = originals.len() + replies.len();
    PostSample {
        reply_ratio: replies.len() as f64 / total as f64,
        quote_ratio: 0.0,
        total_posts: total,
        originals,
        replies,
        quotes: vec![],
    }
}

fn en_replies(texts: &[&str]) -> Vec<ReplyPost> {
    texts
        .iter()
        .enumerate()
        .map(|(i, t)| reply(i, t, &["en"]))
        .collect()
}

/// A protected-user fingerprint about astrophysics, so none of the fixtures
/// overlap it — every fixture clears the topic half of the gate, isolating the
/// toxicity half under test.
fn unrelated_fingerprint() -> TopicFingerprint {
    TopicFingerprint {
        clusters: vec![TopicCluster {
            label: "astrophysics".to_string(),
            keywords: ["quasar", "nebula", "redshift", "telescope"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            keyword_scores: vec![],
            weight: 1.0,
        }],
        post_count: 100,
    }
}

// ============================================================
// Fixtures
// ============================================================

/// The fixture set. `name` is the row label in the #358 comment table.
fn fixtures() -> Vec<(&'static str, PostSample)> {
    let mut hostile_original = BENIGN_ORIGINALS.to_vec();
    hostile_original[0] = HOSTILE_ORIGINAL;

    // 3 English originals + 6 German hostile replies: unassessable posts now
    // outnumber assessable ones, so the coverage gate abstains (NotAssessed)
    // instead of guessing.
    let german_dominant_originals = &BENIGN_ORIGINALS[..3];

    vec![
        (
            "F1 benign originals + solo-hostile replies",
            sample(&BENIGN_ORIGINALS, en_replies(&HOSTILE_REPLIES)),
        ),
        (
            "F2 benign originals + context-dependent replies",
            sample(&BENIGN_ORIGINALS, en_replies(&CONTEXT_DEPENDENT_REPLIES)),
        ),
        (
            "F3 benign originals + benign replies (control)",
            sample(&BENIGN_ORIGINALS, en_replies(&BENIGN_REPLIES)),
        ),
        (
            "F4 benign originals, no replies (control)",
            sample(&BENIGN_ORIGINALS, vec![]),
        ),
        (
            "F5 one hostile original + benign replies (control)",
            sample(&hostile_original, en_replies(&BENIGN_REPLIES)),
        ),
        (
            "F6 benign EN originals + hostile DE replies",
            sample(
                &BENIGN_ORIGINALS,
                GERMAN_HOSTILE_REPLIES
                    .iter()
                    .enumerate()
                    .map(|(i, t)| reply(i, t, &["de"]))
                    .collect(),
            ),
        ),
        (
            "F7 3 EN originals + hostile DE replies (abstain)",
            sample(
                german_dominant_originals,
                GERMAN_HOSTILE_REPLIES
                    .iter()
                    .enumerate()
                    .map(|(i, t)| reply(i, t, &["de"]))
                    .collect(),
            ),
        ),
    ]
}

// ============================================================
// Scorers
// ============================================================

/// Deterministic stand-in for the ONNX model: texts in the hostile set score
/// 0.9, everything else 0.01. Mirrors what the real model does on these
/// fixtures (the `real_onnx_*` tests check that mirror holds).
struct ScriptedScorer {
    scores: HashMap<String, f64>,
}

impl ScriptedScorer {
    fn new() -> Self {
        let mut scores = HashMap::new();
        for t in HOSTILE_REPLIES
            .iter()
            .chain(std::iter::once(&HOSTILE_ORIGINAL))
        {
            scores.insert(t.to_string(), 0.9);
        }
        // German hostile text would get a near-zero score from an English model
        // anyway (#222) — but the language gate drops it before scoring, so the
        // value here is never read. Pinned high to PROVE it is never read: if
        // the gate ever stopped dropping them, F6 would flip to Proceed.
        for t in GERMAN_HOSTILE_REPLIES {
            scores.insert(t.to_string(), 0.9);
        }
        Self { scores }
    }
}

#[async_trait]
impl ToxicityScorer for ScriptedScorer {
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        Ok(ToxicityResult {
            toxicity: *self.scores.get(text).unwrap_or(&0.01),
            attributes: ToxicityAttributes::default(),
        })
    }
}

// ============================================================
// Outcome recording
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    /// Terminal "Low": the account is never looked at again this scan.
    ExitLow,
    /// Terminal "NotAssessed": abstained on language grounds.
    NotAssessed,
    /// Proceeds to Stage 2 (and, for survivors, the RunPod classifier).
    Proceed,
}

/// What the REAL gate does with this sample.
async fn current(sample: &PostSample, scorer: &dyn ToxicityScorer) -> Gate {
    let outcome = stage1_outcome(
        sample,
        scorer,
        "fixture.bsky.social",
        "did:plc:fixture",
        &unrelated_fingerprint(),
        &ThreatWeights::default(),
        None,
    )
    .await
    .expect("stage1_outcome should not error on a fixture");
    match outcome {
        Stage1Outcome::Proceed { .. } => Gate::Proceed,
        Stage1Outcome::Terminal(score) => match score.threat_tier.as_deref() {
            Some("Low") => Gate::ExitLow,
            Some("NotAssessed") => Gate::NotAssessed,
            other => panic!("unexpected terminal tier {other:?}"),
        },
    }
}

/// Solo ONNX scores of the replies Stage 1 actually scores — i.e. after the
/// #222 language partition, exactly as `stage1_outcome` sees them.
async fn assessable_reply_scores(sample: &PostSample, scorer: &dyn ToxicityScorer) -> Vec<f64> {
    let (assessable, _) = partition_assessable(sample);
    let texts: Vec<String> = assessable
        .replies
        .iter()
        .map(|r| r.post.text.clone())
        .collect();
    scorer
        .score_batch(&texts)
        .await
        .expect("score replies")
        .iter()
        .map(|r| r.toxicity)
        .collect()
}

/// HYPOTHETICAL option (a): an early exit is vetoed when any assessable reply
/// scores at or above the clean threshold. It can only ever turn ExitLow into
/// Proceed; it never touches a Proceed or an abstention.
fn option_a(current: Gate, reply_scores: &[f64]) -> Gate {
    if current == Gate::ExitLow && reply_scores.iter().any(|&s| s >= ONNX_CLEAN_THRESHOLD) {
        Gate::Proceed
    } else {
        current
    }
}

/// HYPOTHETICAL option (b) threshold. 5 mirrors
/// `MIN_FIRST_PERSON_POSTS_FOR_EARLY_EXIT`.
const OPTION_B_MIN_REPLIES: usize = 5;

/// HYPOTHETICAL option (b): an account with at least `OPTION_B_MIN_REPLIES`
/// assessable replies in its Stage-1 sample may not early-exit — its replies go
/// to Stage 2 to be judged with their parent post.
fn option_b(current: Gate, assessable_replies: usize) -> Gate {
    if current == Gate::ExitLow && assessable_replies >= OPTION_B_MIN_REPLIES {
        Gate::Proceed
    } else {
        current
    }
}

struct Row {
    name: &'static str,
    current: Gate,
    a: Gate,
    b: Gate,
    max_reply_score: Option<f64>,
}

async fn evaluate(scorer: &dyn ToxicityScorer) -> Vec<Row> {
    let mut rows = Vec::new();
    for (name, s) in fixtures() {
        let cur = current(&s, scorer).await;
        let reply_scores = assessable_reply_scores(&s, scorer).await;
        rows.push(Row {
            name,
            current: cur,
            a: option_a(cur, &reply_scores),
            b: option_b(cur, reply_scores.len()),
            max_reply_score: reply_scores.iter().cloned().reduce(f64::max),
        });
    }
    // Printed so `-- --show-output` reproduces the #358 comment table.
    eprintln!("fixture | current | (a) | (b) | (c) | max solo reply ONNX");
    for r in &rows {
        eprintln!(
            "{} | {:?} | {:?} | {:?} | {:?} | {}",
            r.name,
            r.current,
            r.a,
            r.b,
            r.current, // (c) keeps today's behaviour
            r.max_reply_score
                .map(|s| format!("{s:.3}"))
                .unwrap_or_else(|| "-".into())
        );
    }
    rows
}

fn row<'a>(rows: &'a [Row], prefix: &str) -> &'a Row {
    rows.iter()
        .find(|r| r.name.starts_with(prefix))
        .unwrap_or_else(|| panic!("no fixture {prefix}"))
}

/// The expected table, shared by the scripted and real-ONNX layers so the two
/// must agree. Columns: current, (a), (b).
const EXPECTED: [(&str, Gate, Gate, Gate); 7] = [
    ("F1", Gate::ExitLow, Gate::Proceed, Gate::Proceed),
    ("F2", Gate::ExitLow, Gate::ExitLow, Gate::Proceed),
    ("F3", Gate::ExitLow, Gate::ExitLow, Gate::Proceed),
    ("F4", Gate::ExitLow, Gate::ExitLow, Gate::ExitLow),
    ("F5", Gate::Proceed, Gate::Proceed, Gate::Proceed),
    ("F6", Gate::ExitLow, Gate::ExitLow, Gate::ExitLow),
    (
        "F7",
        Gate::NotAssessed,
        Gate::NotAssessed,
        Gate::NotAssessed,
    ),
];

fn assert_table(rows: &[Row]) {
    for (prefix, cur, a, b) in EXPECTED {
        let r = row(rows, prefix);
        assert_eq!(r.current, cur, "{}: current behaviour", r.name);
        assert_eq!(r.a, a, "{}: option (a)", r.name);
        assert_eq!(r.b, b, "{}: option (b)", r.name);
    }
}

// ============================================================
// Layer 1 — deterministic (always runs)
// ============================================================

/// THE BUG. Six replies that a solo ONNX pass scores as hostile (0.9 here) are
/// computed in Stage 1 and then thrown away by the gate: the account exits
/// Terminal "Low" with threat 0.0 and none of those replies reaches Stage 2.
#[tokio::test]
async fn current_false_negative_hostile_replies_exit_low() {
    let scorer = ScriptedScorer::new();
    let s = sample(&BENIGN_ORIGINALS, en_replies(&HOSTILE_REPLIES));

    // Precondition: the replies really are scored hostile by the same scorer
    // the gate uses — otherwise "exit Low" would be correct, not a bug.
    let reply_scores = assessable_reply_scores(&s, &scorer).await;
    assert_eq!(reply_scores.len(), HOSTILE_REPLIES.len());
    assert!(reply_scores.iter().all(|&x| x >= ONNX_CLEAN_THRESHOLD));

    let outcome = stage1_outcome(
        &s,
        &scorer,
        "replyharasser.bsky.social",
        "did:plc:replyharasser",
        &unrelated_fingerprint(),
        &ThreatWeights::default(),
        None,
    )
    .await
    .unwrap();
    match outcome {
        Stage1Outcome::Terminal(score) => {
            assert_eq!(score.threat_tier.as_deref(), Some("Low"));
            assert_eq!(score.threat_score, Some(0.0));
            assert_eq!(score.toxicity_score, Some(0.0));
            assert_eq!(score.scoring_confidence.as_deref(), Some("low"));
            assert!(score.top_toxic_posts.is_empty(), "no evidence recorded");
        }
        Stage1Outcome::Proceed { .. } => panic!(
            "#358 appears FIXED: hostile replies now block the Stage-1 early exit. \
             Rewrite this test to assert Proceed."
        ),
    }
}

/// Control for the test above: the SAME scores on an original (instead of a
/// reply) do block the exit. The asymmetry is the bug.
#[tokio::test]
async fn control_hostile_original_blocks_early_exit() {
    let mut originals = BENIGN_ORIGINALS.to_vec();
    originals[0] = HOSTILE_ORIGINAL;
    let s = sample(&originals, vec![]);
    assert_eq!(current(&s, &ScriptedScorer::new()).await, Gate::Proceed);
}

/// Second false-negative class, which NO solo-score fix can catch: replies
/// whose hostility is in the parent. Today they exit Low; option (a) would too.
#[tokio::test]
async fn current_false_negative_context_dependent_replies_exit_low() {
    let s = sample(&BENIGN_ORIGINALS, en_replies(&CONTEXT_DEPENDENT_REPLIES));
    assert_eq!(current(&s, &ScriptedScorer::new()).await, Gate::ExitLow);
}

/// Third false-negative class: an English-posting account whose hostile replies
/// are in another language. The #222 gate drops them before any scoring, the
/// 15 English originals keep the coverage gate at `Score`, and the account
/// exits Low. Neither (a) nor (b) changes that — the replies are never scored
/// and never counted.
#[tokio::test]
async fn current_false_negative_non_english_hostile_replies_exit_low() {
    let s = sample(
        &BENIGN_ORIGINALS,
        GERMAN_HOSTILE_REPLIES
            .iter()
            .enumerate()
            .map(|(i, t)| reply(i, t, &["de"]))
            .collect(),
    );
    assert_eq!(current(&s, &ScriptedScorer::new()).await, Gate::ExitLow);
}

/// The full fixture × option table, deterministic layer.
#[tokio::test]
async fn fixture_table_scripted_scorer() {
    let rows = evaluate(&ScriptedScorer::new()).await;
    assert_table(&rows);
}

// ============================================================
// Layer 2 — real Detoxify ONNX (model-gated)
// ============================================================

fn model_dir() -> Option<PathBuf> {
    let base = std::env::var("CHARCOAL_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| charcoal::toxicity::download::default_model_dir());
    charcoal::toxicity::download::model_files_present(&base).then_some(base)
}

/// Wraps the real model so every text is scored in its OWN forward pass.
///
/// Why: on the dev machine (macOS arm64) `OnnxToxicityScorer::score_batch`
/// only scores ROW 0 of a batch correctly — "I hope you die, you stupid bitch"
/// scores 0.999 alone and 0.003 at batch index 3. That is a separate, serious
/// bug (filed alongside #358; see the #358 comment). Stage 1 batches every
/// account's posts, so through the raw scorer the "current" column would be
/// measuring the batch bug, not the reply gate, and would differ by platform.
/// Scoring solo isolates the question #358 asks: what does the gate do with
/// CORRECT real-model scores?
struct SoloScorer(charcoal::toxicity::onnx::OnnxToxicityScorer);

#[async_trait]
impl ToxicityScorer for SoloScorer {
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        self.0.score_text(text).await
    }
    async fn score_batch(&self, texts: &[String]) -> Result<Vec<ToxicityResult>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.0.score_text(t).await?);
        }
        Ok(out)
    }
}

/// Same table through the real model (scored one text per forward pass, see
/// `SoloScorer`). This is what makes the scripted scores honest: the real
/// model must score the solo-hostile replies >= 0.10 (else option (a) would
/// catch nothing) and the benign / context-dependent replies < 0.10 (else they
/// would not be a clean-looking false negative).
#[tokio::test]
async fn real_onnx_fixture_table_matches_scripted() {
    let Some(dir) = model_dir() else {
        eprintln!("SKIP: real_onnx_fixture_table — toxicity model not present; THIS TEST ASSERTED NOTHING");
        return;
    };
    let scorer = SoloScorer(
        charcoal::toxicity::onnx::OnnxToxicityScorer::load(&dir)
            .expect("toxicity model should load when files are present"),
    );

    let rows = evaluate(&scorer).await;
    assert_table(&rows);

    // Positive control: every solo-hostile reply is individually above the
    // threshold, not just the max — so (a) fires on any one of them.
    let texts: Vec<String> = HOSTILE_REPLIES.iter().map(|s| s.to_string()).collect();
    for (t, r) in texts.iter().zip(scorer.score_batch(&texts).await.unwrap()) {
        eprintln!("hostile reply {:.3}  {t}", r.toxicity);
        assert!(
            r.toxicity >= ONNX_CLEAN_THRESHOLD,
            "{t} scored {}",
            r.toxicity
        );
    }
    // Negative controls: every benign and context-dependent reply, and every
    // benign original, is under the threshold.
    let texts: Vec<String> = BENIGN_REPLIES
        .iter()
        .chain(CONTEXT_DEPENDENT_REPLIES.iter())
        .chain(BENIGN_ORIGINALS.iter())
        .map(|s| s.to_string())
        .collect();
    for (t, r) in texts.iter().zip(scorer.score_batch(&texts).await.unwrap()) {
        eprintln!("clean-looking {:.3}  {t}", r.toxicity);
        assert!(
            r.toxicity < ONNX_CLEAN_THRESHOLD,
            "{t} scored {}",
            r.toxicity
        );
    }
}
