//! Cost decomposition pipeline.
//!
//! Port of `skills/claude-usage-db/scripts/decompose_cost.py` and
//! `flamegraph.py`. Methodology and per-stage rationale documented in
//! `skills/claude-usage-db/references/cost-decomposition-methodology.md`.
//!
//! Attributes every billed dollar over a `--days` window to a hierarchical
//! category tree: bucket → category → subcat → stream → scope. Output is a d3
//! flame-graph tree where node values are USD cents.
//!
//! Methodology summary:
//!   - Per turn pair `(aT, aT+1)`: `cc(T+1) − prev_out_tok` is the exact
//!     billing-tokens that entered cache from the user side.
//!   - Within a turn, distribute tokens across categories proportional to
//!     effective_chars = raw_chars + overhead_tok × chars_per_tok × N_blocks.
//!   - Two regimes: non-event turns scale-to-fit; event turns keep raw
//!     chars-derived est_tok and route the residual to `cache_bust:<event>`.
//!   - Lifecycle cache-read: cumulative chars-share at T-1 × cr_cost(T).
//!   - Output-write cost split across asst_text / asst_thinking /
//!     asst_tool_use_json by chars-share of the assistant's content blocks.

use std::collections::BTreeMap;

use duckdb::Connection;
use serde_json::{json, Value};

// ── Calibration (token-calibration.md) ────────────────────────────────────────

const ASST_TEXT_CPT: f64 = 3.0;
const ASST_TOOL_CPT: f64 = 2.93;
const ASST_TOOL_OVH: i32 = 100;
const USER_TEXT_CPT: f64 = 3.0;
const USER_TEXT_OVH: i32 = 50;
const ATTACH_CPT: f64 = 3.29;
const ATTACH_OVH: i32 = 108;
const TOOL_RESULT_DEFAULT_CPT: f64 = 2.8;
const TOOL_RESULT_DEFAULT_OVH: i32 = 150;

/// Per-tool overrides for tool_result calibration.
const TOOL_CALIB: &[(&str, f64, i32)] = &[
    ("Bash", 2.6, 200),
    ("Read", 3.0, 200),
    ("Grep", 2.6, 190),
    ("Glob", 2.9, 75),
    ("WebFetch", 3.17, 0),
    ("WebSearch", 2.31, 100),
    ("Agent", 2.77, 200),
    ("Edit", 3.0, 120),
    ("Write", 3.0, 120),
    ("AskUserQuestion", 1.93, 100),
    ("TaskUpdate", 2.5, 120),
    ("TaskCreate", 2.5, 100),
];

// ── Public API ────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DecompResult {
    /// d3 tree (root node with `name`, `value` (USD cents), `children`).
    pub tree: Value,
    /// `SUM(cost_usd)` in the window — anchor for reconciliation.
    pub total_billed_usd: f64,
    /// Sum of every leaf's USD value — should match billed within ~1 %.
    pub total_attributed_usd: f64,
    pub days: i32,
    pub computed_at_iso: String,
}

/// Run the full pipeline against `db_path` (read-only) over the last `days`.
pub fn compute(db_path: &str, days: i32) -> Result<DecompResult, String> {
    let conn = Connection::open(db_path).map_err(|e| format!("open {db_path}: {e}"))?;
    build_calibration_table(&conn)?;
    run_pipeline(&conn, days)?;
    build_turn_attribution(&conn)?;
    build_attribution_tables(&conn)?;

    let cc_rows = aggregate_cc(&conn)?;
    let cr_rows = aggregate_cr(&conn)?;
    let ow_rows = aggregate_output_write(&conn)?;
    let fresh_rows = aggregate_fresh(&conn)?;
    let total_billed_usd = total_billed(&conn)?;

    let rows = collect_rows(&cc_rows, &cr_rows, &ow_rows, &fresh_rows);
    let (tree, total_attributed_usd) = build_tree(&rows);

    Ok(DecompResult {
        tree,
        total_billed_usd,
        total_attributed_usd,
        days,
        computed_at_iso: chrono::Utc::now().to_rfc3339(),
    })
}

// ── Pipeline ──────────────────────────────────────────────────────────────────

fn build_calibration_table(conn: &Connection) -> Result<(), String> {
    let values: Vec<String> = TOOL_CALIB
        .iter()
        .map(|(name, cpt, ovh)| format!("('{name}', {cpt}, {ovh})"))
        .collect();
    let sql = format!(
        "CREATE OR REPLACE TEMP TABLE tool_calib AS \
         SELECT * FROM (VALUES {}) AS t(tool_name, chars_per_tok, overhead_tok)",
        values.join(", ")
    );
    conn.execute(&sql, [])
        .map_err(|e| format!("tool_calib: {e}"))?;
    Ok(())
}

fn run_pipeline(conn: &Connection, days: i32) -> Result<(), String> {
    exec(conn, "model_rates", SQL_MODEL_RATES.to_string())?;
    exec(
        conn,
        "turn_costs",
        SQL_TURN_COSTS.replace("{days}", &days.to_string()),
    )?;
    exec(
        conn,
        "asst_block_chars",
        SQL_ASST_BLOCKS.replace("{days}", &days.to_string()),
    )?;
    exec(conn, "iv_tool_results", SQL_USER_TOOL_RESULTS.to_string())?;
    exec(conn, "iv_user_text", SQL_USER_TEXT.to_string())?;
    exec(conn, "iv_attachments", SQL_ATTACHMENTS.to_string())?;
    exec(conn, "first_turn_user", SQL_FIRST_TURN_USER.to_string())?;
    exec(conn, "turn_events", SQL_TURN_EVENTS.to_string())?;
    Ok(())
}

fn build_turn_attribution(conn: &Connection) -> Result<(), String> {
    exec(conn, "turn_attribution", SQL_TURN_ATTRIBUTION.to_string())
}

fn build_attribution_tables(conn: &Connection) -> Result<(), String> {
    let per_turn_user_est = format!(
        r#"CREATE OR REPLACE TEMP TABLE per_turn_user_est AS
        WITH cats AS (
          SELECT itr.file_path, itr.aT_eid,
                 'tool_result:' || itr.tool_name AS category,
                 itr.subcat,
                 itr.raw_chars / COALESCE(tc.chars_per_tok, {tr_cpt})
                    + COALESCE(tc.overhead_tok, {tr_ovh}) * itr.n_blocks AS est_tok,
                 itr.n_blocks
          FROM iv_tool_results itr
          LEFT JOIN tool_calib tc ON tc.tool_name = itr.tool_name
          UNION ALL
          SELECT file_path, aT_eid,
                 'attachment:' || attachment_type AS category,
                 '' AS subcat,
                 raw_chars / {at_cpt} + {at_ovh} * n_blocks AS est_tok,
                 n_blocks
          FROM iv_attachments
          UNION ALL
          SELECT file_path, aT_eid,
                 'user_text' AS category,
                 '' AS subcat,
                 raw_chars / {ut_cpt} + {ut_ovh} * n_blocks AS est_tok,
                 n_blocks
          FROM iv_user_text
        )
        SELECT * FROM cats;"#,
        tr_cpt = TOOL_RESULT_DEFAULT_CPT,
        tr_ovh = TOOL_RESULT_DEFAULT_OVH,
        at_cpt = ATTACH_CPT,
        at_ovh = ATTACH_OVH,
        ut_cpt = USER_TEXT_CPT,
        ut_ovh = USER_TEXT_OVH,
    );
    exec(conn, "per_turn_user_est", per_turn_user_est)?;

    exec(conn, "user_side_attr", SQL_USER_SIDE_ATTR.to_string())?;

    let asst_side_attr = format!(
        r#"CREATE OR REPLACE TEMP TABLE asst_side_attr AS
        WITH per_turn AS (
          SELECT ta.file_path, ta.aT_eid, ta.is_subagent, ta.turn_idx, ta.prev_out_tok,
                 COALESCE(abc.text_chars, 0)    AS text_chars,
                 COALESCE(abc.tool_chars, 0)    AS tool_chars,
                 COALESCE(abc.n_tool_blocks, 0) AS n_tool_blocks
          FROM turn_attribution ta
          JOIN turn_costs prev
            ON prev.file_path = ta.file_path AND prev.turn_idx = ta.turn_idx - 1
          LEFT JOIN asst_block_chars abc
            ON abc.file_path = prev.file_path AND abc.message_id = prev.message_id
        ),
        est AS (
          SELECT *,
                 text_chars / {asst_text_cpt}                                    AS text_tok,
                 tool_chars / {asst_tool_cpt} + {asst_tool_ovh} * n_tool_blocks AS tool_tok
          FROM per_turn
        ),
        fin AS (
          SELECT *,
                 GREATEST(0, prev_out_tok - text_tok - tool_tok) AS think_tok,
                 text_tok + tool_tok + GREATEST(0, prev_out_tok - text_tok - tool_tok) AS total_est
          FROM est
        )
        SELECT file_path, aT_eid, 'asst_text' AS category, '' AS subcat, 0 AS n_blocks, is_subagent, turn_idx,
               prev_out_tok * (text_tok / NULLIF(total_est, 0)) AS attributed_tok
        FROM fin WHERE total_est > 0
        UNION ALL
        SELECT file_path, aT_eid, 'asst_tool_use_json' AS category, '' AS subcat, n_tool_blocks AS n_blocks, is_subagent, turn_idx,
               prev_out_tok * (tool_tok / NULLIF(total_est, 0)) AS attributed_tok
        FROM fin WHERE total_est > 0
        UNION ALL
        SELECT file_path, aT_eid, 'asst_thinking' AS category, '' AS subcat, 0 AS n_blocks, is_subagent, turn_idx,
               prev_out_tok * (think_tok / NULLIF(total_est, 0)) AS attributed_tok
        FROM fin WHERE total_est > 0;"#,
        asst_text_cpt = ASST_TEXT_CPT,
        asst_tool_cpt = ASST_TOOL_CPT,
        asst_tool_ovh = ASST_TOOL_OVH,
    );
    exec(conn, "asst_side_attr", asst_side_attr)?;

    let first_turn_attr = format!(
        r#"CREATE OR REPLACE TEMP TABLE first_turn_attr AS
        WITH ft AS (
          SELECT *, user_raw_chars / {ut_cpt}
                  + {ut_ovh} * GREATEST(n_user_blocks, 1) AS user_tok_est
          FROM first_turn_user
        )
        SELECT file_path, aT_eid, 'system_block_first_turn' AS category, '' AS subcat, 0 AS n_blocks, is_subagent,
               1 AS turn_idx, GREATEST(0, cc - user_tok_est) AS attributed_tok
        FROM ft
        UNION ALL
        SELECT file_path, aT_eid, 'user_text_first_turn' AS category, '' AS subcat, n_user_blocks AS n_blocks, is_subagent,
               1 AS turn_idx, LEAST(cc, user_tok_est) AS attributed_tok
        FROM ft;"#,
        ut_cpt = USER_TEXT_CPT,
        ut_ovh = USER_TEXT_OVH,
    );
    exec(conn, "first_turn_attr", first_turn_attr)?;

    exec(conn, "per_turn_cat_attr", SQL_PER_TURN_CAT_ATTR.to_string())?;

    Ok(())
}

fn exec(conn: &Connection, name: &str, sql: String) -> Result<(), String> {
    conn.execute(&sql, [])
        .map(|_| ())
        .map_err(|e| format!("{name}: {e}"))
}

// ── Aggregations ──────────────────────────────────────────────────────────────

/// (category, subcat, is_subagent, n_blocks, cc_cost_usd, attributed_tokens)
type CcRow = (String, String, bool, i64, f64, f64);
/// (category, subcat, is_subagent, n_blocks, cr_cost_usd)
type CrRow = (String, String, bool, i64, f64);
/// (category, is_subagent, cost_usd, tokens)
type StreamRow = (String, bool, f64, f64);

fn aggregate_cc(conn: &Connection) -> Result<Vec<CcRow>, String> {
    let sql = r#"
    WITH cc_per_turn AS (
      SELECT file_path, entry_id, cc5m, cc1h, cc5m_rate, cc1h_rate,
             (cc5m * cc5m_rate + cc1h * cc1h_rate) / 1e6 AS cc_cost_usd
      FROM turn_costs
    ),
    cat_with_total AS (
      SELECT pca.*, ct.cc_cost_usd,
             SUM(pca.attributed_tok) OVER (PARTITION BY pca.file_path, pca.aT_eid) AS total_at_turn
      FROM per_turn_cat_attr pca
      JOIN cc_per_turn ct ON ct.file_path = pca.file_path AND ct.entry_id = pca.aT_eid
    )
    SELECT category, subcat, is_subagent, COALESCE(SUM(n_blocks), 0) AS n_blocks,
           COALESCE(SUM(attributed_tok / NULLIF(total_at_turn, 0) * cc_cost_usd), 0) AS cc_cost_usd,
           COALESCE(SUM(attributed_tok), 0) AS attributed_tokens
    FROM cat_with_total
    GROUP BY 1, 2, 3
    "#;
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("aggregate_cc prepare: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, bool>(2)?,
                row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
            ))
        })
        .map_err(|e| format!("aggregate_cc query: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("aggregate_cc row: {e}"))?);
    }
    Ok(out)
}

fn aggregate_cr(conn: &Connection) -> Result<Vec<CrRow>, String> {
    let sql = r#"
    WITH cat_per_turn AS (
      SELECT file_path, aT_eid, turn_idx, category, subcat, is_subagent,
             SUM(attributed_tok) AS attributed_tok,
             SUM(n_blocks) AS n_blocks
      FROM per_turn_cat_attr GROUP BY 1, 2, 3, 4, 5, 6
    ),
    file_categories AS (
      SELECT DISTINCT file_path, category, subcat, is_subagent FROM cat_per_turn
    ),
    file_turns AS (
      SELECT DISTINCT file_path, aT_eid, turn_idx FROM cat_per_turn
    ),
    grid AS (
      SELECT t.file_path, t.aT_eid, t.turn_idx, c.category, c.subcat, c.is_subagent
      FROM file_turns t
      JOIN file_categories c ON c.file_path = t.file_path
    ),
    filled AS (
      SELECT g.*, COALESCE(c.attributed_tok, 0) AS attributed_tok,
             COALESCE(c.n_blocks, 0) AS n_blocks
      FROM grid g
      LEFT JOIN cat_per_turn c USING (file_path, aT_eid, turn_idx, category, subcat, is_subagent)
    ),
    cum_through AS (
      SELECT *,
             SUM(attributed_tok) OVER (PARTITION BY file_path, category, subcat
                                       ORDER BY turn_idx ROWS UNBOUNDED PRECEDING) AS cum_tok_through
      FROM filled
    ),
    cum_before AS (
      SELECT *,
             COALESCE(LAG(cum_tok_through) OVER (PARTITION BY file_path, category, subcat
                                                 ORDER BY turn_idx), 0) AS cum_tok_before
      FROM cum_through
    ),
    file_turn_total AS (
      SELECT file_path, aT_eid, turn_idx, SUM(cum_tok_before) AS cum_total_before
      FROM cum_before GROUP BY 1, 2, 3
    ),
    cr_per_turn AS (
      SELECT file_path, entry_id, cr / 1e6 * cr_rate AS cr_cost_usd
      FROM turn_costs
    ),
    joined AS (
      SELECT cb.category, cb.subcat, cb.is_subagent, cb.n_blocks,
             cb.cum_tok_before, ftt.cum_total_before, cr.cr_cost_usd
      FROM cum_before cb
      JOIN file_turn_total ftt USING (file_path, aT_eid, turn_idx)
      JOIN cr_per_turn cr ON cr.file_path = cb.file_path AND cr.entry_id = cb.aT_eid
    )
    SELECT category, subcat, is_subagent, MAX(n_blocks) AS n_blocks,
           SUM(CASE WHEN cum_total_before > 0
                    THEN cum_tok_before / cum_total_before * cr_cost_usd
                    ELSE 0 END) AS cr_cost_usd
    FROM joined
    GROUP BY 1, 2, 3
    "#;
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("aggregate_cr prepare: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, bool>(2)?,
                row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
            ))
        })
        .map_err(|e| format!("aggregate_cr query: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("aggregate_cr row: {e}"))?);
    }
    Ok(out)
}

fn aggregate_output_write(conn: &Connection) -> Result<Vec<StreamRow>, String> {
    let sql = format!(
        r#"
    WITH per_turn AS (
      SELECT tc.file_path, tc.entry_id, tc.message_id, tc.output_tokens, tc.out_rate, tc.is_subagent,
             COALESCE(abc.text_chars, 0)    AS text_chars,
             COALESCE(abc.tool_chars, 0)    AS tool_chars,
             COALESCE(abc.n_tool_blocks, 0) AS n_tool_blocks
      FROM turn_costs tc
      LEFT JOIN asst_block_chars abc
        ON abc.file_path = tc.file_path AND abc.message_id = tc.message_id
    ),
    est AS (
      SELECT *,
             text_chars / {asst_text_cpt}                                    AS text_tok,
             tool_chars / {asst_tool_cpt} + {asst_tool_ovh} * n_tool_blocks AS tool_tok
      FROM per_turn
    ),
    fin AS (
      SELECT *,
             GREATEST(0, output_tokens - text_tok - tool_tok) AS think_tok,
             (text_tok + tool_tok + GREATEST(0, output_tokens - text_tok - tool_tok)) AS total_tok
      FROM est
    )
    SELECT category, is_subagent,
           SUM(tok_attributed / 1e6 * out_rate) AS write_cost_usd,
           SUM(tok_attributed) AS tokens
    FROM (
      SELECT 'asst_text' AS category, is_subagent,
             output_tokens * (text_tok / NULLIF(total_tok, 0)) AS tok_attributed, out_rate
      FROM fin WHERE total_tok > 0
      UNION ALL
      SELECT 'asst_tool_use_json' AS category, is_subagent,
             output_tokens * (tool_tok / NULLIF(total_tok, 0)) AS tok_attributed, out_rate
      FROM fin WHERE total_tok > 0
      UNION ALL
      SELECT 'asst_thinking' AS category, is_subagent,
             output_tokens * (think_tok / NULLIF(total_tok, 0)) AS tok_attributed, out_rate
      FROM fin WHERE total_tok > 0
    ) x GROUP BY 1, 2
    "#,
        asst_text_cpt = ASST_TEXT_CPT,
        asst_tool_cpt = ASST_TOOL_CPT,
        asst_tool_ovh = ASST_TOOL_OVH,
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("aggregate_output_write prepare: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, bool>(1)?,
                row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            ))
        })
        .map_err(|e| format!("aggregate_output_write query: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("aggregate_output_write row: {e}"))?);
    }
    Ok(out)
}

fn aggregate_fresh(conn: &Connection) -> Result<Vec<StreamRow>, String> {
    let sql = r#"
    SELECT 'fresh_input_uncategorized' AS category, is_subagent,
           SUM(fresh_in / 1e6 * in_rate) AS fresh_cost_usd,
           SUM(fresh_in) AS tokens
    FROM turn_costs GROUP BY 2
    "#;
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("aggregate_fresh prepare: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, bool>(1)?,
                row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            ))
        })
        .map_err(|e| format!("aggregate_fresh query: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("aggregate_fresh row: {e}"))?);
    }
    Ok(out)
}

fn total_billed(conn: &Connection) -> Result<f64, String> {
    let sql = "SELECT COALESCE(SUM(cost_usd), 0) FROM turn_costs";
    conn.query_row(sql, [], |row| row.get::<_, f64>(0))
        .map_err(|e| format!("total_billed: {e}"))
}

// ── Tree builder ──────────────────────────────────────────────────────────────

/// (path, value_usd)
type FlatRow = (Vec<String>, f64);

fn bucket_of(cat: &str) -> &'static str {
    if cat.starts_with("tool_result:") {
        "tool_result"
    } else if cat.starts_with("attachment:") {
        "attachment"
    } else if cat.starts_with("cache_bust:") {
        "cache_bust"
    } else if matches!(cat, "asst_text" | "asst_thinking" | "asst_tool_use_json") {
        "asst_output_persisted"
    } else if matches!(cat, "user_text" | "user_text_first_turn") {
        "user_text"
    } else if cat == "system_block_first_turn" {
        "system_block_first_turn"
    } else if cat == "fresh_input_uncategorized" {
        "fresh_input"
    } else if cat == "unaccounted" {
        "unaccounted"
    } else {
        "other"
    }
}

fn short_cat(cat: &str) -> &str {
    cat.strip_prefix("tool_result:")
        .or_else(|| cat.strip_prefix("attachment:"))
        .unwrap_or(cat)
}

fn collect_rows(
    cc: &[CcRow],
    cr: &[CrRow],
    output_write: &[StreamRow],
    fresh: &[StreamRow],
) -> Vec<FlatRow> {
    let mut rows: Vec<FlatRow> = Vec::new();

    // output_write: (cat, is_sub, cost, tok)
    for (cat, is_sub, cost, _tok) in output_write {
        let cost = *cost;
        if cost <= 0.0 {
            continue;
        }
        rows.push(make_path(cat, "", *is_sub, "output_write", cost));
    }
    // cc_attribution: (cat, subcat, is_sub, n_blocks, cost, tok)
    for (cat, subcat, is_sub, _n, cost, _tok) in cc {
        let cost = *cost;
        if cost <= 0.0 {
            continue;
        }
        rows.push(make_path(cat, subcat, *is_sub, "cache_create", cost));
    }
    // cr_attribution: (cat, subcat, is_sub, n_blocks, cost)
    for (cat, subcat, is_sub, _n, cost) in cr {
        let cost = *cost;
        if cost <= 0.0 {
            continue;
        }
        rows.push(make_path(cat, subcat, *is_sub, "cache_read", cost));
    }
    // fresh: (cat, is_sub, cost, tok)
    for (cat, is_sub, cost, _tok) in fresh {
        let cost = *cost;
        if cost <= 0.0 {
            continue;
        }
        rows.push(make_path(cat, "", *is_sub, "fresh_input", cost));
    }
    rows
}

fn make_path(cat: &str, subcat: &str, is_sub: bool, stream: &str, cost: f64) -> FlatRow {
    let mut path = vec![bucket_of(cat).to_string(), short_cat(cat).to_string()];
    if !subcat.is_empty() {
        path.push(subcat.to_string());
    }
    path.push(stream.to_string());
    path.push(if is_sub { "subagent" } else { "main" }.to_string());
    (path, cost)
}

/// Build a hierarchical d3 tree where node values are USD cents (i64) so the
/// flame-graph can sum without floating drift. Returns the tree and the sum of
/// every leaf's USD cost (USD, for the root header).
fn build_tree(rows: &[FlatRow]) -> (Value, f64) {
    #[derive(Default)]
    struct Node {
        value_cents: i64,
        children: BTreeMap<String, Node>,
    }
    let mut root = Node::default();

    for (path, value_usd) in rows {
        let cents = (*value_usd * 100.0).round() as i64;
        if cents <= 0 {
            continue;
        }
        root.value_cents += cents;
        let mut cursor = &mut root;
        for frame in path {
            cursor = cursor.children.entry(frame.clone()).or_default();
            cursor.value_cents += cents;
        }
    }

    fn to_value(name: &str, n: &Node) -> Value {
        let children: Vec<Value> = n.children.iter().map(|(k, v)| to_value(k, v)).collect();
        json!({
            "name":     name,
            "value":    n.value_cents,
            "children": children,
        })
    }

    let total_usd = root.value_cents as f64 / 100.0;
    (to_value("all", &root), total_usd)
}

// ── SQL constants (verbatim from decompose_cost.py) ───────────────────────────

const SQL_MODEL_RATES: &str = r#"
CREATE OR REPLACE TEMP TABLE model_rates AS
SELECT m AS asst_model,
       (SELECT cache_read_per_mtok        FROM model_pricing p WHERE m LIKE p.model || '%' ORDER BY LENGTH(p.model) DESC LIMIT 1) AS cr_rate,
       (SELECT cache_creation_5m_per_mtok FROM model_pricing p WHERE m LIKE p.model || '%' ORDER BY LENGTH(p.model) DESC LIMIT 1) AS cc5m_rate,
       (SELECT cache_creation_1h_per_mtok FROM model_pricing p WHERE m LIKE p.model || '%' ORDER BY LENGTH(p.model) DESC LIMIT 1) AS cc1h_rate,
       (SELECT input_per_mtok             FROM model_pricing p WHERE m LIKE p.model || '%' ORDER BY LENGTH(p.model) DESC LIMIT 1) AS in_rate,
       (SELECT output_per_mtok            FROM model_pricing p WHERE m LIKE p.model || '%' ORDER BY LENGTH(p.model) DESC LIMIT 1) AS out_rate
FROM (SELECT DISTINCT model FROM assistant_entries_deduped WHERE model IS NOT NULL AND model != '<synthetic>') x(m);
"#;

const SQL_TURN_COSTS: &str = r#"
CREATE OR REPLACE TEMP TABLE turn_costs AS
WITH base AS (
  SELECT e.file_path, e.session_id, e.entry_id, e.timestamp,
         d.message_id, d.model, d.output_tokens,
         COALESCE(d.cache_creation_5m, 0)           AS cc5m,
         COALESCE(d.cache_creation_1h, 0)           AS cc1h,
         COALESCE(d.cache_creation_input_tokens, 0) AS cc,
         COALESCE(d.cache_read_input_tokens, 0)     AS cr,
         COALESCE(d.input_tokens, 0)                AS fresh_in,
         d.cost_usd,
         mr.cc5m_rate, mr.cc1h_rate, mr.cr_rate, mr.in_rate, mr.out_rate,
         CASE WHEN COALESCE(d.cache_creation_input_tokens, 0) > 0
              THEN (COALESCE(d.cache_creation_5m, 0) * mr.cc5m_rate
                  + COALESCE(d.cache_creation_1h, 0) * mr.cc1h_rate)
                  / d.cache_creation_input_tokens
              ELSE mr.cc5m_rate END AS cc_rate_blended,
         t.is_subagent
  FROM assistant_entries_deduped d
  JOIN entries e     ON e.entry_id = d.entry_id
  JOIN transcripts t ON t.file_path = e.file_path
  JOIN model_rates mr ON mr.asst_model = d.model
  WHERE e.timestamp >= CAST(CURRENT_TIMESTAMP AS TIMESTAMP) - INTERVAL '{days} days'
    AND d.model != '<synthetic>'
)
SELECT b.*,
       ROW_NUMBER() OVER w AS turn_idx,
       COALESCE(SUM(b.cr_rate) OVER (PARTITION BY b.file_path ORDER BY b.entry_id
                                     ROWS BETWEEN 1 FOLLOWING AND UNBOUNDED FOLLOWING), 0) AS future_cr_weight,
       LAG(b.output_tokens) OVER w AS prev_out_tok,
       LAG(b.entry_id) OVER w      AS prev_eid,
       LAG(b.timestamp) OVER w     AS prev_ts
FROM base b
WINDOW w AS (PARTITION BY b.file_path ORDER BY b.entry_id);
"#;

const SQL_ASST_BLOCKS: &str = r#"
CREATE OR REPLACE TEMP TABLE asst_block_chars AS
SELECT e.file_path, ae.message_id,
       SUM(CASE WHEN acb.block_type='text'     THEN LENGTH(acb.text) ELSE 0 END) AS text_chars,
       SUM(CASE WHEN acb.block_type='tool_use' THEN LENGTH(CAST(acb.tool_input AS VARCHAR)) ELSE 0 END) AS tool_chars,
       SUM(CASE WHEN acb.block_type='tool_use' THEN 1 ELSE 0 END) AS n_tool_blocks
FROM assistant_content_blocks acb
JOIN entries e          ON e.entry_id = acb.entry_id
JOIN assistant_entries ae ON ae.entry_id = e.entry_id
WHERE e.timestamp >= CAST(CURRENT_TIMESTAMP AS TIMESTAMP) - INTERVAL '{days} days'
GROUP BY 1, 2;
"#;

const SQL_USER_TOOL_RESULTS: &str = r#"
CREATE OR REPLACE TEMP TABLE iv_tool_results AS
WITH iv AS (
  SELECT tc.file_path, tc.entry_id AS aT_eid, tc.prev_eid,
         COALESCE(tu.name, 'UNKNOWN') AS tool_name,
         COALESCE(
           CASE
             WHEN tu.name = 'Bash' AND tu.input_command IS NOT NULL
               THEN SUBSTR(
                      REGEXP_REPLACE(
                        REGEXP_EXTRACT(
                          REGEXP_REPLACE(
                            REGEXP_REPLACE(tu.input_command, '^\s+', ''),
                            '^([A-Za-z_][A-Za-z0-9_]*=\S*\s+)+', ''
                          ),
                          '^\S+', 0
                        ),
                        '^.*/', ''
                      ),
                      1, 40
                    )
             WHEN tu.name IN ('Read', 'Edit', 'Write', 'NotebookEdit')
                  AND tu.file_ext IS NOT NULL
               THEN SUBSTR(REGEXP_REPLACE(tu.file_ext, '^.*/', ''), 1, 30)
             WHEN tu.name = 'Agent'
               THEN JSON_EXTRACT_STRING(tu.input, '$.subagent_type')
             WHEN tu.name = 'WebFetch'
               THEN REGEXP_EXTRACT(JSON_EXTRACT_STRING(tu.input, '$.url'),
                                   '://([^/]+)', 1)
             ELSE NULL
           END,
           ''
         ) AS subcat,
         LENGTH(CAST(ucb.tool_result_content AS VARCHAR)) AS chars
  FROM turn_costs tc
  JOIN entries ue ON ue.file_path = tc.file_path
                  AND ue.entry_id > tc.prev_eid
                  AND ue.entry_id < tc.entry_id
                  AND ue.type = 'user'
  JOIN user_content_blocks ucb ON ucb.entry_id = ue.entry_id
                              AND ucb.block_type = 'tool_result'
  LEFT JOIN tool_uses tu ON tu.tool_use_id = ucb.tool_use_id
  WHERE tc.prev_eid IS NOT NULL
)
SELECT file_path, aT_eid, tool_name, subcat,
       SUM(chars) AS raw_chars,
       COUNT(*)  AS n_blocks
FROM iv GROUP BY 1, 2, 3, 4;
"#;

const SQL_USER_TEXT: &str = r#"
CREATE OR REPLACE TEMP TABLE iv_user_text AS
WITH ucb_text AS (
  SELECT tc.file_path, tc.entry_id AS aT_eid,
         LENGTH(ucb.text) AS chars
  FROM turn_costs tc
  JOIN entries ue ON ue.file_path = tc.file_path
                  AND ue.entry_id > tc.prev_eid
                  AND ue.entry_id < tc.entry_id
                  AND ue.type = 'user'
  JOIN user_content_blocks ucb ON ucb.entry_id = ue.entry_id
                              AND ucb.block_type = 'text'
                              AND ucb.text IS NOT NULL
  WHERE tc.prev_eid IS NOT NULL
),
uet_text AS (
  SELECT tc.file_path, tc.entry_id AS aT_eid,
         LENGTH(uet.message_content_text) AS chars
  FROM turn_costs tc
  JOIN entries ue ON ue.file_path = tc.file_path
                  AND ue.entry_id > tc.prev_eid
                  AND ue.entry_id < tc.entry_id
                  AND ue.type = 'user'
  JOIN user_entries uet ON uet.entry_id = ue.entry_id
                       AND uet.message_content_text IS NOT NULL
  WHERE tc.prev_eid IS NOT NULL
),
unioned AS (
  SELECT * FROM ucb_text UNION ALL SELECT * FROM uet_text
)
SELECT file_path, aT_eid,
       SUM(chars) AS raw_chars,
       COUNT(*)   AS n_blocks
FROM unioned
GROUP BY 1, 2;
"#;

const SQL_ATTACHMENTS: &str = r#"
CREATE OR REPLACE TEMP TABLE iv_attachments AS
WITH attach_chars AS (
  SELECT tc.file_path, tc.entry_id AS aT_eid,
         COALESCE(att.attachment_type, 'unknown') AS attachment_type,
         (COALESCE(LENGTH(att.hook_content),0)            + COALESCE(LENGTH(att.hook_stdout),0)
        + COALESCE(LENGTH(att.hook_stderr),0)             + COALESCE(LENGTH(att.hook_command),0)
        + COALESCE(LENGTH(att.file_content_text),0)       + COALESCE(LENGTH(att.directory_content),0)
        + COALESCE(LENGTH(att.skill_listing_content),0)   + COALESCE(LENGTH(CAST(att.task_reminder_content AS VARCHAR)),0)
        + COALESCE(LENGTH(att.nested_memory_content),0)   + COALESCE(LENGTH(att.queued_command_prompt),0)
        + COALESCE(LENGTH(CAST(att.diagnostics_files AS VARCHAR)),0)
        + COALESCE(LENGTH(CAST(att.invoked_skills AS VARCHAR)),0)
        + COALESCE(LENGTH(CAST(att.deferred_added_lines AS VARCHAR)),0)
        + COALESCE(LENGTH(CAST(att.deferred_added_names AS VARCHAR)),0)
        + COALESCE(LENGTH(CAST(att.deferred_removed_names AS VARCHAR)),0)
        + COALESCE(LENGTH(CAST(att.mcp_added_blocks AS VARCHAR)),0)
        + COALESCE(LENGTH(CAST(att.mcp_added_names AS VARCHAR)),0)
        + COALESCE(LENGTH(CAST(att.mcp_removed_names AS VARCHAR)),0)
        + COALESCE(LENGTH(CAST(att.command_allowed_tools AS VARCHAR)),0)
        + COALESCE(LENGTH(CAST(att.skill_names AS VARCHAR)),0)
         ) AS chars
  FROM turn_costs tc
  JOIN entries ae ON ae.file_path = tc.file_path
                 AND ae.entry_id > tc.prev_eid
                 AND ae.entry_id < tc.entry_id
                 AND ae.type = 'attachment'
  JOIN attachment_entries att ON att.entry_id = ae.entry_id
  WHERE tc.prev_eid IS NOT NULL
)
SELECT file_path, aT_eid, attachment_type,
       SUM(chars) AS raw_chars,
       COUNT(*)  AS n_blocks
FROM attach_chars GROUP BY 1, 2, 3;
"#;

const SQL_FIRST_TURN_USER: &str = r#"
CREATE OR REPLACE TEMP TABLE first_turn_user AS
WITH first_turn AS (
  SELECT file_path, entry_id, cc, cc_rate_blended AS cc_rate, future_cr_weight, is_subagent
  FROM turn_costs WHERE turn_idx = 1
),
ucb_chars AS (
  SELECT ft.file_path, ft.entry_id AS aT_eid,
         COALESCE(SUM(LENGTH(ucb.text)), 0)
         + COALESCE(SUM(LENGTH(CAST(ucb.tool_result_content AS VARCHAR))), 0) AS chars,
         COUNT(ucb.entry_id) AS n_blocks
  FROM first_turn ft
  LEFT JOIN entries ue ON ue.file_path = ft.file_path
                       AND ue.entry_id < ft.entry_id
                       AND ue.type = 'user'
  LEFT JOIN user_content_blocks ucb ON ucb.entry_id = ue.entry_id
  GROUP BY 1, 2
),
uet_chars AS (
  SELECT ft.file_path, ft.entry_id AS aT_eid,
         COALESCE(SUM(LENGTH(uet.message_content_text)), 0) AS chars,
         COUNT(uet.entry_id) AS n_blocks
  FROM first_turn ft
  LEFT JOIN entries ue ON ue.file_path = ft.file_path
                       AND ue.entry_id < ft.entry_id
                       AND ue.type = 'user'
  LEFT JOIN user_entries uet ON uet.entry_id = ue.entry_id
                            AND uet.message_content_text IS NOT NULL
  GROUP BY 1, 2
)
SELECT ft.file_path, ft.entry_id AS aT_eid, ft.cc, ft.cc_rate, ft.future_cr_weight, ft.is_subagent,
       (COALESCE(uc.chars, 0) + COALESCE(ut.chars, 0))     AS user_raw_chars,
       (COALESCE(uc.n_blocks, 0) + COALESCE(ut.n_blocks, 0)) AS n_user_blocks
FROM first_turn ft
LEFT JOIN ucb_chars uc ON uc.file_path = ft.file_path AND uc.aT_eid = ft.entry_id
LEFT JOIN uet_chars ut ON ut.file_path = ft.file_path AND ut.aT_eid = ft.entry_id;
"#;

const SQL_TURN_EVENTS: &str = r#"
CREATE OR REPLACE TEMP TABLE turn_events AS
WITH attach_flags AS (
  SELECT tc.file_path, tc.entry_id AS aT_eid,
         BOOL_OR(att.attachment_type = 'deferred_tools_delta')   AS has_dtd,
         BOOL_OR(att.attachment_type = 'mcp_instructions_delta') AS has_mcp_delta,
         BOOL_OR(att.attachment_type = 'date_change')            AS has_date_change,
         BOOL_OR(att.attachment_type = 'dynamic_skill')          AS has_dynamic_skill
  FROM turn_costs tc
  JOIN entries ae ON ae.file_path = tc.file_path
                  AND ae.entry_id > tc.prev_eid
                  AND ae.entry_id < tc.entry_id
                  AND ae.type = 'attachment'
  JOIN attachment_entries att ON att.entry_id = ae.entry_id
  WHERE tc.prev_eid IS NOT NULL
  GROUP BY 1, 2
),
compact_flags AS (
  SELECT tc.file_path, tc.entry_id AS aT_eid,
         BOOL_OR(uet.is_compact_summary) AS has_compact
  FROM turn_costs tc
  JOIN entries ue ON ue.file_path = tc.file_path
                  AND ue.entry_id > tc.prev_eid
                  AND ue.entry_id < tc.entry_id
                  AND ue.type = 'user'
  JOIN user_entries uet ON uet.entry_id = ue.entry_id
  WHERE tc.prev_eid IS NOT NULL
  GROUP BY 1, 2
)
SELECT tc.file_path, tc.entry_id AS aT_eid,
       COALESCE(cf.has_compact, false)         AS has_compact,
       COALESCE(af.has_dtd, false)             AS has_dtd,
       COALESCE(af.has_mcp_delta, false)       AS has_mcp_delta,
       COALESCE(af.has_dynamic_skill, false)   AS has_dynamic_skill,
       COALESCE(af.has_date_change, false)     AS has_date_change
FROM turn_costs tc
LEFT JOIN attach_flags  af ON af.file_path = tc.file_path AND af.aT_eid = tc.entry_id
LEFT JOIN compact_flags cf ON cf.file_path = tc.file_path AND cf.aT_eid = tc.entry_id
WHERE tc.prev_eid IS NOT NULL;
"#;

const SQL_TURN_ATTRIBUTION: &str = r#"
CREATE OR REPLACE TEMP TABLE turn_attribution AS
SELECT tc.file_path, tc.entry_id AS aT_eid, tc.message_id, tc.is_subagent, tc.session_id,
       tc.cc, tc.cc5m, tc.cc1h, tc.cr, tc.fresh_in, tc.output_tokens AS out_tok, tc.cost_usd,
       tc.cc_rate_blended AS cc_rate, tc.cc5m_rate, tc.cc1h_rate,
       tc.cr_rate, tc.in_rate, tc.out_rate, tc.future_cr_weight,
       tc.prev_out_tok, tc.turn_idx
FROM turn_costs tc
WHERE tc.turn_idx >= 2;
"#;

const SQL_USER_SIDE_ATTR: &str = r#"
CREATE OR REPLACE TEMP TABLE user_side_attr AS
WITH per_turn AS (
  SELECT ta.file_path, ta.aT_eid, ta.is_subagent, ta.turn_idx,
         GREATEST(0, ta.cc - COALESCE(ta.prev_out_tok, 0)) AS user_intervening_tok
  FROM turn_attribution ta
),
sums AS (
  SELECT file_path, aT_eid, SUM(est_tok) AS total_est
  FROM per_turn_user_est GROUP BY 1, 2
),
labelled AS (
  SELECT pt.file_path, pt.aT_eid, pt.is_subagent, pt.turn_idx,
         pt.user_intervening_tok,
         COALESCE(s.total_est, 0) AS total_est,
         CASE WHEN te.has_compact        THEN 'cache_bust:compact_summary'
              WHEN te.has_dtd            THEN 'cache_bust:deferred_tools_delta'
              WHEN te.has_mcp_delta      THEN 'cache_bust:mcp_instructions_delta'
              WHEN te.has_dynamic_skill  THEN 'cache_bust:dynamic_skill'
              WHEN te.has_date_change    THEN 'cache_bust:date_change'
              ELSE NULL END AS event_label
  FROM per_turn pt
  LEFT JOIN sums s USING (file_path, aT_eid)
  LEFT JOIN turn_events te USING (file_path, aT_eid)
),
cat_attr AS (
  SELECT u.file_path, u.aT_eid, u.category, u.subcat, u.n_blocks, j.is_subagent, j.turn_idx,
         CASE
           WHEN j.event_label IS NOT NULL AND j.total_est > j.user_intervening_tok
             THEN u.est_tok * j.user_intervening_tok / j.total_est
           WHEN j.event_label IS NOT NULL
             THEN u.est_tok
           WHEN j.total_est > 0
             THEN u.est_tok * j.user_intervening_tok / j.total_est
           ELSE 0
         END AS attributed_tok
  FROM per_turn_user_est u
  JOIN labelled j USING (file_path, aT_eid)
),
cache_bust_excess AS (
  SELECT j.file_path, j.aT_eid, j.event_label AS category, '' AS subcat, 0 AS n_blocks,
         j.is_subagent, j.turn_idx,
         GREATEST(0, j.user_intervening_tok - j.total_est) AS attributed_tok
  FROM labelled j
  WHERE j.event_label IS NOT NULL
),
unaccounted AS (
  SELECT j.file_path, j.aT_eid, 'unaccounted' AS category, '' AS subcat, 0 AS n_blocks,
         j.is_subagent, j.turn_idx, j.user_intervening_tok AS attributed_tok
  FROM labelled j
  WHERE j.event_label IS NULL
    AND j.total_est = 0
    AND j.user_intervening_tok > 0
)
SELECT * FROM cat_attr WHERE attributed_tok > 0
UNION ALL
SELECT * FROM cache_bust_excess WHERE attributed_tok > 0
UNION ALL
SELECT * FROM unaccounted WHERE attributed_tok > 0;
"#;

const SQL_PER_TURN_CAT_ATTR: &str = r#"
CREATE OR REPLACE TEMP TABLE per_turn_cat_attr AS
SELECT file_path, aT_eid, category, subcat, n_blocks, is_subagent, turn_idx, attributed_tok FROM user_side_attr
UNION ALL
SELECT file_path, aT_eid, category, subcat, n_blocks, is_subagent, turn_idx, attributed_tok FROM asst_side_attr
UNION ALL
SELECT file_path, aT_eid, category, subcat, n_blocks, is_subagent, turn_idx, attributed_tok FROM first_turn_attr;
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_routing() {
        assert_eq!(bucket_of("tool_result:Bash"), "tool_result");
        assert_eq!(bucket_of("attachment:hook_success"), "attachment");
        assert_eq!(bucket_of("cache_bust:compact_summary"), "cache_bust");
        assert_eq!(bucket_of("asst_thinking"), "asst_output_persisted");
        assert_eq!(bucket_of("user_text"), "user_text");
        assert_eq!(
            bucket_of("system_block_first_turn"),
            "system_block_first_turn"
        );
        assert_eq!(bucket_of("fresh_input_uncategorized"), "fresh_input");
    }

    #[test]
    fn short_cat_strips_prefix() {
        assert_eq!(short_cat("tool_result:Bash"), "Bash");
        assert_eq!(short_cat("attachment:hook_success"), "hook_success");
        assert_eq!(short_cat("asst_thinking"), "asst_thinking");
    }

    #[test]
    fn make_path_with_subcat() {
        let row = make_path("tool_result:Bash", "git", false, "cache_create", 1.50);
        assert_eq!(
            row.0,
            vec!["tool_result", "Bash", "git", "cache_create", "main"]
        );
        assert!((row.1 - 1.50).abs() < 1e-9);
    }

    #[test]
    fn make_path_no_subcat() {
        let row = make_path("user_text", "", true, "cache_read", 2.25);
        assert_eq!(
            row.0,
            vec!["user_text", "user_text", "cache_read", "subagent"]
        );
    }

    #[test]
    fn build_tree_aggregates() {
        let rows = vec![
            (
                vec![
                    "tool_result".into(),
                    "Bash".into(),
                    "git".into(),
                    "cache_create".into(),
                    "main".into(),
                ],
                1.0,
            ),
            (
                vec![
                    "tool_result".into(),
                    "Bash".into(),
                    "git".into(),
                    "cache_read".into(),
                    "main".into(),
                ],
                2.0,
            ),
        ];
        let (tree, total) = build_tree(&rows);
        assert!((total - 3.0).abs() < 1e-9);
        assert_eq!(tree["value"].as_i64(), Some(300));
    }
}
