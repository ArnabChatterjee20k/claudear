//! Agent orchestrator for provider selection, experiments, and fallback chains.
//!
//! Implements `AgentRunner` itself (composite pattern), so consumers hold
//! `Arc<dyn AgentRunner>` and are completely unaware of orchestration logic.

use super::{AgentRunner, ProviderCapabilities};
use crate::error::{Error, Result};
use crate::types::{AgentResult, Issue};
use async_trait::async_trait;
use rand::Rng;
use std::path::Path;
use std::sync::Arc;

/// A provider with an associated weight for experiment selection.
pub struct WeightedProvider {
    pub provider: Arc<dyn AgentRunner>,
    pub weight: f64,
}

/// Strategy for selecting which provider to use.
#[derive(Debug, Clone)]
pub enum SelectionStrategy {
    /// Always use the first provider.
    Primary,
    /// A/B split by weight.
    WeightedRandom,
    /// Try providers in order until one succeeds.
    Fallback,
}

/// Orchestrator that manages multiple providers with experiment support.
pub struct AgentOrchestrator {
    providers: Vec<WeightedProvider>,
    strategy: SelectionStrategy,
    experiment_name: Option<String>,
}

impl AgentOrchestrator {
    /// Create a new orchestrator with the given providers and strategy.
    pub fn new(
        providers: Vec<WeightedProvider>,
        strategy: SelectionStrategy,
        experiment_name: Option<String>,
    ) -> Self {
        Self {
            providers,
            strategy,
            experiment_name,
        }
    }

    /// Create an orchestrator from experiment config.
    pub fn from_experiment(
        providers: Vec<WeightedProvider>,
        experiment_name: &str,
        strategy_str: &str,
    ) -> Self {
        let strategy = match strategy_str {
            "weighted_random" => SelectionStrategy::WeightedRandom,
            "fallback" => SelectionStrategy::Fallback,
            _ => SelectionStrategy::Primary,
        };
        Self {
            providers,
            strategy,
            experiment_name: Some(experiment_name.to_string()),
        }
    }

    /// Select a provider index using weighted random selection.
    fn select_weighted_random(&self) -> usize {
        let total_weight: f64 = self.providers.iter().map(|p| p.weight).sum();
        if total_weight <= 0.0 || self.providers.is_empty() {
            return 0;
        }

        let mut rng = rand::rng();
        let roll: f64 = rng.random::<f64>() * total_weight;
        let mut cumulative = 0.0;

        for (i, provider) in self.providers.iter().enumerate() {
            cumulative += provider.weight;
            if roll < cumulative {
                return i;
            }
        }

        self.providers.len() - 1
    }
}

#[async_trait]
impl AgentRunner for AgentOrchestrator {
    fn name(&self) -> &str {
        "orchestrator"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        // Return capabilities of the primary (first) provider.
        self.providers
            .first()
            .map(|p| p.provider.capabilities())
            .unwrap_or_default()
    }

    fn build_prompt_for_issue(
        &self,
        issue: &Issue,
        context: &str,
        project_dir: &Path,
    ) -> String {
        // Use the primary provider's prompt building.
        self.providers
            .first()
            .map(|p| p.provider.build_prompt_for_issue(issue, context, project_dir))
            .unwrap_or_default()
    }

    async fn execute_with_attempt(
        &self,
        prompt: &str,
        issue: Option<&Issue>,
        attempt_id: Option<i64>,
        project_dir: &Path,
    ) -> Result<AgentResult> {
        if self.providers.is_empty() {
            return Err(Error::runner("No providers configured in orchestrator"));
        }

        match self.strategy {
            SelectionStrategy::Primary => {
                let provider = &self.providers[0].provider;
                tracing::info!(
                    component = "orchestrator",
                    provider = provider.name(),
                    "Using primary provider"
                );
                provider
                    .execute_with_attempt(prompt, issue, attempt_id, project_dir)
                    .await
            }

            SelectionStrategy::WeightedRandom => {
                let idx = self.select_weighted_random();
                let provider = &self.providers[idx].provider;
                tracing::info!(
                    component = "orchestrator",
                    provider = provider.name(),
                    experiment = self.experiment_name.as_deref().unwrap_or("none"),
                    "Selected provider via weighted random"
                );
                provider
                    .execute_with_attempt(prompt, issue, attempt_id, project_dir)
                    .await
            }

            SelectionStrategy::Fallback => {
                let mut last_error = None;
                for (i, wp) in self.providers.iter().enumerate() {
                    let provider = &wp.provider;
                    tracing::info!(
                        component = "orchestrator",
                        provider = provider.name(),
                        attempt = i + 1,
                        total = self.providers.len(),
                        "Trying provider in fallback chain"
                    );

                    match provider
                        .execute_with_attempt(prompt, issue, attempt_id, project_dir)
                        .await
                    {
                        Ok(result) => {
                            // Only fall back on Err, NOT on AgentResult { success: false }.
                            // The agent tried but couldn't fix — that's a valid outcome.
                            return Ok(result);
                        }
                        Err(e) => {
                            tracing::warn!(
                                component = "orchestrator",
                                provider = provider.name(),
                                error = %e,
                                "Provider failed, trying next in fallback chain"
                            );
                            last_error = Some(e);
                        }
                    }
                }

                Err(last_error.unwrap_or_else(|| Error::runner("All providers failed")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock provider for testing.
    struct MockProvider {
        name: String,
        should_fail: bool,
    }

    #[async_trait]
    impl AgentRunner for MockProvider {
        fn name(&self) -> &str {
            &self.name
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }

        fn build_prompt_for_issue(
            &self,
            _issue: &Issue,
            _context: &str,
            _project_dir: &Path,
        ) -> String {
            format!("prompt from {}", self.name)
        }

        async fn execute_with_attempt(
            &self,
            _prompt: &str,
            _issue: Option<&Issue>,
            _attempt_id: Option<i64>,
            _project_dir: &Path,
        ) -> Result<AgentResult> {
            if self.should_fail {
                Err(Error::runner(format!("{} failed", self.name)))
            } else {
                Ok(AgentResult {
                    success: true,
                    output: format!("result from {}", self.name),
                    pr_url: None,
                    changelog: None,
                    error: None,
                    blocking_question: None,
                    used_qa_ids: Vec::new(),
                })
            }
        }
    }

    #[test]
    fn test_primary_strategy_uses_first_provider() {
        let orchestrator = AgentOrchestrator::new(
            vec![
                WeightedProvider {
                    provider: Arc::new(MockProvider {
                        name: "alpha".into(),
                        should_fail: false,
                    }),
                    weight: 1.0,
                },
                WeightedProvider {
                    provider: Arc::new(MockProvider {
                        name: "beta".into(),
                        should_fail: false,
                    }),
                    weight: 1.0,
                },
            ],
            SelectionStrategy::Primary,
            None,
        );

        let prompt = orchestrator.build_prompt_for_issue(
            &Issue::new("1", "T-1", "Bug", "url", "test"),
            "ctx",
            Path::new("/tmp"),
        );
        assert_eq!(prompt, "prompt from alpha");
    }

    #[tokio::test]
    async fn test_fallback_skips_failing_provider() {
        let orchestrator = AgentOrchestrator::new(
            vec![
                WeightedProvider {
                    provider: Arc::new(MockProvider {
                        name: "failing".into(),
                        should_fail: true,
                    }),
                    weight: 1.0,
                },
                WeightedProvider {
                    provider: Arc::new(MockProvider {
                        name: "working".into(),
                        should_fail: false,
                    }),
                    weight: 1.0,
                },
            ],
            SelectionStrategy::Fallback,
            None,
        );

        let result = orchestrator
            .execute_with_attempt("test", None, None, Path::new("/tmp"))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "result from working");
    }

    #[tokio::test]
    async fn test_fallback_all_fail_returns_error() {
        let orchestrator = AgentOrchestrator::new(
            vec![
                WeightedProvider {
                    provider: Arc::new(MockProvider {
                        name: "fail1".into(),
                        should_fail: true,
                    }),
                    weight: 1.0,
                },
                WeightedProvider {
                    provider: Arc::new(MockProvider {
                        name: "fail2".into(),
                        should_fail: true,
                    }),
                    weight: 1.0,
                },
            ],
            SelectionStrategy::Fallback,
            None,
        );

        let result = orchestrator
            .execute_with_attempt("test", None, None, Path::new("/tmp"))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fallback_returns_unsuccessful_result_without_fallback() {
        // A provider that returns success=false should NOT trigger fallback.
        struct UnsuccessfulProvider;

        #[async_trait]
        impl AgentRunner for UnsuccessfulProvider {
            fn name(&self) -> &str {
                "unsuccessful"
            }
            fn capabilities(&self) -> ProviderCapabilities {
                ProviderCapabilities::default()
            }
            fn build_prompt_for_issue(
                &self,
                _issue: &Issue,
                _context: &str,
                _project_dir: &Path,
            ) -> String {
                String::new()
            }
            async fn execute_with_attempt(
                &self,
                _prompt: &str,
                _issue: Option<&Issue>,
                _attempt_id: Option<i64>,
                _project_dir: &Path,
            ) -> Result<AgentResult> {
                Ok(AgentResult {
                    success: false,
                    output: "could not fix".to_string(),
                    pr_url: None,
                    changelog: None,
                    error: Some("failed to fix".to_string()),
                    blocking_question: None,
                    used_qa_ids: Vec::new(),
                })
            }
        }

        let orchestrator = AgentOrchestrator::new(
            vec![
                WeightedProvider {
                    provider: Arc::new(UnsuccessfulProvider),
                    weight: 1.0,
                },
                WeightedProvider {
                    provider: Arc::new(MockProvider {
                        name: "backup".into(),
                        should_fail: false,
                    }),
                    weight: 1.0,
                },
            ],
            SelectionStrategy::Fallback,
            None,
        );

        let result = orchestrator
            .execute_with_attempt("test", None, None, Path::new("/tmp"))
            .await
            .unwrap();
        // Should return the unsuccessful result, NOT fall back to backup.
        assert!(!result.success);
        assert_eq!(result.output, "could not fix");
    }

    #[tokio::test]
    async fn test_empty_orchestrator_returns_error() {
        let orchestrator =
            AgentOrchestrator::new(vec![], SelectionStrategy::Primary, None);

        let result = orchestrator
            .execute_with_attempt("test", None, None, Path::new("/tmp"))
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn test_weighted_selection_single_provider() {
        let orchestrator = AgentOrchestrator::new(
            vec![WeightedProvider {
                provider: Arc::new(MockProvider {
                    name: "only".into(),
                    should_fail: false,
                }),
                weight: 1.0,
            }],
            SelectionStrategy::WeightedRandom,
            None,
        );

        // With a single provider, it should always be selected.
        for _ in 0..10 {
            assert_eq!(orchestrator.select_weighted_random(), 0);
        }
    }

    #[test]
    fn test_weighted_selection_respects_weights() {
        let orchestrator = AgentOrchestrator::new(
            vec![
                WeightedProvider {
                    provider: Arc::new(MockProvider {
                        name: "heavy".into(),
                        should_fail: false,
                    }),
                    weight: 100.0,
                },
                WeightedProvider {
                    provider: Arc::new(MockProvider {
                        name: "light".into(),
                        should_fail: false,
                    }),
                    weight: 0.001,
                },
            ],
            SelectionStrategy::WeightedRandom,
            None,
        );

        // With 100 vs 0.001 weights, the heavy provider should be selected
        // almost exclusively. Run 100 trials.
        let mut heavy_count = 0;
        for _ in 0..100 {
            if orchestrator.select_weighted_random() == 0 {
                heavy_count += 1;
            }
        }
        assert!(
            heavy_count >= 95,
            "Heavy provider selected only {} times out of 100",
            heavy_count
        );
    }

    #[test]
    fn test_from_experiment() {
        let orchestrator = AgentOrchestrator::from_experiment(
            vec![WeightedProvider {
                provider: Arc::new(MockProvider {
                    name: "test".into(),
                    should_fail: false,
                }),
                weight: 1.0,
            }],
            "my-experiment",
            "fallback",
        );
        assert_eq!(orchestrator.experiment_name.as_deref(), Some("my-experiment"));
        assert!(matches!(orchestrator.strategy, SelectionStrategy::Fallback));
    }
}
