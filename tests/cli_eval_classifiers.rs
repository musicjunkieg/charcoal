//! End-to-end wiring test for `charcoal eval-classifiers` (#404): a full
//! round using fake classifiers only — no network, no model files.
use charcoal::cli::eval_classifiers::{
    cache,
    cost::{Budget, Price},
    data, labelling, report, runner,
};
use charcoal::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};
use std::collections::HashMap;

struct Says(bool);
#[async_trait::async_trait]
impl ToxicityClassifier for Says {
    async fn classify(&self, content: &str) -> anyhow::Result<ClassifierVerdict> {
        let toxic = self.0 && content.contains("bad");
        Ok(ClassifierVerdict {
            toxic_token: toxic,
            confidence: 0.9,
            latency_ms: 3,
            model_id: "f".into(),
            policy_version: "p".into(),
        })
    }
    fn name(&self) -> &'static str {
        "fake"
    }
    fn model_id(&self) -> &'static str {
        "f"
    }
    fn policy_version(&self) -> &'static str {
        "p"
    }
    fn threshold(&self) -> f32 {
        0.5
    }
}

#[tokio::test]
async fn a_full_round_runs_picks_merges_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let h = (0..200)
        .map(|i| format!("h{i}"))
        .find(|id| labelling::in_holdout(id))
        .unwrap();
    let d = (0..200)
        .map(|i| format!("d{i}"))
        .find(|id| !labelling::in_holdout(id) && !labelling::sampled(id))
        .unwrap();
    let pool_path = dir.path().join("pool.jsonl");
    std::fs::write(
        &pool_path,
        format!(
            "{{\"id\":\"{h}\",\"content\":\"bad\"}}\n{{\"id\":\"{d}\",\"content\":\"bad too\"}}\n"
        ),
    )
    .unwrap();
    let pool = data::load_pool(&pool_path).unwrap();
    let verdicts = dir.path().join("verdicts");
    let mut budget = Budget::new(1.0);
    for (name, m) in [("good", Says(true)), ("lazy", Says(false))] {
        runner::run_model(
            name,
            &m,
            &pool,
            &verdicts,
            &Price::default(),
            &mut budget,
            runner::DEFAULT_TIMEOUT,
        )
        .await
        .unwrap();
    }
    // The same library gate main.rs's label and report handlers call.
    let current = |m: &str| cache::current_for(&verdicts, m, &pool).unwrap();
    let per_model = vec![current("good"), current("lazy")];
    let labels_path = dir.path().join("labels.jsonl");
    let picked =
        labelling::pick_for_labelling(&pool, &per_model, &data::load_labels(&labels_path).unwrap());
    assert_eq!(picked.len(), 2, "the holdout post, and the disagreement");

    let to_label = dir.path().join("to-label.jsonl");
    labelling::write_to_label(
        &to_label,
        &picked,
        &data::load_labels(&labels_path).unwrap(),
    )
    .unwrap();
    let answered = std::fs::read_to_string(&to_label)
        .unwrap()
        .replace("\"label\":\"\"", "\"label\":\"toxic\"");
    std::fs::write(&to_label, answered).unwrap();
    assert_eq!(
        labelling::merge_labels(&to_label, &labels_path, &pool).unwrap(),
        2
    );

    let answers = data::load_labels(&labels_path).unwrap();
    let models = vec![
        ("good".to_string(), per_model[0].clone()),
        ("lazy".to_string(), per_model[1].clone()),
    ];
    let text = report::render(
        &report::build(&models, &answers, &HashMap::new()),
        &answers,
        "good",
    );
    assert!(text.contains("1 holdout (scored), 1 diagnostic"), "{text}");
    assert!(text.contains("lazy") && text.contains("(1/1)"), "{text}");
    assert!(
        text.contains("not enough holdout labels"),
        "one label is far below the 20-per-class gate: {text}"
    );
}

/// One judged post "a" (text "bad") under model "good", as a run leaves it.
async fn one_judged_post(verdicts: &std::path::Path) -> Vec<data::FixtureRow> {
    let pool: Vec<data::FixtureRow> =
        vec![serde_json::from_str(r#"{"id":"a","content":"bad"}"#).unwrap()];
    runner::run_model(
        "good",
        &Says(true),
        &pool,
        verdicts,
        &Price::default(),
        &mut Budget::new(1.0),
        runner::DEFAULT_TIMEOUT,
    )
    .await
    .unwrap();
    assert_eq!(
        cache::current_for(verdicts, "good", &pool).unwrap()["a"].called_toxic,
        Some(true),
        "the fresh verdict is current"
    );
    pool
}

/// Final review fix 6: a verdict for text that has since changed is stale.
#[tokio::test]
async fn a_verdict_for_changed_post_text_is_not_current() {
    let dir = tempfile::tempdir().unwrap();
    let verdicts = dir.path().join("verdicts");
    one_judged_post(&verdicts).await;
    let edited: Vec<data::FixtureRow> =
        vec![serde_json::from_str(r#"{"id":"a","content":"bad, edited"}"#).unwrap()];
    assert!(cache::current_for(&verdicts, "good", &edited)
        .unwrap()
        .is_empty());
}

/// Final review fix 6: after a redeploy (a different verified identity),
/// verdicts from the old identity are not current.
#[tokio::test]
async fn a_verdict_from_a_changed_identity_is_not_current() {
    let dir = tempfile::tempdir().unwrap();
    let verdicts = dir.path().join("verdicts");
    let pool = one_judged_post(&verdicts).await;
    cache::save_identity(
        &verdicts,
        "good",
        cache::Identity {
            model_id: "f",
            policy_version: "p-new",
        },
    )
    .unwrap();
    assert!(cache::current_for(&verdicts, "good", &pool)
        .unwrap()
        .is_empty());
}
