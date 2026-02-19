//! Self-evaluation system for measuring code quality before and after fix attempts.

pub mod detector;
pub mod parsers;
pub mod runner;
pub mod types;

pub use runner::CodeQualityEvaluator;
pub use types::{Diagnostic, EvalCategory, EvalDelta, EvalSnapshot, EvaluationResult};
