use crate::types::Usage;

/// Last updated date for the pricing table.
pub const LAST_UPDATED: &str = "2026-03";

/// Per-model pricing in dollars per million tokens.
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

/// Look up pricing for a model by substring matching on the model ID.
pub fn lookup(model: &str) -> Option<ModelPricing> {
    let m = model.to_lowercase();

    if m.contains("opus") {
        Some(ModelPricing {
            input_per_mtok: 5.0,
            output_per_mtok: 25.0,
        })
    } else if m.contains("sonnet") {
        Some(ModelPricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
        })
    } else if m.contains("haiku") {
        Some(ModelPricing {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
        })
    } else {
        None
    }
}

/// Estimate cost in dollars for the given model and token usage.
pub fn estimate(model: &str, usage: &Usage) -> Option<f64> {
    let pricing = lookup(model)?;
    let input_cost = (usage.input_tokens as f64 / 1_000_000.0) * pricing.input_per_mtok;
    let output_cost = (usage.output_tokens as f64 / 1_000_000.0) * pricing.output_per_mtok;
    Some(input_cost + output_cost)
}

/// Format a human-readable cost string, e.g. `"$0.31"`.
pub fn format_cost(model: &str, usage: &Usage) -> Option<String> {
    let cost = estimate(model, usage)?;
    Some(format!("${:.2}", cost))
}

/// Cost estimate for a pipeline run.
#[derive(Debug, Clone)]
pub struct CostEstimate {
    pub model: String,
    pub spent: Usage,
    pub estimated_remaining: Usage,
    pub spent_cost: Option<f64>,
    pub remaining_cost: Option<f64>,
    pub total_cost: Option<f64>,
}

impl CostEstimate {
    pub fn new(model: &str, spent: Usage, estimated_remaining: Usage) -> Self {
        let spent_cost = estimate(model, &spent);
        let remaining_cost = estimate(model, &estimated_remaining);
        let total_cost = match (spent_cost, remaining_cost) {
            (Some(s), Some(r)) => Some(s + r),
            _ => None,
        };
        Self {
            model: model.to_string(),
            spent,
            estimated_remaining,
            spent_cost,
            remaining_cost,
            total_cost,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_sonnet() {
        let p = lookup("claude-sonnet-4-5-20250929").unwrap();
        assert_eq!(p.input_per_mtok, 3.0);
        assert_eq!(p.output_per_mtok, 15.0);
    }

    #[test]
    fn lookup_haiku() {
        let p = lookup("claude-haiku-4-5-20251001").unwrap();
        assert_eq!(p.input_per_mtok, 1.0);
        assert_eq!(p.output_per_mtok, 5.0);
    }

    #[test]
    fn lookup_opus() {
        let p = lookup("claude-opus-4-6").unwrap();
        assert_eq!(p.input_per_mtok, 5.0);
        assert_eq!(p.output_per_mtok, 25.0);
    }

    #[test]
    fn lookup_unknown() {
        assert!(lookup("gpt-4o").is_none());
    }

    #[test]
    fn estimate_cost() {
        let usage = Usage {
            input_tokens: 62_000,
            output_tokens: 8_000,
        };
        let cost = estimate("claude-sonnet-4-5-20250929", &usage).unwrap();
        // 62K * 3/1M + 8K * 15/1M = 0.186 + 0.12 = 0.306
        assert!((cost - 0.306).abs() < 0.001);
    }

    #[test]
    fn format_cost_display() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
        };
        let s = format_cost("claude-sonnet-4-5-20250929", &usage).unwrap();
        // 1M * 3/1M + 100K * 15/1M = 3.0 + 1.5 = 4.5
        assert_eq!(s, "$4.50");
    }

    #[test]
    fn cost_estimate_struct() {
        let spent = Usage {
            input_tokens: 62_000,
            output_tokens: 8_000,
        };
        let remaining = Usage {
            input_tokens: 440_000,
            output_tokens: 55_000,
        };
        let est = CostEstimate::new("claude-sonnet-4-5-20250929", spent, remaining);
        assert!(est.spent_cost.is_some());
        assert!(est.remaining_cost.is_some());
        assert!(est.total_cost.is_some());
    }
}
