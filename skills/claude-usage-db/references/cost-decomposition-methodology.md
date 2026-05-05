# Cost decomposition methodology

Use this reference when the question is "where is my money going?" — attributing every billed dollar to a source category like `tool_result:Bash`, `attachment:hook_success`, `asst_thinking`, `system_block_first_turn`, or `cache_bust:compact_summary`. This goes deeper than the cumulative-cost question (`cumulative-cost-analysis.md`) — it covers the full per-turn split, including the system-block re-injection costs that no JSONL `content_block` records.

The reference implementation lives in [`scripts/decompose_cost.py`](../scripts/decompose_cost.py) (~1k lines of pipelined DuckDB SQL). The flamegraph wrapper [`scripts/flamegraph.py`](../scripts/flamegraph.py) visualizes the result. This doc is the *why* behind those scripts — read them together when adapting for a new question.

---

## The core idea: billing-exact split by chars-share

The two existing references each cover one piece:

- `token-calibration.md` — chars-per-token per category, plus per-block overhead. Lets you split a token bucket across categories with calibrated proportions.
- `cumulative-cost-analysis.md` — every cached token is paid once at creation and again on every later turn. The `lifecycle_cost = cc + Σ_{T'>T} cr` formula.

This doc combines them. For each turn pair `(aT, aT+1)`:

1. **Billing-exact tokens** are known per turn:
   - `user_intervening = cc(T+1) − output_tokens(T)` — tokens entering cache from user-side content.
   - `aT_persisted = output_tokens(T)` — tokens entering cache from the previous assistant turn.
   - `cache_creation_5m * cc5m_rate + cache_creation_1h * cc1h_rate` — exact cc cost.
   - `cache_read_input_tokens * cr_rate` — exact cr cost.

2. **Chars-share split** distributes those exact tokens across categories using `effective_chars = raw_chars + overhead_tok × chars_per_tok × N_blocks` from `token-calibration.md`. Calibration error stays within categories — the sum across categories matches billing exactly.

3. **Lifecycle cost** propagates each turn's split forward: a token in category `c` born at turn `T` accrues `cr_rate(T') × tok` at every later turn `T'`. The pipeline computes this via cumulative chars-share at `T−1` × `cr_cost(T)`.

The reconciliation gap (attributed total vs `SUM(cost_usd)`) should be <1% with current calibration. Larger gaps usually mean the pricing-JOIN dup (see `cumulative-cost-analysis.md`) or a missing model in `model_pricing`.

---

## Two regimes for user-side attribution

The user-side bucket (`user_intervening = cc(T+1) − prev_out_tok`) is the trickiest because it includes both **chars-evidenced content** (tool results, attachments, user text) and **invisible content** (system-block re-injections triggered by cache-bust events). Treat them differently:

### Non-event turn (no cache-bust event fired)

Sum chars-derived `est_tok` across categories. Scale every category to fit `user_intervening` exactly:

```
attributed_tok(c) = est_tok(c) × user_intervening / Σ est_tok
```

Calibration error redistributes between categories symmetrically — top-level totals are exact, only fine-grained category shares can drift by ±10–15%. **No residual category** — there's nothing to absorb because the chars-evidence covers all the user-side tokens we have.

### Event turn (a cache-bust event fired in the user-side window)

The cc(T+1) spike on event turns is dominated by the system-block re-injection (system prompt + tool definitions + MCP schemas + Claude Code preamble — none of which appears as a JSONL content block). Attribution:

```
chars-derived categories: keep raw est_tok (or scaled down if total_est > user_intervening)
remainder:                user_intervening − Σ est_tok  →  cache_bust:<event>
```

The remainder absorbs the invisible tokens. `cache_bust:<event>` becomes its own category in the rollup.

### Why this works

The split is **billing-exact by construction**: sum of attributed tokens at turn T = `cc(T+1) − prev_out_tok` exactly, by either regime. The only freedom is the per-category proportion within a turn, governed by chars-share. So:

- Top-level totals match `SUM(cost_usd)` within rounding.
- Category shares are stable to ~10% across reasonable calibration choices.
- Cache-bust events are *interpretable*, not noise — sessions with high `cache_bust:*` share are paying for repeated system-block re-caching.

---

## Five known cache-bust events

These are the events that deterministically invalidate the cached prefix in current Claude Code data, in priority order (first match wins when multiple fire on the same turn):

| Event | Detection | What it does |
|-------|-----------|--------------|
| `compact_summary` | `user_entries.is_compact_summary = true` | Context compaction collapsed prior turns into a summary. Forces full re-cache. |
| `deferred_tools_delta` | `attachment_type = 'deferred_tools_delta'` | ToolSearch loaded a deferred tool's schema; system-block tool list grew. |
| `mcp_instructions_delta` | `attachment_type = 'mcp_instructions_delta'` | MCP server reloaded instructions; tool descriptions changed. |
| `dynamic_skill` | `attachment_type = 'dynamic_skill'` | Plugin/skill content injected into system block. |
| `date_change` | `attachment_type = 'date_change'` | Day rolled over; the system-block `currentDate` updated. |

Detection in one query — see the SKILL.md "Cache-bust events" section for the full SQL. The pipeline's `turn_events` table flags these per turn pair.

---

## Categories produced

The pipeline rolls every billed dollar into one of these:

| Category | Source | Where it gets billed |
|----------|--------|----------------------|
| `tool_result:<Tool>` | user-side content blocks of `block_type='tool_result'` | cc(T+1) on the turn after the tool ran, plus cr on every later turn |
| `attachment:<type>` | `attachment_entries`, multi-field union | same as above |
| `user_text` | `user_content_blocks.text` + `user_entries.message_content_text` (UNION; bimodal storage) | same as above |
| `user_text_first_turn` | first user message in a session | cc(1) |
| `system_block_first_turn` | residual on cc(1) after user-text accounted | cc(1) |
| `asst_text`, `asst_thinking`, `asst_tool_use_json` | split of `output_tokens(T)` across `assistant_content_blocks` block types via chars-share | output_write rate, plus cc/cr lifecycle when those tokens enter cache as `prev_out_tok` on turn T+1 |
| `cache_bust:<event>` | event-turn residual: `user_intervening − Σ chars-evidence` | cc(T+1) on event turns |
| `unaccounted` | non-event turn with no chars-evidence and non-zero user_intervening (rare structural overhead) | cc(T+1) tail |
| `fresh_input_uncategorized` | `input_tokens` (not cached, billed at fresh-input rate) | rare in cached sessions |

`tool_result:*` and `attachment:*` are the largest. Each gets a *subcat* drill-down for further breakdown — see below.

---

## Subcat extraction (flamegraph drill-down)

For `tool_result:*` rows, a per-tool sub-key gives finer breakdown — answers "which Bash command", "which file extension", "which subagent type" rather than just "Bash" or "Read".

```sql
COALESCE(
  CASE
    WHEN tu.name = 'Bash' AND tu.input_command IS NOT NULL
      THEN SUBSTR(
             REGEXP_REPLACE(
               REGEXP_EXTRACT(
                 REGEXP_REPLACE(
                   REGEXP_REPLACE(tu.input_command, '^\s+', ''),
                   '^([A-Za-z_][A-Za-z0-9_]*=\S*\s+)+', ''),
                 '^\S+', 0),
               '^.*/', ''),
             1, 40)
    WHEN tu.name IN ('Read', 'Edit', 'Write', 'NotebookEdit')
         AND tu.file_ext IS NOT NULL
      THEN SUBSTR(REGEXP_REPLACE(tu.file_ext, '^.*/', ''), 1, 30)
    WHEN tu.name = 'Agent'
      THEN JSON_EXTRACT_STRING(tu.input, '$.subagent_type')
    WHEN tu.name = 'WebFetch'
      THEN REGEXP_EXTRACT(JSON_EXTRACT_STRING(tu.input, '$.url'), '://([^/]+)', 1)
    ELSE NULL
  END,
  ''
) AS subcat
```

Notes on the patterns:

- **Bash**: strip leading whitespace, strip leading `FOO=val ` env-var assignments, take first non-whitespace token, drop directory prefix (so `/usr/local/bin/git` becomes `git`), cap at 40 chars. Compound chains like `cd /x && git status` collapse to `cd` only — a deliberate trade-off for flamegraph clarity over completeness. If the leading-`cd` pollution becomes a problem, layer a "skip leading `cd` / `source`" pass.

- **Read / Edit / Write**: `tool_uses.file_ext` is a convenience column extracted as "everything after the first dot". For dotfile paths like `.git/hooks/pre-commit` it returns `git/hooks/pre-commit`. Strip to the last path segment (`pre-commit`) and cap at 30.

- **Agent**: `JSON_EXTRACT_STRING(input, '$.subagent_type')` returns the subagent name (`general-purpose`, `Explore`, `agentfiles:read-only-researcher`, etc.).

- **WebFetch**: domain only — the regex `://([^/]+)` strips scheme and path. Captures `github.com`, `code.claude.com`, `raw.githubusercontent.com`, etc.

- **Empty string, not NULL.** `subcat` is a non-null `VARCHAR`; tools without a meaningful sub-key (`mcp__*`, `AskUserQuestion`, `ToolSearch`, …) get `''`. Reason: DuckDB `PARTITION BY` and `USING` joins downstream drop rows under SQL NULL semantics. Empty string preserves them.

---

## Pipeline stages (decompose_cost.py)

Each `CREATE OR REPLACE TEMP TABLE` step is independently inspectable — query the table by name after running the script in interactive mode. Stages:

1. **`model_rates`** — longest-prefix join from `assistant_entries_deduped.model` to `model_pricing`. Cache-create rates split into 5m and 1h. See `cumulative-cost-analysis.md` for the JOIN footgun.

2. **`turn_costs`** — per assistant turn `T` (in window): cc/cr/fresh/output tokens, blended cc rate `(cc5m × cc5m_rate + cc1h × cc1h_rate) / cc`, lifecycle weight `Σ_{T'>T} cr_rate(T')`, lagged `prev_out_tok` and `prev_eid` (within `file_path`). Filters `model = '<synthetic>'`.

3. **`asst_block_chars`** — per assistant `message_id`, sum of chars per `block_type` (text / tool_use) and count of tool_use blocks. Used downstream to split `output_tokens` across text / thinking / tool_use_json.

4. **`iv_tool_results`** — per turn pair, chars per `(tool_name, subcat)` from intervening user `tool_result` blocks between `prev_eid` and `entry_id`. Joins `user_content_blocks` to `tool_uses` on `tool_use_id` to get the tool name.

5. **`iv_user_text`** — per turn pair, chars from intervening user text. UNIONs the two storage sources (`user_content_blocks.text` + `user_entries.message_content_text`) — see SKILL.md "User text storage is bimodal".

6. **`iv_attachments`** — per turn pair, chars per `attachment_type`. Sums **every populated content field** of `attachment_entries` (12+ columns including `deferred_added_lines`, `mcp_added_blocks`, `hook_command`, `file_content_text`, `directory_content`, `skill_listing_content`, etc.). Older versions that summed only `hook_content` missed ~50% of attachment cost.

7. **`first_turn_user`** — special case for `turn_idx = 1`: `cc(1) − u_1_chars / chars_per_tok` is the system-prompt baseline (CLAUDE.md + tool definitions + MCP schemas + Claude Code preamble). The `u_1` term sums user content blocks before the first assistant turn.

8. **`turn_events`** — per turn pair, the five cache-bust event flags (one per known event).

9. **`per_turn_cat_attr`** — UNION of three attribution sources, each producing `(file_path, aT_eid, category, subcat, n_blocks, is_subagent, turn_idx, attributed_tok)`:
   - `user_side_attr` (turn_idx ≥ 2) — two-regime split described above.
   - `asst_side_attr` (turn_idx ≥ 2) — split `prev_out_tok` across `asst_text` / `asst_thinking` / `asst_tool_use_json` by chars-share with thinking as residual: `think_tok = prev_out_tok − text_tok − tool_tok`.
   - `first_turn_attr` (turn_idx = 1) — split `cc(1)` between `system_block_first_turn` and `user_text_first_turn`.

10. **`cc_attribution`** — per category cc cost: `Σ_T (attributed_tok_c(T) / Σ attributed_tok(T)) × cc_cost(T)`. The denominator is the per-turn total of attributed tokens.

11. **`cr_attribution`** — per category cr cost across the lifecycle. Builds a `(file_path × category × turn_idx)` grid so categories without a fresh allocation in turn `T` still inherit cum chars from prior turns. Then: `Σ_T (cum_attributed_tok_c(T−1) / cum_attributed_tok(T−1)) × cr_cost(T)`.

12. **`output_write`** — `Σ output_tokens × out_rate`, split across `asst_text` / `asst_thinking` / `asst_tool_use_json` by the same chars-share formula. Separate from cc/cr (a different billing stream).

13. **`fresh`** — `Σ input_tokens × in_rate`, attributed entirely to `fresh_input_uncategorized` (no further breakdown — `input_tokens` is small in well-cached sessions).

---

## Reconciliation gap interpretation

The pipeline reports `total_attributed` vs `SUM(cost_usd)`. Expected gap with current calibration: <1%, almost always slightly negative (attributed slightly under observed) due to small-payload structural overhead not captured in chars-evidence.

Larger gaps usually mean:

| Gap | Likely cause | Fix |
|-----|--------------|-----|
| +50% to +200% | Pricing-JOIN dup (multiple model_pricing rows match the model name) | Use longest-prefix CTE — see `cumulative-cost-analysis.md`. |
| −20% to −5% | Missing model in `model_pricing` (newer revision the ingester didn't price) | Add the model to `model_pricing`. The pipeline filters `model='<synthetic>'`; check for other unpriced ones. |
| −5% to −1% | Calibration drift, small-block overhead | Re-run the per-tool calibration regressions in `token-calibration.md`. |
| ≈0% | Healthy. | — |

Always sanity-check before reporting attribution numbers. A single bad join silently inflates everything downstream by an integer factor and the result still looks plausible.

---

## What to extend

- **Recalibrate per-tool factors** when a new tool dominates usage. Re-run the regressions from `token-calibration.md` with `HAVING COUNT(*) >= 50`.
- **Add a new cache-bust event** — extend `CACHE_BUST_EVENTS` priority list in `decompose_cost.py` and add a flag in `turn_events`. The two-regime logic handles the rest.
- **Subcat for a new tool** — extend the `CASE` in `iv_tool_results`. Keep `''` as the default; the rest of the pipeline handles non-empty subcats automatically.
- **Per-session output** — the `per_turn_cat_attr` table has `file_path` and is per-turn; group by the parent session via `transcripts.parent_session_id` for per-session cost dashboards.
