//! #427 — an ONNX batch's working memory must be handed back after the batch.
//!
//! ONNX Runtime's CPU memory arena grows to the largest batch it has ever run
//! and never shrinks. A scan scores each account's posts in one batch, so the
//! web server kept its biggest scan's peak for as long as it ran: staging went
//! 0.72 -> 1.74 GB in one scan and stayed there, prod sat at 3.8 GB for days.
//! Railway bills the memory a service holds, so that was most of the bill.
//!
//! Measured on Linux glibc 2.39 with this model before the fix: RSS after the
//! work stayed at the peak (~1.4 GB) under glibc AND under mimalloc — the
//! allocator was never the problem, the arena was. The fix shrinks the arena
//! after every run (src/onnx_session.rs).
//!
//! Linux only: it reads /proc, and it asserts glibc behaviour, which is what
//! production runs. macOS malloc keeps freed pages resident on its own, so the
//! same check there would measure the platform, not Charcoal.
//!
//! Model-gated: prints the `SKIP:` sentinel and asserts nothing when the
//! models are absent — run with `CHARCOAL_MODEL_DIR=./models`.
#![cfg(target_os = "linux")]

use charcoal::topics::embeddings::SentenceEmbedder;
use charcoal::toxicity::onnx::OnnxToxicityScorer;
use charcoal::toxicity::traits::ToxicityScorer;
use serial_test::serial;
use std::path::PathBuf;

fn model_dir() -> Option<PathBuf> {
    let base = std::env::var("CHARCOAL_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| charcoal::toxicity::download::default_model_dir());
    charcoal::toxicity::download::model_files_present(&base).then_some(base)
}

/// (current RSS, peak RSS since the last reset), in MB, from /proc.
fn rss_and_peak_mb() -> (f64, f64) {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    let field = |name: &str| -> f64 {
        let line = status
            .lines()
            .find(|l| l.starts_with(name))
            .unwrap_or_else(|| panic!("{name} missing from /proc/self/status"));
        let kb: f64 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        kb / 1024.0
    };
    (field("VmRSS:"), field("VmHWM:"))
}

/// Writing 5 to clear_refs resets the peak (VmHWM) to the current RSS, so the
/// peak read afterwards belongs to the batch under test alone.
fn reset_peak() {
    std::fs::write("/proc/self/clear_refs", "5").expect("reset VmHWM");
}

/// Long, varied posts: big enough that the batch's tensors dwarf noise, so the
/// arena's retention (or its absence) is unmistakable.
fn big_batch() -> Vec<String> {
    (0..150)
        .map(|i| {
            (0..90)
                .map(|w| format!("word{}", (i * 31 + w * 7) % 997))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// The requirement, stated once for both models: after the batch, the process
/// must give back at least half of what the batch added at its peak. Without arena
/// shrinkage it gives back ~none (retained ≈ peak). The peak must also
/// have grown by a real amount, or "gave back half of nothing" proves nothing.
fn assert_released(label: &str, before: f64, after: f64, peak: f64) {
    let grew = peak - before;
    let kept = after - before;
    eprintln!("{label}: before={before:.0} peak={peak:.0} after={after:.0} MB");
    assert!(
        grew > 150.0,
        "{label}: the batch only grew RSS by {grew:.0} MB — too small to tell retention from noise"
    );
    assert!(
        kept < grew * 0.5,
        "{label}: kept {kept:.0} of the {grew:.0} MB the batch added — ONNX working memory is not \
         being returned (are inference runs still shrinking the CPU arena? see src/onnx_session.rs)"
    );
}

#[tokio::test]
#[serial]
async fn toxicity_batch_memory_is_returned() {
    let Some(dir) = model_dir() else {
        eprintln!("SKIP: models not present");
        return;
    };
    let scorer = OnnxToxicityScorer::load(&dir).expect("load toxicity model");
    let texts = big_batch();
    // One warm-up run: lazily-initialised runtime state is a one-off, not
    // per-batch retention.
    scorer.score_batch(&texts[..4]).await.unwrap();

    let (before, _) = rss_and_peak_mb();
    reset_peak();
    scorer.score_batch(&texts).await.unwrap();
    let (after, peak) = rss_and_peak_mb();
    assert_released("toxicity", before, after, peak);
}

#[tokio::test]
#[serial]
async fn embedding_batch_memory_is_returned() {
    let Some(dir) = model_dir() else {
        eprintln!("SKIP: models not present");
        return;
    };
    let embedder = SentenceEmbedder::load(&charcoal::toxicity::download::embedding_model_dir(&dir))
        .expect("load embedding model");
    // MiniLM is small: a bigger batch is needed for its activations to clear
    // the noise floor.
    let texts: Vec<String> = big_batch().into_iter().cycle().take(600).collect();
    embedder.embed_batch(&texts[..4]).await.unwrap();

    let (before, _) = rss_and_peak_mb();
    reset_peak();
    embedder.embed_batch(&texts).await.unwrap();
    let (after, peak) = rss_and_peak_mb();
    assert_released("embeddings", before, after, peak);
}
