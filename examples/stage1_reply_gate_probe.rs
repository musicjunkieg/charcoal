//! Probe: what would each #358 option cost? (read-only, no paid services)
//!
//! For a list of real account DIDs it fetches the same 25-post Stage-1 sample
//! production fetches (public Bluesky API, free), runs the REAL `stage1_outcome`
//! with the local Detoxify ONNX model, and counts how many accounts:
//!
//! - exit Low today (current gate),
//! - would instead proceed under option (a) — any assessable reply with a solo
//!   ONNX score >= 0.10 vetoes the exit,
//! - would instead proceed under option (b) — >= 5 assessable replies veto it.
//!
//! For every account that proceeds (today, or newly under a/b) it then mimics
//! Phase A of Stage 2: fetches the 50-post sample plus reply parents, builds the
//! same parent+reply envelopes gather.rs builds, ONNX-scores them, and counts
//! the SURVIVORS (>= 0.10). Survivors are exactly the rows Phase B sends to the
//! RunPod classifier, so survivor counts are the GPU cost proxy.
//!
//! All ONNX scoring here is one text per forward pass, because
//! `OnnxToxicityScorer::score_batch` mis-scores every row but the first on
//! macOS arm64 (#400). The "current (batched)" line shows what the gate does
//! with the batch path as production calls it, for comparison.
//!
//! It never calls RunPod, never writes a database, and prints only aggregate
//! counts (no handles or DIDs).
//!
//! Run:
//!   cargo run --release --example stage1_reply_gate_probe -- <dids.txt> [protected_handle]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream::{self, StreamExt};

use charcoal::bluesky::client::{PublicAtpClient, DEFAULT_PUBLIC_API_URL};
use charcoal::bluesky::posts::{self, PostSample};
use charcoal::scoring::language::partition_assessable;
use charcoal::scoring::profile::{stage1_outcome, Stage1Outcome};
use charcoal::scoring::threat::ThreatWeights;
use charcoal::topics::build::{assemble_fingerprint, EmbeddedPosts};
use charcoal::topics::embeddings::SentenceEmbedder;
use charcoal::topics::fingerprint::TopicFingerprint;
use charcoal::topics::tfidf::clean_for_embedding;
use charcoal::toxicity::download::embedding_model_dir;
use charcoal::toxicity::ensemble::ONNX_CLEAN_THRESHOLD;
use charcoal::toxicity::onnx::OnnxToxicityScorer;
use charcoal::toxicity::traits::{ToxicityResult, ToxicityScorer};

const OPTION_B_MIN_REPLIES: usize = 5;
const CONCURRENCY: usize = 4;

/// One forward pass per text (see module docs / #400).
struct Solo(Arc<OnnxToxicityScorer>);

#[async_trait]
impl ToxicityScorer for Solo {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    ExitLow,
    OtherTerminal,
    Proceed,
}

fn gate_of(o: &Stage1Outcome) -> Gate {
    match o {
        Stage1Outcome::Proceed { .. } => Gate::Proceed,
        Stage1Outcome::Terminal(s) if s.threat_tier.as_deref() == Some("Low") => Gate::ExitLow,
        Stage1Outcome::Terminal(_) => Gate::OtherTerminal,
    }
}

#[derive(Default, Debug)]
struct Row {
    current: Option<Gate>,
    current_batched: Option<Gate>,
    a_veto: bool,
    b_veto: bool,
    assessable_replies: usize,
    hostile_replies: usize,
    /// Stage-2 survivors (RunPod rows) if this account proceeds. None = not measured.
    survivors: Option<usize>,
    stage2_posts: Option<usize>,
}

async fn stage2_survivors(
    client: &PublicAtpClient,
    scorer: &Solo,
    did: &str,
) -> Result<(usize, usize)> {
    let sample = posts::fetch_posts_with_replies(client, did, 50).await?;
    let (sample, _) = partition_assessable(&sample);
    let parent_uris: Vec<String> = sample
        .replies
        .iter()
        .map(|r| r.parent_uri.clone())
        .collect();
    let parents = posts::fetch_parent_posts(client, &parent_uris).await?;
    let mut texts: Vec<String> = sample.originals.iter().map(|p| p.text.clone()).collect();
    for r in &sample.replies {
        texts.push(match parents.get(&r.parent_uri) {
            Some(p) => charcoal::toxicity::format_parent_reply(p, &r.post.text),
            None => r.post.text.clone(),
        });
    }
    texts.extend(sample.quotes.iter().map(|p| p.text.clone()));
    let scores = scorer.score_batch(&texts).await?;
    let survivors = scores
        .iter()
        .filter(|r| r.toxicity >= ONNX_CLEAN_THRESHOLD)
        .count();
    Ok((survivors, texts.len()))
}

async fn probe_account(
    client: &PublicAtpClient,
    solo: &Solo,
    batched: &OnnxToxicityScorer,
    fp: &TopicFingerprint,
    did: &str,
) -> Result<Row> {
    let weights = ThreatWeights::default();
    let sample: PostSample = posts::fetch_posts_with_replies(client, did, 25).await?;
    let cur = gate_of(&stage1_outcome(&sample, solo, did, did, fp, &weights, None).await?);
    let cur_b = gate_of(&stage1_outcome(&sample, batched, did, did, fp, &weights, None).await?);

    let (assessable, _) = partition_assessable(&sample);
    let reply_texts: Vec<String> = assessable
        .replies
        .iter()
        .map(|r| r.post.text.clone())
        .collect();
    let reply_scores = solo.score_batch(&reply_texts).await?;
    let hostile = reply_scores
        .iter()
        .filter(|r| r.toxicity >= ONNX_CLEAN_THRESHOLD)
        .count();

    let mut row = Row {
        current: Some(cur),
        current_batched: Some(cur_b),
        a_veto: cur == Gate::ExitLow && hostile > 0,
        b_veto: cur == Gate::ExitLow && reply_texts.len() >= OPTION_B_MIN_REPLIES,
        assessable_replies: reply_texts.len(),
        hostile_replies: hostile,
        ..Default::default()
    };
    if cur == Gate::Proceed || row.a_veto || row.b_veto {
        if let Ok((s, n)) = stage2_survivors(client, solo, did).await {
            row.survivors = Some(s);
            row.stage2_posts = Some(n);
        }
    }
    Ok(row)
}

async fn protected_fingerprint(
    client: &PublicAtpClient,
    model_dir: &std::path::Path,
    handle: &str,
) -> Result<TopicFingerprint> {
    // Same path as topics::build::build_user_fingerprint, minus persistence.
    let fp_posts = posts::fetch_recent_posts(client, handle, 500).await?;
    let post_texts: Vec<String> = fp_posts.iter().map(|p| p.text.clone()).collect();
    let embedder = SentenceEmbedder::load(&embedding_model_dir(model_dir))?;
    let aligned: Vec<(String, String)> = post_texts
        .iter()
        .map(|t| (t.clone(), clean_for_embedding(t)))
        .filter(|(_, c)| !c.is_empty())
        .collect();
    let cleaned: Vec<String> = aligned.iter().map(|(_, c)| c.clone()).collect();
    let embeddings = embedder.embed_batch(&cleaned).await?;
    let embedded = EmbeddedPosts {
        original_texts: aligned.into_iter().map(|(o, _)| o).collect(),
        embeddings,
    };
    Ok(assemble_fingerprint(&post_texts, Some(&embedded))?.fingerprint)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dids_path = args
        .get(1)
        .context("usage: <dids.txt> [protected_handle]")?;
    let handle = args.get(2).map(String::as_str).unwrap_or("chaosgreml.in");
    let model_dir = std::env::var("CHARCOAL_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./models"));

    let dids: Vec<String> = std::fs::read_to_string(dids_path)?
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("did:"))
        .map(String::from)
        .collect();

    let client = PublicAtpClient::new(DEFAULT_PUBLIC_API_URL)?;
    let onnx = Arc::new(OnnxToxicityScorer::load(&model_dir)?);
    let solo = Solo(Arc::clone(&onnx));
    let fp = protected_fingerprint(&client, &model_dir, handle).await?;
    eprintln!("protected fingerprint: {} clusters", fp.clusters.len());

    let results: Vec<Result<Row>> = stream::iter(dids.iter())
        .map(|did| probe_account(&client, &solo, &onnx, &fp, did))
        .buffer_unordered(CONCURRENCY)
        .collect()
        .await;

    let rows: Vec<&Row> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    let errors = results.len() - rows.len();
    let n = |f: &dyn Fn(&Row) -> bool| rows.iter().filter(|r| f(r)).count();
    let survivors = |f: &dyn Fn(&Row) -> bool| -> (usize, usize, usize) {
        let sel: Vec<&&Row> = rows.iter().filter(|r| f(r)).collect();
        let measured: Vec<&&&Row> = sel.iter().filter(|r| r.survivors.is_some()).collect();
        (
            measured.len(),
            measured.iter().map(|r| r.survivors.unwrap_or(0)).sum(),
            measured
                .iter()
                .filter(|r| r.survivors.unwrap_or(0) > 0)
                .count(),
        )
    };

    println!("accounts requested: {}", dids.len());
    println!(
        "fetched + scored:   {}   (errors / gone: {errors})",
        rows.len()
    );
    println!();
    println!("CURRENT gate (solo-correct scores)");
    println!(
        "  exit Low:        {}",
        n(&|r| r.current == Some(Gate::ExitLow))
    );
    println!(
        "  proceed:         {}",
        n(&|r| r.current == Some(Gate::Proceed))
    );
    println!(
        "  other terminal:  {}",
        n(&|r| r.current == Some(Gate::OtherTerminal))
    );
    println!("CURRENT gate (batched scorer, as production calls it)");
    println!(
        "  exit Low:        {}",
        n(&|r| r.current_batched == Some(Gate::ExitLow))
    );
    println!(
        "  proceed:         {}",
        n(&|r| r.current_batched == Some(Gate::Proceed))
    );
    println!();
    println!("Among today's Low exits:");
    println!(
        "  with >=1 assessable reply:             {}",
        n(&|r| r.current == Some(Gate::ExitLow) && r.assessable_replies > 0)
    );
    println!(
        "  with >=1 reply solo ONNX >= 0.10 (a):  {}",
        n(&|r| r.a_veto)
    );
    println!(
        "  with >=5 assessable replies (b):       {}",
        n(&|r| r.b_veto)
    );
    println!(
        "  hostile-scoring replies in (a) set:    {}",
        rows.iter()
            .filter(|r| r.a_veto)
            .map(|r| r.hostile_replies)
            .sum::<usize>()
    );
    println!();
    let (m, s, nz) = survivors(&|r| r.current == Some(Gate::Proceed));
    println!("Stage-2 survivors (RunPod rows), measured on 50-post samples:");
    println!("  baseline (proceed today): {m} accounts, {s} rows, {nz} accounts with >=1 row");
    let (m, s, nz) = survivors(&|r| r.a_veto);
    println!("  added by (a):             {m} accounts, {s} rows, {nz} accounts with >=1 row");
    let (m, s, nz) = survivors(&|r| r.b_veto);
    println!("  added by (b):             {m} accounts, {s} rows, {nz} accounts with >=1 row");
    let (m, s, nz) = survivors(&|r| r.b_veto && !r.a_veto);
    println!("  added by (b) but not (a): {m} accounts, {s} rows, {nz} accounts with >=1 row");
    Ok(())
}
