//! Continuous learning subsystems for Claudear.
//!
//! Each module is an independent subsystem that can be toggled via `LearningConfig`.

pub mod cluster_detector;
pub mod diff_analysis;
pub mod log_extractor;
pub mod qa_promoter;
pub mod quality_scorer;
pub mod repo_knowledge;
pub mod review_classifier;
pub mod strategy_parser;

pub use cluster_detector::ClusterDetector;
pub use diff_analysis::DiffAnalyzer;
pub use log_extractor::LogExtractor;
pub use qa_promoter::QaPromoter;
pub use quality_scorer::QualityScorer;
pub use repo_knowledge::RepoKnowledgeManager;
pub use review_classifier::ReviewClassifier;
pub use strategy_parser::StrategyParser;
