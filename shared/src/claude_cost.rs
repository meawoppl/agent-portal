//! Claude cost scopes shared by the driver and durable result accounting.

use claude_codes::io::ResultMessage;

/// Modern results report disjoint call costs; older results report a process
/// cumulative snapshot. Keeping the scopes explicit prevents summing snapshots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClaudeResultCost {
    Delta(f64),
    Cumulative(f64),
    Unavailable,
}

impl ClaudeResultCost {
    pub fn from_result(result: &ResultMessage) -> Self {
        if let Some(models) = &result.model_usage_since_last_result {
            let sum = models.values().try_fold(0.0, |sum, model| {
                valid_cost(model.cost_usd).and_then(|cost| valid_cost(sum + cost))
            });
            return sum.map(Self::Delta).unwrap_or(Self::Unavailable);
        }
        valid_cost(result.total_cost_usd)
            .map(Self::Cumulative)
            .unwrap_or(Self::Unavailable)
    }
}

fn valid_cost(cost: f64) -> Option<f64> {
    (cost.is_finite() && cost >= 0.0).then_some(cost)
}

/// One tracker per CLI process, not per user turn or portal connection.
/// Modern empty maps are an explicit zero. Legacy snapshots are differenced
/// only within the process that emitted them; restarting the driver resets the
/// baseline, while reconnecting its WebSocket does not.
#[derive(Debug, Default)]
pub struct ClaudeTurnCostTracker {
    previous_cumulative: f64,
}

impl ClaudeTurnCostTracker {
    pub fn observe(&mut self, result: &ResultMessage) -> Option<f64> {
        let previous = self.previous_cumulative;
        if let Some(total) = valid_cost(result.total_cost_usd) {
            self.previous_cumulative = total;
        }
        match ClaudeResultCost::from_result(result) {
            ClaudeResultCost::Delta(delta) => Some(delta),
            ClaudeResultCost::Cumulative(total) => {
                // A counter reset must not produce negative spending.
                Some(if total >= previous {
                    total - previous
                } else {
                    total
                })
            }
            ClaudeResultCost::Unavailable => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_codes::io::ModelUsageEntry;
    use std::collections::BTreeMap;

    fn result(total: f64, delta: Option<&[f64]>) -> ResultMessage {
        let mut result: ResultMessage = serde_json::from_str(
            r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1,"duration_api_ms":1,"num_turns":1,"session_id":"s","total_cost_usd":0}"#,
        ).unwrap();
        result.total_cost_usd = total;
        result.model_usage_since_last_result = delta.map(|costs| {
            costs
                .iter()
                .enumerate()
                .map(|(i, cost)| {
                    (
                        i.to_string(),
                        ModelUsageEntry {
                            cost_usd: *cost,
                            ..Default::default()
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>()
        });
        result
    }

    #[test]
    fn modern_costs_are_disjoint_across_turns_and_process_resumes() {
        let mut tracker = ClaudeTurnCostTracker::default();
        assert_eq!(tracker.observe(&result(10.0, Some(&[1.0, 2.0]))), Some(3.0));
        assert_eq!(tracker.observe(&result(20.0, Some(&[4.0]))), Some(4.0));
        let mut resumed = ClaudeTurnCostTracker::default();
        assert_eq!(resumed.observe(&result(1.0, Some(&[0.5]))), Some(0.5));
    }

    #[test]
    fn empty_modern_map_is_zero_not_cumulative_fallback() {
        assert_eq!(
            ClaudeTurnCostTracker::default().observe(&result(99.0, Some(&[]))),
            Some(0.0)
        );
    }

    #[test]
    fn legacy_totals_are_differenced_and_tracker_is_process_scoped() {
        let mut tracker = ClaudeTurnCostTracker::default();
        assert_eq!(tracker.observe(&result(2.0, None)), Some(2.0));
        assert_eq!(tracker.observe(&result(5.0, None)), Some(3.0));
        assert_eq!(tracker.observe(&result(1.0, None)), Some(1.0));
        assert_eq!(
            ClaudeTurnCostTracker::default().observe(&result(2.0, None)),
            Some(2.0)
        );
    }

    #[test]
    fn modern_and_legacy_results_share_the_process_snapshot_baseline() {
        let mut tracker = ClaudeTurnCostTracker::default();
        assert_eq!(tracker.observe(&result(2.0, None)), Some(2.0));
        assert_eq!(tracker.observe(&result(5.0, Some(&[3.0]))), Some(3.0));
        assert_eq!(tracker.observe(&result(5.0, None)), Some(0.0));
    }

    #[test]
    fn invalid_deltas_are_unavailable_not_replaced_by_a_cumulative_total() {
        let mut tracker = ClaudeTurnCostTracker::default();
        assert_eq!(tracker.observe(&result(10.0, Some(&[-1.0]))), None);
        assert_eq!(tracker.observe(&result(10.0, Some(&[f64::INFINITY]))), None);
        assert_eq!(
            tracker.observe(&result(10.0, Some(&[f64::MAX, f64::MAX]))),
            None
        );
    }
}
