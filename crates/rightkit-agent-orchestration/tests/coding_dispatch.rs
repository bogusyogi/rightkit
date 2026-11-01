#![cfg(feature = "coding")]
use async_trait::async_trait;
use rightkit_agent_orchestration::coding::{
    ClassifierComplexity, ClassifierOutcome, DispatchClassifier, DispatchRouter, ExecutionStrategy,
    JudgeSource,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct CountingClassifier {
    calls: Arc<AtomicUsize>,
    outcome: ClassifierOutcome,
}

impl CountingClassifier {
    fn new(outcome: ClassifierOutcome) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            outcome,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl DispatchClassifier for CountingClassifier {
    async fn classify(&self, _task: &str) -> Result<ClassifierOutcome, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.outcome.clone())
    }
}

fn is_direct(strategy: &ExecutionStrategy) -> bool {
    matches!(strategy, ExecutionStrategy::Direct)
}

fn is_conduct(strategy: &ExecutionStrategy) -> bool {
    matches!(strategy, ExecutionStrategy::Conduct { .. })
}

#[tokio::test]
async fn confident_heuristics_classify_without_classifier() {
    let classifier = CountingClassifier::new(ClassifierOutcome {
        complexity: ClassifierComplexity::Complex,
        reason: "stub should not be called".into(),
    });
    let router = DispatchRouter::new();

    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, fn(&ExecutionStrategy) -> bool)> = vec![
        ("what does this function do?", is_direct),
        ("fix the typo in README.md", is_direct),
        (
            "change the button label in apps/rightkit-agent-tauri/src/App.tsx",
            is_direct,
        ),
        (
            "refactor the auth module across the codebase and add tests",
            is_conduct,
        ),
        (
            "audit and fix the engine, frontend, and protocol changes end-to-end",
            is_conduct,
        ),
        (
            "migrate every command handler to the new schema and update all call sites",
            is_conduct,
        ),
    ];

    for (task, expected) in cases {
        let decision = router.decide(task, Some(&classifier)).await;
        assert!(
            expected(&decision.strategy),
            "unexpected strategy for `{task}`: {:?}",
            decision
        );
        assert_eq!(decision.judged_by, JudgeSource::Heuristic);
        assert!(
            decision.confidence >= 0.75,
            "confident cases must stay above the classifier threshold: {:?}",
            decision
        );
    }

    assert_eq!(classifier.calls(), 0, "heuristics must be cost-free");
}

#[tokio::test]
async fn borderline_tasks_use_classifier_once() {
    let classifier = CountingClassifier::new(ClassifierOutcome {
        complexity: ClassifierComplexity::Complex,
        reason: "multiple coordinated changes likely need decomposition".into(),
    });
    let router = DispatchRouter::new();

    let decision = router
        .decide(
            "Improve onboarding flow and add safeguards",
            Some(&classifier),
        )
        .await;

    assert!(matches!(
        decision.strategy,
        ExecutionStrategy::Conduct { ref reason }
            if reason.contains("multiple coordinated changes")
    ));
    assert_eq!(decision.judged_by, JudgeSource::Classifier);
    assert_eq!(classifier.calls(), 1);
}

#[tokio::test]
async fn classifier_can_route_borderline_task_direct() {
    let classifier = CountingClassifier::new(ClassifierOutcome {
        complexity: ClassifierComplexity::Simple,
        reason: "small local edit".into(),
    });
    let router = DispatchRouter::new();

    let decision = router
        .decide(
            "Improve onboarding flow and add safeguards",
            Some(&classifier),
        )
        .await;

    assert!(matches!(decision.strategy, ExecutionStrategy::Direct));
    assert_eq!(decision.rationale, "small local edit");
    assert_eq!(decision.judged_by, JudgeSource::Classifier);
    assert_eq!(classifier.calls(), 1);
}
