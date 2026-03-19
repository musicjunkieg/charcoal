//! Unit tests for NLI model integration and hostility scoring.

use std::path::PathBuf;

// --- NLI model file detection tests ---

#[test]
fn nli_files_present_returns_false_for_empty_dir() {
    let dir = std::env::temp_dir().join("charcoal-nli-test-nonexistent");
    assert!(!charcoal::toxicity::download::nli_files_present(&dir));
}

#[test]
fn nli_files_present_returns_true_when_both_files_exist() {
    let dir = std::env::temp_dir().join("charcoal-nli-test-present");
    let nli_dir = charcoal::toxicity::download::nli_model_dir(&dir);
    std::fs::create_dir_all(&nli_dir).unwrap();
    std::fs::write(nli_dir.join("model_quantized.onnx"), b"fake model").unwrap();
    std::fs::write(nli_dir.join("tokenizer.json"), b"fake tokenizer").unwrap();
    assert!(charcoal::toxicity::download::nli_files_present(&dir));

    // Cleanup
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn nli_files_present_returns_false_when_model_missing() {
    let dir = std::env::temp_dir().join("charcoal-nli-test-partial");
    let nli_dir = charcoal::toxicity::download::nli_model_dir(&dir);
    std::fs::create_dir_all(&nli_dir).unwrap();
    std::fs::write(nli_dir.join("tokenizer.json"), b"fake tokenizer").unwrap();
    // model_quantized.onnx missing
    assert!(!charcoal::toxicity::download::nli_files_present(&dir));

    // Cleanup
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn nli_model_dir_is_subdirectory() {
    let base = PathBuf::from("/tmp/test-models");
    let nli_dir = charcoal::toxicity::download::nli_model_dir(&base);
    assert_eq!(nli_dir, base.join("nli-deberta-v3-xsmall"));
}
