use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct PriceRow {
    pub model: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_creation_5m_per_mtok: f64,
    pub cache_creation_1h_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub effective_date: String, // ISO date YYYY-MM-DD
}

/// Seed pricing — USD per 1M tokens.
///
/// Every rate is a verbatim copy from the Anthropic docs. No multipliers,
/// no derivation. Sources captured on 2026-04-17:
///   - https://claude.com/pricing
///   - https://platform.claude.com/docs/en/build-with-claude/prompt-caching
///   - https://platform.claude.com/docs/en/about-claude/models/overview
///
/// Fable 5, Mythos 5, Opus 5, Opus 4.8, and Sonnet 5 were captured on
/// 2026-08-24 from https://platform.claude.com/docs/en/about-claude/pricing
/// and carry that later `effective_date`. Sonnet 5's $2/$10 launch rate is
/// permanent as of that capture — the announced 2026-09-01 rise to $3/$15
/// was cancelled.
///
/// Keys use the "alias" or date-stripped form (see `normalize_model`).
/// Rates are: (input, cache_5m, cache_1h, cache_read, output).
pub fn seed_rows() -> Vec<PriceRow> {
    let date = "2026-04-17".to_string();
    let mk_on =
        |date: &str, model: &str, input: f64, c5m: f64, c1h: f64, read: f64, output: f64| {
            PriceRow {
                model: model.to_string(),
                input_per_mtok: input,
                output_per_mtok: output,
                cache_creation_5m_per_mtok: c5m,
                cache_creation_1h_per_mtok: c1h,
                cache_read_per_mtok: read,
                effective_date: date.to_string(),
            }
        };
    let mk = |model: &str, input: f64, c5m: f64, c1h: f64, read: f64, output: f64| {
        mk_on(&date, model, input, c5m, c1h, read, output)
    };
    let aug = "2026-08-24";
    vec![
        // ── Fable family ──────────────────────────────────────────────────
        mk_on(aug, "claude-fable-5", 10.00, 12.50, 20.00, 1.00, 50.00),
        // ── Mythos family (limited availability) ──────────────────────────
        mk_on(aug, "claude-mythos-5", 10.00, 12.50, 20.00, 1.00, 50.00),
        // ── Opus family ───────────────────────────────────────────────────
        mk_on(aug, "claude-opus-5", 5.00, 6.25, 10.00, 0.50, 25.00),
        mk_on(aug, "claude-opus-4-8", 5.00, 6.25, 10.00, 0.50, 25.00),
        mk("claude-opus-4-7", 5.00, 6.25, 10.00, 0.50, 25.00),
        mk("claude-opus-4-6", 5.00, 6.25, 10.00, 0.50, 25.00),
        mk("claude-opus-4-5", 5.00, 6.25, 10.00, 0.50, 25.00),
        mk("claude-opus-4-1", 15.00, 18.75, 30.00, 1.50, 75.00),
        mk("claude-opus-4", 15.00, 18.75, 30.00, 1.50, 75.00),
        mk("claude-opus-4-0", 15.00, 18.75, 30.00, 1.50, 75.00),
        // ── Sonnet family ─────────────────────────────────────────────────
        mk_on(aug, "claude-sonnet-5", 2.00, 2.50, 4.00, 0.20, 10.00),
        mk("claude-sonnet-4-6", 3.00, 3.75, 6.00, 0.30, 15.00),
        mk("claude-sonnet-4-5", 3.00, 3.75, 6.00, 0.30, 15.00),
        mk("claude-sonnet-4", 3.00, 3.75, 6.00, 0.30, 15.00),
        mk("claude-sonnet-4-0", 3.00, 3.75, 6.00, 0.30, 15.00),
        mk("claude-3-7-sonnet", 3.00, 3.75, 6.00, 0.30, 15.00),
        // ── Haiku family ──────────────────────────────────────────────────
        mk("claude-haiku-4-5", 1.00, 1.25, 2.00, 0.10, 5.00),
        mk("claude-3-5-haiku", 0.80, 1.00, 1.60, 0.08, 4.00),
        mk("claude-3-haiku", 0.25, 0.30, 0.50, 0.03, 1.25),
        // ── Family defaults — used when an unknown ID falls through to
        //    family_key(). Tracks current-flagship rates (Fable 5,
        //    Mythos 5, Opus 5, Sonnet 5, Haiku 4.5). ──────────────────────
        mk_on(aug, "claude-opus", 5.00, 6.25, 10.00, 0.50, 25.00),
        mk_on(aug, "claude-sonnet", 2.00, 2.50, 4.00, 0.20, 10.00),
        mk("claude-haiku", 1.00, 1.25, 2.00, 0.10, 5.00),
        mk_on(aug, "claude-fable", 10.00, 12.50, 20.00, 1.00, 50.00),
        mk_on(aug, "claude-mythos", 10.00, 12.50, 20.00, 1.00, 50.00),
    ]
}

#[derive(Debug, Deserialize)]
struct PricingFile {
    #[serde(default)]
    models: Vec<TomlPriceRow>,
}

#[derive(Debug, Deserialize)]
struct TomlPriceRow {
    model: String,
    input_per_mtok: f64,
    output_per_mtok: f64,
    cache_creation_5m_per_mtok: Option<f64>,
    cache_creation_1h_per_mtok: Option<f64>,
    cache_read_per_mtok: Option<f64>,
    effective_date: Option<String>,
}

pub fn load_overrides(path: &Path) -> Result<Vec<PriceRow>, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let parsed: PricingFile =
        toml::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(parsed
        .models
        .into_iter()
        .map(|t| PriceRow {
            cache_creation_5m_per_mtok: t
                .cache_creation_5m_per_mtok
                .unwrap_or(t.input_per_mtok * 1.25),
            cache_creation_1h_per_mtok: t
                .cache_creation_1h_per_mtok
                .unwrap_or(t.input_per_mtok * 2.0),
            cache_read_per_mtok: t.cache_read_per_mtok.unwrap_or(t.input_per_mtok * 0.10),
            effective_date: t.effective_date.unwrap_or_else(|| "1970-01-01".to_string()),
            model: t.model,
            input_per_mtok: t.input_per_mtok,
            output_per_mtok: t.output_per_mtok,
        })
        .collect())
}

/// Merge seed + overrides (overrides win on `model` collision).
pub fn merge(seed: Vec<PriceRow>, overrides: Vec<PriceRow>) -> Vec<PriceRow> {
    let mut map: HashMap<String, PriceRow> =
        seed.into_iter().map(|r| (r.model.clone(), r)).collect();
    for r in overrides {
        map.insert(r.model.clone(), r);
    }
    map.into_values().collect()
}

/// Look up a price row, or return None when the model is unknown.
pub fn build_lookup(rows: &[PriceRow]) -> HashMap<String, PriceRow> {
    rows.iter().map(|r| (r.model.clone(), r.clone())).collect()
}

/// Strip trailing `-YYYYMMDD` build-date suffix from model names.
/// e.g. `claude-haiku-4-5-20251001` → `claude-haiku-4-5`
fn normalize_model(model: &str) -> &str {
    // Check if the last segment is exactly 8 decimal digits (a date like 20251001)
    if let Some(pos) = model.rfind('-') {
        let suffix = &model[pos + 1..];
        if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
            return &model[..pos];
        }
    }
    model
}

/// Map a model ID to its family default pricing key.
/// Used as a last-resort fallback when the exact ID and the date-stripped
/// form are both absent from the pricing table.
fn family_key(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();
    if m.contains("fable") {
        Some("claude-fable")
    } else if m.contains("mythos") {
        Some("claude-mythos")
    } else if m.contains("opus") {
        Some("claude-opus")
    } else if m.contains("sonnet") {
        Some("claude-sonnet")
    } else if m.contains("haiku") {
        Some("claude-haiku")
    } else {
        None
    }
}

/// Compute cost in USD given a model and usage breakdown.
#[allow(clippy::too_many_arguments)]
pub fn compute_cost(
    pricing: &HashMap<String, PriceRow>,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_5m: Option<u64>,
    cache_creation_1h: Option<u64>,
    cache_creation_input_tokens_total: Option<u64>,
    cache_read_input_tokens: Option<u64>,
) -> Option<f64> {
    let p = pricing
        .get(model)
        .or_else(|| pricing.get(normalize_model(model)))
        .or_else(|| family_key(model).and_then(|k| pricing.get(k)))?;
    let (c5m, c1h) = match (cache_creation_5m, cache_creation_1h) {
        (Some(a), Some(b)) => (a, b),
        (Some(a), None) => (a, 0),
        (None, Some(b)) => (0, b),
        (None, None) => (cache_creation_input_tokens_total.unwrap_or(0), 0),
    };
    let cr = cache_read_input_tokens.unwrap_or(0);
    let cost = (input_tokens as f64) * p.input_per_mtok
        + (output_tokens as f64) * p.output_per_mtok
        + (c5m as f64) * p.cache_creation_5m_per_mtok
        + (c1h as f64) * p.cache_creation_1h_per_mtok
        + (cr as f64) * p.cache_read_per_mtok;
    Some(cost / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup() -> HashMap<String, PriceRow> {
        build_lookup(&seed_rows())
    }

    /// Cost for a 1-of-each-bucket probe. Exercises every rate field
    /// so two models with identical rates produce identical output.
    fn probe(p: &HashMap<String, PriceRow>, model: &str) -> Option<f64> {
        compute_cost(
            p,
            model,
            1_000,
            1_000,
            Some(1_000),
            Some(1_000),
            None,
            Some(1_000),
        )
    }

    #[test]
    fn date_suffix_strips_to_alias() {
        let p = lookup();
        // Dated ID routes to the same row as the alias.
        assert_eq!(
            probe(&p, "claude-haiku-4-5-20251001"),
            probe(&p, "claude-haiku-4-5"),
        );
        assert_eq!(
            probe(&p, "claude-sonnet-4-5-20250929"),
            probe(&p, "claude-sonnet-4-5"),
        );
    }

    #[test]
    fn family_fallback_routes_to_default() {
        let p = lookup();
        // Unknown IDs fall through to the family default row.
        assert_eq!(probe(&p, "claude-opus-9-9"), probe(&p, "claude-opus"));
        assert_eq!(probe(&p, "claude-sonnet-next"), probe(&p, "claude-sonnet"));
        assert_eq!(probe(&p, "claude-haiku-99"), probe(&p, "claude-haiku"));
        assert_eq!(probe(&p, "claude-fable-6"), probe(&p, "claude-fable"));
    }

    /// Sonnet 5 is cheaper than Sonnet 4.6 ($2/$10 vs $3/$15) — a rare case
    /// where a newer model in the same family lowers the rate, so it must have
    /// its own row rather than inherit an older sibling's.
    #[test]
    fn sonnet_5_is_priced_below_sonnet_4_6() {
        let p = lookup();
        let s5 = probe(&p, "claude-sonnet-5").expect("sonnet-5 priced");
        let s46 = probe(&p, "claude-sonnet-4-6").expect("sonnet-4-6 priced");
        // 1k of each bucket at 2.00 + 2.50 + 4.00 + 0.20 + 10.00 per Mtok.
        assert!((s5 - 0.0187).abs() < 1e-9, "unexpected sonnet-5 cost {s5}");
        assert!(s5 < s46, "sonnet-5 should undercut sonnet-4-6");
    }

    /// Explicit rows for the current flagships must agree with the family
    /// default they used to fall through to.
    #[test]
    fn opus_5_and_4_8_match_the_opus_family_rate() {
        let p = lookup();
        assert_eq!(probe(&p, "claude-opus-5"), probe(&p, "claude-opus"));
        assert_eq!(probe(&p, "claude-opus-4-8"), probe(&p, "claude-opus"));
    }

    #[test]
    fn mythos_matches_fable_rates() {
        let p = lookup();
        assert_eq!(probe(&p, "claude-mythos-5"), probe(&p, "claude-fable-5"));
        assert_eq!(
            probe(&p, "claude-mythos-preview"),
            probe(&p, "claude-mythos")
        );
    }

    #[test]
    fn fable_is_priced_and_distinct_from_opus() {
        let p = lookup();
        let fable = probe(&p, "claude-fable-5").expect("fable-5 priced");
        let opus = probe(&p, "claude-opus-4-7").expect("opus-4-7 priced");
        // 1k of each bucket at 10.00 + 12.50 + 20.00 + 1.00 + 50.00 per Mtok.
        assert!(
            (fable - 0.0935).abs() < 1e-9,
            "unexpected fable cost {fable}"
        );
        assert!(fable > opus, "fable should out-price opus");
    }

    #[test]
    fn unknown_family_returns_none() {
        let p = lookup();
        assert!(probe(&p, "<synthetic>").is_none());
        assert!(probe(&p, "gpt-4").is_none());
    }
}
