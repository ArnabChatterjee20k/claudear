//! Generic fallback parser using exit codes only.

use crate::evaluation::types::{EvalCategory, EvalSnapshot};

pub fn parse(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Test,
        ..Default::default()
    };

    // Count lines for basic metrics
    let error_lines = stderr
        .lines()
        .filter(|l| {
            let lower = l.to_lowercase();
            lower.contains("error") || lower.contains("fail")
        })
        .count();

    let warning_lines = stderr
        .lines()
        .filter(|l| l.to_lowercase().contains("warning"))
        .count();

    snapshot.errors = error_lines as u32;
    snapshot.warnings = warning_lines as u32;
    let _ = stdout; // suppress unused warning in generic parser

    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generic_parse() {
        let snap = parse(
            "some output",
            "error: something failed\nwarning: be careful",
        );
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.warnings, 1);
    }
}
