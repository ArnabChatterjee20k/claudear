//! System 7: Score fix quality based on merge velocity and review feedback.

use claudear_core::types::{FixQualityScore, PrRecord};

pub struct QualityScorer;

impl QualityScorer {
    /// Compute fix quality score (0.0 - 1.0) from PR metrics.
    pub fn compute(pr: &PrRecord) -> FixQualityScore {
        // Merge speed (50% weight): exponential decay with 2hr half-life
        let merge_speed = if let Some(mins) = pr.time_to_merge_mins {
            let mins = mins.max(0) as f64;
            1.0 / (1.0 + (mins / 120.0))
        } else {
            0.5 // Default if no merge time yet
        };

        // Review cycles (30% weight): fewer cycles = better
        let review_cycles = 1.0 / (1.0 + pr.review_cycles as f64);

        // Approval signal (20% weight): 2+ approvals = full score
        let approvals = (pr.approvals_count as f64 / 2.0).min(1.0);

        let score = merge_speed * 0.5 + review_cycles * 0.3 + approvals * 0.2;

        FixQualityScore {
            score,
            merge_speed_component: merge_speed,
            review_cycles_component: review_cycles,
            approval_component: approvals,
        }
    }

    /// Apply quality weighting to a base confidence score.
    pub fn weight_confidence(base_confidence: f64, quality_score: f64) -> f64 {
        // Blend: 70% base confidence, 30% quality-weighted
        base_confidence * 0.7 + (base_confidence * quality_score) * 0.3
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_pr(time_to_merge_mins: Option<i64>, review_cycles: i32, approvals: i32) -> PrRecord {
        PrRecord {
            id: 1,
            pr_url: "https://github.com/foo/bar/pull/1".to_string(),
            scm_repo: "foo/bar".to_string(),
            pr_number: 1,
            attempt_id: Some(1),
            issue_id: None,
            issue_source: None,
            title: None,
            description: None,
            author: None,
            head_branch: None,
            base_branch: None,
            status: "merged".to_string(),
            created_at: Utc::now(),
            updated_at: None,
            merged_at: Some(Utc::now()),
            closed_at: None,
            approvals_count: approvals,
            changes_requested_count: 0,
            comments_count: 0,
            last_review_at: None,
            time_to_first_review_mins: None,
            time_to_merge_mins,
            review_cycles,
            files_changed: None,
            lines_added: None,
            lines_removed: None,
        }
    }

    #[test]
    fn test_fast_merge_high_score() {
        let pr = make_pr(Some(10), 0, 2);
        let score = QualityScorer::compute(&pr);
        // Fast merge, no review cycles, 2 approvals => high score
        assert!(score.score > 0.8, "expected > 0.8, got {}", score.score);
    }

    #[test]
    fn test_slow_merge_lower_score() {
        let pr = make_pr(Some(480), 3, 0);
        let score = QualityScorer::compute(&pr);
        // Slow merge, many review cycles, no approvals => lower score
        assert!(score.score < 0.3, "expected < 0.3, got {}", score.score);
    }

    #[test]
    fn test_weight_confidence() {
        let weighted = QualityScorer::weight_confidence(0.8, 1.0);
        assert!((weighted - 0.8).abs() < 0.01);

        let weighted_low = QualityScorer::weight_confidence(0.8, 0.0);
        assert!((weighted_low - 0.56).abs() < 0.01);
    }

    #[test]
    fn test_no_merge_time_defaults() {
        let pr = make_pr(None, 0, 0);
        let score = QualityScorer::compute(&pr);
        // No merge time => 0.5 merge speed, 0 cycles => 1.0 review, 0 approvals => 0.0
        // 0.5*0.5 + 1.0*0.3 + 0.0*0.2 = 0.25 + 0.3 + 0.0 = 0.55
        assert!(
            (score.score - 0.55).abs() < 0.01,
            "expected ~0.55, got {}",
            score.score
        );
        assert!((score.merge_speed_component - 0.5).abs() < 0.01);
        assert!((score.review_cycles_component - 1.0).abs() < 0.01);
        assert!((score.approval_component - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_instant_merge_perfect_approval() {
        let pr = make_pr(Some(0), 0, 3);
        let score = QualityScorer::compute(&pr);
        // 0 mins => 1/(1+0) = 1.0 merge speed, 0 cycles => 1.0, 3 approvals => min(1.5, 1.0) = 1.0
        // 1.0*0.5 + 1.0*0.3 + 1.0*0.2 = 1.0
        assert!(
            (score.score - 1.0).abs() < 0.01,
            "expected ~1.0, got {}",
            score.score
        );
    }

    #[test]
    fn test_120min_half_life() {
        let pr = make_pr(Some(120), 0, 0);
        let score = QualityScorer::compute(&pr);
        // 120 mins => 1/(1+1) = 0.5 merge speed
        assert!((score.merge_speed_component - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_review_cycles_decay() {
        let pr0 = make_pr(Some(60), 0, 0);
        let pr1 = make_pr(Some(60), 1, 0);
        let pr3 = make_pr(Some(60), 3, 0);
        let s0 = QualityScorer::compute(&pr0);
        let s1 = QualityScorer::compute(&pr1);
        let s3 = QualityScorer::compute(&pr3);
        // More review cycles => lower score
        assert!(s0.score > s1.score, "0 cycles should beat 1 cycle");
        assert!(s1.score > s3.score, "1 cycle should beat 3 cycles");
        // Verify review component values
        assert!((s0.review_cycles_component - 1.0).abs() < 0.01);
        assert!((s1.review_cycles_component - 0.5).abs() < 0.01);
        assert!((s3.review_cycles_component - 0.25).abs() < 0.01);
    }

    #[test]
    fn test_approvals_cap_at_two() {
        let pr2 = make_pr(Some(60), 0, 2);
        let pr5 = make_pr(Some(60), 0, 5);
        let s2 = QualityScorer::compute(&pr2);
        let s5 = QualityScorer::compute(&pr5);
        // Both should have approval_component = 1.0
        assert!((s2.approval_component - 1.0).abs() < 0.01);
        assert!((s5.approval_component - 1.0).abs() < 0.01);
        assert!((s2.score - s5.score).abs() < 0.01);
    }

    #[test]
    fn test_weight_confidence_zero_base() {
        let w = QualityScorer::weight_confidence(0.0, 1.0);
        assert!((w - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_weight_confidence_half_quality() {
        // base=1.0, quality=0.5: 1.0*0.7 + (1.0*0.5)*0.3 = 0.7 + 0.15 = 0.85
        let w = QualityScorer::weight_confidence(1.0, 0.5);
        assert!((w - 0.85).abs() < 0.01);
    }

    #[test]
    fn test_score_always_between_0_and_1() {
        let test_cases = vec![
            make_pr(Some(0), 0, 10),      // best case
            make_pr(Some(10000), 100, 0), // worst case
            make_pr(None, 0, 0),          // defaults
            make_pr(Some(1), 1, 1),       // moderate
        ];
        for pr in test_cases {
            let score = QualityScorer::compute(&pr);
            assert!(
                score.score >= 0.0 && score.score <= 1.0,
                "Score {} out of [0, 1] range",
                score.score
            );
            assert!(score.merge_speed_component >= 0.0 && score.merge_speed_component <= 1.0);
            assert!(score.review_cycles_component >= 0.0 && score.review_cycles_component <= 1.0);
            assert!(score.approval_component >= 0.0 && score.approval_component <= 1.0);
        }
    }

    #[test]
    fn test_negative_merge_time_clamped_to_zero() {
        // Negative time_to_merge_mins should be treated as 0 (instant merge)
        let pr_neg = make_pr(Some(-60), 0, 0);
        let pr_zero = make_pr(Some(0), 0, 0);
        let score_neg = QualityScorer::compute(&pr_neg);
        let score_zero = QualityScorer::compute(&pr_zero);
        // Negative clamped to 0, so both should produce identical scores
        assert!((score_neg.score - score_zero.score).abs() < 0.001);
        assert!(
            score_neg.score <= 1.0,
            "Score {} exceeds 1.0",
            score_neg.score
        );
        assert!((score_neg.merge_speed_component - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_very_large_merge_time() {
        let pr = make_pr(Some(i64::MAX), 0, 0);
        let score = QualityScorer::compute(&pr);
        // Merge speed approaches 0 for very large times
        assert!(score.merge_speed_component.is_finite());
        assert!(score.merge_speed_component >= 0.0);
        assert!(score.merge_speed_component < 0.001);
    }

    #[test]
    fn test_very_large_review_cycles() {
        let pr = make_pr(Some(60), i32::MAX, 0);
        let score = QualityScorer::compute(&pr);
        // 1/(1+MAX) approaches 0
        assert!(score.review_cycles_component.is_finite());
        assert!(score.review_cycles_component >= 0.0);
        assert!(score.review_cycles_component < 0.001);
    }

    #[test]
    fn test_weight_confidence_both_one() {
        let w = QualityScorer::weight_confidence(1.0, 1.0);
        // 1.0*0.7 + (1.0*1.0)*0.3 = 1.0
        assert!((w - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_weight_confidence_both_zero() {
        let w = QualityScorer::weight_confidence(0.0, 0.0);
        assert!((w - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_weight_confidence_negative_base() {
        // Negative base confidence (edge case - shouldn't happen but verify no panic)
        let w = QualityScorer::weight_confidence(-1.0, 1.0);
        assert!(w.is_finite());
    }

    #[test]
    fn test_weight_confidence_above_one_quality() {
        // Quality score > 1.0 (edge case)
        let w = QualityScorer::weight_confidence(0.5, 2.0);
        // 0.5*0.7 + (0.5*2.0)*0.3 = 0.35 + 0.30 = 0.65
        assert!((w - 0.65).abs() < 0.01);
    }

    #[test]
    fn test_zero_review_cycles_is_best() {
        let pr = make_pr(Some(60), 0, 1);
        let score = QualityScorer::compute(&pr);
        assert!((score.review_cycles_component - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_one_approval_is_half() {
        let pr = make_pr(Some(60), 0, 1);
        let score = QualityScorer::compute(&pr);
        assert!((score.approval_component - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_compute_exact_score_fast_merge_no_reviews_one_approval() {
        // 10 mins => 1/(1+10/120) = 1/1.0833 = 0.923
        // 0 cycles => 1/(1+0) = 1.0
        // 1 approval => 0.5/1.0 = 0.5
        // score = 0.923*0.5 + 1.0*0.3 + 0.5*0.2 = 0.4615 + 0.3 + 0.1 = 0.8615
        let pr = make_pr(Some(10), 0, 1);
        let score = QualityScorer::compute(&pr);
        let expected_merge = 1.0 / (1.0 + 10.0 / 120.0);
        assert!((score.merge_speed_component - expected_merge).abs() < 0.001);
        assert!((score.review_cycles_component - 1.0).abs() < 0.001);
        assert!((score.approval_component - 0.5).abs() < 0.001);
        let expected_score = expected_merge * 0.5 + 1.0 * 0.3 + 0.5 * 0.2;
        assert!(
            (score.score - expected_score).abs() < 0.001,
            "Expected {:.4}, got {:.4}",
            expected_score,
            score.score
        );
    }

    #[test]
    fn test_compute_exact_components_240min_2cycles_1approval() {
        // 240 mins => 1/(1+240/120) = 1/3 = 0.333
        // 2 cycles => 1/(1+2) = 0.333
        // 1 approval => 0.5
        // score = 0.333*0.5 + 0.333*0.3 + 0.5*0.2 = 0.1667 + 0.1 + 0.1 = 0.3667
        let pr = make_pr(Some(240), 2, 1);
        let score = QualityScorer::compute(&pr);
        let expected_merge = 1.0 / 3.0;
        let expected_review = 1.0 / 3.0;
        assert!((score.merge_speed_component - expected_merge).abs() < 0.001);
        assert!((score.review_cycles_component - expected_review).abs() < 0.001);
        assert!((score.approval_component - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_compute_merge_speed_360min() {
        // 360 mins => 1/(1+360/120) = 1/4 = 0.25
        let pr = make_pr(Some(360), 0, 0);
        let score = QualityScorer::compute(&pr);
        assert!((score.merge_speed_component - 0.25).abs() < 0.001);
    }

    #[test]
    fn test_compute_merge_speed_60min() {
        // 60 mins => 1/(1+60/120) = 1/1.5 = 0.667
        let pr = make_pr(Some(60), 0, 0);
        let score = QualityScorer::compute(&pr);
        assert!((score.merge_speed_component - (2.0 / 3.0)).abs() < 0.001);
    }

    #[test]
    fn test_weight_confidence_mid_range() {
        // base=0.5, quality=0.5: 0.5*0.7 + (0.5*0.5)*0.3 = 0.35 + 0.075 = 0.425
        let w = QualityScorer::weight_confidence(0.5, 0.5);
        assert!((w - 0.425).abs() < 0.001);
    }

    #[test]
    fn test_weight_confidence_high_base_low_quality() {
        // base=0.9, quality=0.1: 0.9*0.7 + (0.9*0.1)*0.3 = 0.63 + 0.027 = 0.657
        let w = QualityScorer::weight_confidence(0.9, 0.1);
        assert!((w - 0.657).abs() < 0.001);
    }

    #[test]
    fn test_weight_confidence_low_base_high_quality() {
        // base=0.1, quality=0.9: 0.1*0.7 + (0.1*0.9)*0.3 = 0.07 + 0.027 = 0.097
        let w = QualityScorer::weight_confidence(0.1, 0.9);
        assert!((w - 0.097).abs() < 0.001);
    }

    #[test]
    fn test_compute_monotonic_merge_speed() {
        // Merge speed should decrease as time increases
        let times = [0, 10, 30, 60, 120, 240, 480, 960, 1920];
        let mut prev_speed = f64::MAX;
        for t in times {
            let pr = make_pr(Some(t), 0, 0);
            let score = QualityScorer::compute(&pr);
            assert!(
                score.merge_speed_component <= prev_speed,
                "Merge speed should decrease: {} mins gave {}, but {} was previous",
                t,
                score.merge_speed_component,
                prev_speed
            );
            prev_speed = score.merge_speed_component;
        }
    }

    #[test]
    fn test_compute_monotonic_review_cycles() {
        // Review cycles component should decrease as cycles increase
        let mut prev_component = f64::MAX;
        for cycles in 0..=10 {
            let pr = make_pr(Some(60), cycles, 0);
            let score = QualityScorer::compute(&pr);
            assert!(
                score.review_cycles_component <= prev_component,
                "Review component should decrease: {} cycles gave {}, but {} was previous",
                cycles,
                score.review_cycles_component,
                prev_component
            );
            prev_component = score.review_cycles_component;
        }
    }

    #[test]
    fn test_compute_monotonic_approvals() {
        // Approval component should increase then cap at 2 approvals
        let mut prev_component = -1.0;
        for approvals in 0..=5 {
            let pr = make_pr(Some(60), 0, approvals);
            let score = QualityScorer::compute(&pr);
            assert!(
                score.approval_component >= prev_component,
                "Approval component should not decrease: {} approvals gave {}, previous was {}",
                approvals,
                score.approval_component,
                prev_component
            );
            prev_component = score.approval_component;
        }
    }

    #[test]
    fn test_compute_all_components_finite() {
        let test_cases = vec![
            make_pr(Some(0), 0, 0),
            make_pr(Some(i64::MAX), i32::MAX, i32::MAX),
            make_pr(None, 0, 0),
            make_pr(Some(1), 1, 1),
            make_pr(Some(-100), 0, 0),
        ];
        for pr in test_cases {
            let score = QualityScorer::compute(&pr);
            assert!(score.score.is_finite(), "Score must be finite");
            assert!(
                score.merge_speed_component.is_finite(),
                "Merge speed must be finite"
            );
            assert!(
                score.review_cycles_component.is_finite(),
                "Review cycles must be finite"
            );
            assert!(
                score.approval_component.is_finite(),
                "Approval must be finite"
            );
        }
    }

    #[test]
    fn test_weight_confidence_is_linear_in_base() {
        // weight_confidence should be linear in base_confidence for fixed quality
        let quality = 0.6;
        let w1 = QualityScorer::weight_confidence(0.2, quality);
        let w2 = QualityScorer::weight_confidence(0.4, quality);
        let w3 = QualityScorer::weight_confidence(0.6, quality);
        // Differences should be equal
        let diff1 = w2 - w1;
        let diff2 = w3 - w2;
        assert!(
            (diff1 - diff2).abs() < 0.001,
            "Should be linear: diff1={}, diff2={}",
            diff1,
            diff2
        );
    }

    #[test]
    fn test_compute_zero_approvals_gives_zero_approval_component() {
        let pr = make_pr(Some(60), 0, 0);
        let score = QualityScorer::compute(&pr);
        assert!((score.approval_component - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_compute_two_cycles() {
        // 2 cycles => 1/(1+2) = 1/3
        let pr = make_pr(Some(60), 2, 0);
        let score = QualityScorer::compute(&pr);
        assert!((score.review_cycles_component - (1.0 / 3.0)).abs() < 0.001);
    }

    #[test]
    fn test_weight_confidence_preserves_zero_base_regardless_of_quality() {
        for quality in [0.0, 0.5, 1.0, 2.0] {
            let w = QualityScorer::weight_confidence(0.0, quality);
            assert!(
                (w - 0.0).abs() < 0.001,
                "Zero base should give zero weighted confidence, got {} for quality {}",
                w,
                quality
            );
        }
    }

    #[test]
    fn test_compute_score_weights_sum_to_one() {
        // When all components are 1.0, score should be 1.0 (0.5+0.3+0.2)
        let pr = make_pr(Some(0), 0, 2);
        let score = QualityScorer::compute(&pr);
        assert!((score.merge_speed_component - 1.0).abs() < 0.001);
        assert!((score.review_cycles_component - 1.0).abs() < 0.001);
        assert!((score.approval_component - 1.0).abs() < 0.001);
        assert!(
            (score.score - 1.0).abs() < 0.001,
            "When all components are 1.0, weighted sum should be 1.0"
        );
    }
}
