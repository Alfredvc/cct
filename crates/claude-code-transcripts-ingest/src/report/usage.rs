//! `cct report usage` — token & cost breakdown by model.
//!
//! Aggregates `assistant_entries_deduped` (the cost-correct table — see the
//! `cct-db` skill for why summing the raw table double-counts) joined to
//! `entries` for cwd / timestamp filtering.

use std::path::Path;

use duckdb::{params_from_iter, Connection};
use serde::Serialize;

use crate::cli::UsageArgs;
use crate::scope::Scope;

#[derive(Debug, Serialize)]
struct ModelRow {
    model: String,
    calls: i64,
    input_tokens: i64,
    cache_write_5m: i64,
    cache_write_1h: i64,
    cache_read: i64,
    output_tokens: i64,
    cost_usd: f64,
}

#[derive(Debug, Serialize)]
struct Totals {
    calls: i64,
    input_tokens: i64,
    cache_write_5m: i64,
    cache_write_1h: i64,
    cache_read: i64,
    output_tokens: i64,
    cost_usd: f64,
    cache_hit_rate: f64,
    first_event: Option<String>,
    last_event: Option<String>,
    duration_seconds: Option<i64>,
}

#[derive(Debug, Serialize)]
struct UsageReport {
    db: String,
    scope: ScopeOut,
    by_model: Vec<ModelRow>,
    totals: Totals,
}

#[derive(Debug, Serialize)]
struct ScopeOut {
    project: Option<String>,
    all: bool,
    no_subdirs: bool,
    from: Option<String>,
    to: Option<String>,
}

pub fn run(args: UsageArgs) {
    if let Err(e) = run_inner(args) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run_inner(args: UsageArgs) -> Result<(), String> {
    if !args.db.exists() {
        return Err(format!(
            "database not found at {} (run `cct ingest` first)",
            args.db.display()
        ));
    }

    let scope = Scope::resolve(
        args.all,
        args.project.as_deref(),
        args.no_subdirs,
        args.from.as_deref(),
        args.to.as_deref(),
    )?;

    let conn = open_readonly(&args.db)?;

    let by_model = query_by_model(&conn, &scope)?;
    let (first_event, last_event) = query_time_range(&conn, &scope)?;

    let totals = compute_totals(&by_model, first_event.clone(), last_event.clone());

    let report = UsageReport {
        db: args.db.display().to_string(),
        scope: ScopeOut {
            project: scope.cwd_exact.clone(),
            all: args.all,
            no_subdirs: args.no_subdirs,
            from: args.from.clone(),
            to: args.to.clone(),
        },
        by_model,
        totals,
    };

    if args.json {
        let s = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        println!("{s}");
    } else {
        print_text(&report);
    }
    Ok(())
}

fn open_readonly(path: &Path) -> Result<Connection, String> {
    let cfg = duckdb::Config::default()
        .access_mode(duckdb::AccessMode::ReadOnly)
        .map_err(|e| format!("config: {e}"))?;
    Connection::open_with_flags(path, cfg).map_err(|e| format!("open {}: {e}", path.display()))
}

fn query_by_model(conn: &Connection, scope: &Scope) -> Result<Vec<ModelRow>, String> {
    let mut sql = String::from(
        "SELECT \
            d.model, \
            COUNT(*) FILTER (WHERE d.message_id IS NOT NULL) AS calls, \
            COALESCE(SUM(d.input_tokens), 0) AS input_tokens, \
            COALESCE(SUM(d.cache_creation_5m), 0) AS cache_write_5m, \
            COALESCE(SUM(d.cache_creation_1h), 0) AS cache_write_1h, \
            COALESCE(SUM(d.cache_read_input_tokens), 0) AS cache_read, \
            COALESCE(SUM(d.output_tokens), 0) AS output_tokens, \
            COALESCE(SUM(d.cost_usd), 0.0) AS cost_usd \
         FROM assistant_entries_deduped d \
         JOIN entries e ON e.entry_id = d.entry_id \
         WHERE d.model IS NOT NULL AND d.model <> '<synthetic>'",
    );
    let mut params: Vec<String> = Vec::new();
    scope.append_where(&mut sql, &mut params);
    sql.push_str(" GROUP BY d.model ORDER BY cost_usd DESC, d.model");

    let mut stmt = conn.prepare(&sql).map_err(|e| format!("prepare: {e}"))?;
    let rows = stmt
        .query_map(params_from_iter(params.iter()), |row| {
            Ok(ModelRow {
                model: row.get(0)?,
                calls: row.get(1)?,
                input_tokens: row.get(2)?,
                cache_write_5m: row.get(3)?,
                cache_write_1h: row.get(4)?,
                cache_read: row.get(5)?,
                output_tokens: row.get(6)?,
                cost_usd: row.get(7)?,
            })
        })
        .map_err(|e| format!("query: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("rows: {e}"))?;
    Ok(rows)
}

fn query_time_range(
    conn: &Connection,
    scope: &Scope,
) -> Result<(Option<String>, Option<String>), String> {
    let mut sql = String::from(
        "SELECT \
            CAST(MIN(e.timestamp) AS VARCHAR), \
            CAST(MAX(e.timestamp) AS VARCHAR) \
         FROM entries e \
         WHERE 1=1",
    );
    let mut params: Vec<String> = Vec::new();
    scope.append_where(&mut sql, &mut params);

    let mut stmt = conn.prepare(&sql).map_err(|e| format!("prepare: {e}"))?;
    let row: (Option<String>, Option<String>) = stmt
        .query_row(params_from_iter(params.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|e| format!("query: {e}"))?;
    Ok(row)
}

fn compute_totals(
    rows: &[ModelRow],
    first_event: Option<String>,
    last_event: Option<String>,
) -> Totals {
    let mut t = Totals {
        calls: 0,
        input_tokens: 0,
        cache_write_5m: 0,
        cache_write_1h: 0,
        cache_read: 0,
        output_tokens: 0,
        cost_usd: 0.0,
        cache_hit_rate: 0.0,
        first_event,
        last_event,
        duration_seconds: None,
    };
    for r in rows {
        t.calls += r.calls;
        t.input_tokens += r.input_tokens;
        t.cache_write_5m += r.cache_write_5m;
        t.cache_write_1h += r.cache_write_1h;
        t.cache_read += r.cache_read;
        t.output_tokens += r.output_tokens;
        t.cost_usd += r.cost_usd;
    }
    let effective_input = t.input_tokens + t.cache_write_5m + t.cache_write_1h + t.cache_read;
    if effective_input > 0 {
        t.cache_hit_rate = t.cache_read as f64 / effective_input as f64;
    }
    t.duration_seconds = parse_duration(t.first_event.as_deref(), t.last_event.as_deref());
    t
}

fn parse_duration(first: Option<&str>, last: Option<&str>) -> Option<i64> {
    // DuckDB's CAST(TIMESTAMP AS VARCHAR) format is `YYYY-MM-DD HH:MM:SS[.ffffff]`.
    // We only need second-resolution math so pull the leading 19 chars.
    fn to_secs(s: &str) -> Option<i64> {
        let head = s.get(..19)?;
        let (date, time) = head.split_once(' ')?;
        let mut date_parts = date.split('-');
        let y: i32 = date_parts.next()?.parse().ok()?;
        let mo: u32 = date_parts.next()?.parse().ok()?;
        let d: u32 = date_parts.next()?.parse().ok()?;
        let mut tp = time.split(':');
        let h: u32 = tp.next()?.parse().ok()?;
        let mi: u32 = tp.next()?.parse().ok()?;
        let se: u32 = tp.next()?.parse().ok()?;
        // Days since civil epoch (Howard Hinnant's algorithm). Branchless and exact.
        let y = y - i32::from(mo <= 2);
        let era = y.div_euclid(400);
        let yoe = (y - era * 400) as u32;
        let mp = if mo > 2 { mo - 3 } else { mo + 9 };
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era as i64 * 146_097 + doe as i64 - 719_468;
        Some(days * 86_400 + (h as i64) * 3600 + (mi as i64) * 60 + se as i64)
    }
    let a = to_secs(first?)?;
    let b = to_secs(last?)?;
    Some((b - a).max(0))
}

// ── Text rendering ───────────────────────────────────────────────────────────

fn print_text(r: &UsageReport) {
    let bar = "═".repeat(96);
    let sep = "─".repeat(96);

    println!();
    println!("{bar}");
    println!("  CCT USAGE REPORT");
    println!("{bar}");
    println!("  DB:       {}", r.db);
    if r.scope.all {
        println!("  Scope:    ALL projects");
    } else if let Some(p) = &r.scope.project {
        let suffix = if r.scope.no_subdirs {
            ""
        } else {
            " (+ subdirectory cwds)"
        };
        println!("  Project:  {p}{suffix}");
    }
    if let (Some(a), Some(b)) = (&r.totals.first_event, &r.totals.last_event) {
        println!("  Window:   {a} → {b}");
    }
    println!();

    if r.by_model.is_empty() {
        println!("  No assistant entries match the requested scope.");
        println!();
        return;
    }

    // Per-model rows.
    println!("  {}", header_row());
    println!("  {sep}");
    for m in &r.by_model {
        println!("  {}", model_text_row(m));
    }
    println!("  {sep}");

    // Totals row.
    let totals_pseudo = ModelRow {
        model: "TOTAL".to_string(),
        calls: r.totals.calls,
        input_tokens: r.totals.input_tokens,
        cache_write_5m: r.totals.cache_write_5m,
        cache_write_1h: r.totals.cache_write_1h,
        cache_read: r.totals.cache_read,
        output_tokens: r.totals.output_tokens,
        cost_usd: r.totals.cost_usd,
    };
    println!("  {}", model_text_row(&totals_pseudo));
    println!();
    println!(
        "  Cache hit rate:  {:>5.1}% of effective input served from cache",
        r.totals.cache_hit_rate * 100.0
    );

    if let Some(d) = r.totals.duration_seconds {
        let (label, months_opt) = duration_summary(d);
        println!("  Duration:        {label}");
        if let Some(months) = months_opt {
            println!(
                "  Monthly avg:     ${:.2} / mo, {:.0} calls / mo  (over {:.1} months)",
                r.totals.cost_usd / months,
                r.totals.calls as f64 / months,
                months,
            );
            // Per-model monthly projection.
            println!();
            println!("  {:<32}  {:>12}", "Model", "$/mo");
            for m in &r.by_model {
                println!("  {:<32}  ${:>11.2}", m.model, m.cost_usd / months);
            }
        }
    }
    println!();
}

fn header_row() -> String {
    format!(
        "{:<32}  {:>8}  {:>14}  {:>14}  {:>14}  {:>14}  {:>14}  {:>10}",
        "Model", "Calls", "Input", "Cache W5m", "Cache W1h", "Cache Read", "Output", "Cost",
    )
}

fn model_text_row(m: &ModelRow) -> String {
    format!(
        "{:<32}  {:>8}  {:>14}  {:>14}  {:>14}  {:>14}  {:>14}  ${:>9.2}",
        truncate(&m.model, 32),
        commas(m.calls),
        commas(m.input_tokens),
        commas(m.cache_write_5m),
        commas(m.cache_write_1h),
        commas(m.cache_read),
        commas(m.output_tokens),
        m.cost_usd,
    )
}

/// Returns `(label, months)`. `months` is `Some` only when the window is at
/// least a month — projection below that is too noisy to be useful.
fn duration_summary(secs: i64) -> (String, Option<f64>) {
    let days = secs as f64 / 86_400.0;
    let label = if days < 1.0 {
        format!("{:.1} hours", secs as f64 / 3600.0)
    } else if days < 14.0 {
        format!("{days:.1} days")
    } else {
        format!("{:.1} months ({} days)", days / 30.44, days as i64)
    };
    let months = days / 30.44;
    let monthly = if months >= 1.0 { Some(months) } else { None };
    (label, monthly)
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n.saturating_sub(1)])
    }
}

fn commas(n: i64) -> String {
    let neg = n < 0;
    let mut s = n.unsigned_abs().to_string();
    let bytes: Vec<u8> = s.as_bytes().to_vec();
    s.clear();
    for (i, b) in bytes.iter().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            s.push(',');
        }
        s.push(*b as char);
    }
    let mut rev: String = s.chars().rev().collect();
    if neg {
        rev.insert(0, '-');
    }
    rev
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commas_basic() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(123), "123");
        assert_eq!(commas(1_234), "1,234");
        assert_eq!(commas(1_234_567), "1,234,567");
        assert_eq!(commas(-1_234), "-1,234");
    }

    #[test]
    fn truncate_basic() {
        assert_eq!(truncate("hi", 5), "hi");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }

    #[test]
    fn duration_under_day() {
        let (label, monthly) = duration_summary(7_200);
        assert!(label.contains("hours"));
        assert!(monthly.is_none());
    }

    #[test]
    fn duration_over_month() {
        let (label, monthly) = duration_summary(60 * 86_400);
        assert!(label.contains("months"));
        assert!(monthly.unwrap() > 1.5);
    }

    #[test]
    fn parse_duration_basic() {
        let d = parse_duration(Some("2026-05-01 00:00:00"), Some("2026-05-02 00:00:00"));
        assert_eq!(d, Some(86_400));
    }

    #[test]
    fn parse_duration_handles_microseconds() {
        let d = parse_duration(
            Some("2026-05-01 00:00:00.123456"),
            Some("2026-05-01 00:00:30.999999"),
        );
        assert_eq!(d, Some(30));
    }
}
