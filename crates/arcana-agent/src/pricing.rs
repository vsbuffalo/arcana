use crate::types::Usage;

/// Last updated date for the pricing table.
pub const LAST_UPDATED: &str = "2026-06";

/// Model family — the base price tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Opus,
    Sonnet,
    Haiku,
    Fable,
}

/// A model's pricing identity: its family plus the major generation parsed from
/// the id (`claude-opus-4-8` → 4, `claude-3-5-sonnet-…` → 3, `claude-fable-5`
/// → 5). Keying the price table on this — rather than a bare `opus`/`sonnet`
/// substring — means a *future* generation that hasn't been priced yet returns
/// `None` (a deliberate miss) instead of silently inheriting a stale price.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelKey {
    pub family: Family,
    pub generation: u32,
}

/// Per-model pricing in dollars per million tokens.
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

impl ModelPricing {
    /// Cache *reads* (tokens served from the prompt cache) bill at 0.1× input.
    pub fn cache_read_per_mtok(&self) -> f64 {
        self.input_per_mtok * 0.1
    }

    /// Cache *writes* (5-minute ephemeral breakpoints) bill at 1.25× input.
    pub fn cache_write_per_mtok(&self) -> f64 {
        self.input_per_mtok * 1.25
    }
}

/// Parse a model id into a `(family, generation)` key, or `None` if the family
/// is unrecognized (e.g. a non-Claude model).
pub fn parse_model(model: &str) -> Option<ModelKey> {
    let m = model.to_lowercase();
    let family = if m.contains("opus") {
        Family::Opus
    } else if m.contains("sonnet") {
        Family::Sonnet
    } else if m.contains("haiku") {
        Family::Haiku
    } else if m.contains("fable") || m.contains("mythos") {
        Family::Fable
    } else {
        return None;
    };
    let generation = first_integer(&m)?;
    Some(ModelKey { family, generation })
}

/// First run of ASCII digits in the id — the major Claude generation.
fn first_integer(s: &str) -> Option<u32> {
    let mut digits = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else if !digits.is_empty() {
            break;
        }
    }
    digits.parse().ok()
}

/// Look up base pricing for a model. Returns `None` for unrecognized families
/// *and* for unpriced generations — adding a new generation is a deliberate
/// edit here, never an implicit inheritance.
pub fn lookup(model: &str) -> Option<ModelPricing> {
    let key = parse_model(model)?;
    let (input_per_mtok, output_per_mtok) = match (key.family, key.generation) {
        // Claude 4.x line (4.5 / 4.6 / 4.7 / 4.8 share a tier within a family).
        (Family::Opus, 4) => (5.0, 25.0),
        (Family::Sonnet, 4) => (3.0, 15.0),
        (Family::Haiku, 4) => (1.0, 5.0),
        // Fable / Mythos 5.
        (Family::Fable, 5) => (10.0, 50.0),
        _ => return None,
    };
    Some(ModelPricing {
        input_per_mtok,
        output_per_mtok,
    })
}

/// Estimate cost in dollars for the given model and token usage, accounting for
/// the cheaper cache-read and pricier cache-write rates.
pub fn estimate(model: &str, usage: &Usage) -> Option<f64> {
    let p = lookup(model)?;
    let input = (usage.input_tokens as f64 / 1_000_000.0) * p.input_per_mtok;
    let output = (usage.output_tokens as f64 / 1_000_000.0) * p.output_per_mtok;
    let cache_read = (usage.cache_read_tokens as f64 / 1_000_000.0) * p.cache_read_per_mtok();
    let cache_write = (usage.cache_creation_tokens as f64 / 1_000_000.0) * p.cache_write_per_mtok();
    Some(input + output + cache_read + cache_write)
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
    fn lookup_fable() {
        let p = lookup("claude-fable-5").unwrap();
        assert_eq!(p.input_per_mtok, 10.0);
        assert_eq!(p.output_per_mtok, 50.0);
    }

    #[test]
    fn lookup_unknown() {
        assert!(lookup("gpt-4o").is_none());
    }

    #[test]
    fn parse_model_extracts_family_and_generation() {
        assert_eq!(
            parse_model("claude-opus-4-8"),
            Some(ModelKey {
                family: Family::Opus,
                generation: 4
            })
        );
        assert_eq!(
            parse_model("claude-3-5-sonnet-20241022"),
            Some(ModelKey {
                family: Family::Sonnet,
                generation: 3
            })
        );
        assert_eq!(
            parse_model("claude-fable-5"),
            Some(ModelKey {
                family: Family::Fable,
                generation: 5
            })
        );
        assert!(parse_model("gpt-4o").is_none());
    }

    #[test]
    fn unpriced_generation_returns_none() {
        // A hypothetical future Opus generation must NOT silently inherit the
        // gen-4 price — it returns None until a price is added deliberately.
        assert!(lookup("claude-opus-7-0").is_none());
        // Retired gen-3 families are likewise unpriced.
        assert!(lookup("claude-3-5-sonnet-20241022").is_none());
    }

    #[test]
    fn estimate_cost() {
        let usage = Usage {
            input_tokens: 62_000,
            output_tokens: 8_000,
            ..Default::default()
        };
        let cost = estimate("claude-sonnet-4-5-20250929", &usage).unwrap();
        // 62K * 3/1M + 8K * 15/1M = 0.186 + 0.12 = 0.306
        assert!((cost - 0.306).abs() < 0.001);
    }

    #[test]
    fn cache_reads_are_a_tenth_of_input() {
        // 100K tokens served from cache vs. processed fresh as input.
        let cached = Usage {
            cache_read_tokens: 100_000,
            ..Default::default()
        };
        let fresh = Usage {
            input_tokens: 100_000,
            ..Default::default()
        };
        let c = estimate("claude-sonnet-4-5", &cached).unwrap();
        let f = estimate("claude-sonnet-4-5", &fresh).unwrap();
        assert!(c < f, "cache hits must be cheaper than fresh input");
        assert!(
            (c - f * 0.1).abs() < 1e-9,
            "cache read is exactly 0.1× input"
        );
    }

    #[test]
    fn cache_writes_carry_a_premium() {
        let write = Usage {
            cache_creation_tokens: 1_000_000,
            ..Default::default()
        };
        let cost = estimate("claude-sonnet-4-5", &write).unwrap();
        // 1M cache-write tokens at 1.25 × $3 = $3.75
        assert!((cost - 3.75).abs() < 1e-9);
    }

    #[test]
    fn cache_hits_lower_a_run_estimate() {
        // Same total input volume; the cached run must estimate cheaper.
        let no_cache = Usage {
            input_tokens: 100_000,
            output_tokens: 5_000,
            ..Default::default()
        };
        let with_cache = Usage {
            input_tokens: 10_000,
            output_tokens: 5_000,
            cache_read_tokens: 90_000,
            ..Default::default()
        };
        let plain = estimate("claude-sonnet-4-5", &no_cache).unwrap();
        let cached = estimate("claude-sonnet-4-5", &with_cache).unwrap();
        assert!(cached < plain);
    }

    #[test]
    fn format_cost_display() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            ..Default::default()
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
            ..Default::default()
        };
        let remaining = Usage {
            input_tokens: 440_000,
            output_tokens: 55_000,
            ..Default::default()
        };
        let est = CostEstimate::new("claude-sonnet-4-5-20250929", spent, remaining);
        assert!(est.spent_cost.is_some());
        assert!(est.remaining_cost.is_some());
        assert!(est.total_cost.is_some());
    }
}
