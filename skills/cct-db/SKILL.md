---
name: cct-db
description: Query the local DuckDB of ingested Claude Code transcripts (`~/.local/share/cct/transcripts.duckdb` by default, or `$XDG_DATA_HOME/cct/transcripts.duckdb`) to answer any question about sessions, costs, tokens, tools, models, cache hits, subagents, skills invoked, permission modes, or raw conversation data. Use this skill whenever the user wants to run SQL against that DB or asks analytical questions whose answer lives in it — "show me sessions from last week", "cost breakdown by model", "which tools did I call most", "how much on Opus yesterday", "pull the raw data", "find sessions where…", "longest sessions", "top Bash commands", "top files edited", "cache hit rate", "what skills have I used", "first-turn cache creation", "main-chain vs subagent cost", or any aggregate/filter/ranking over transcripts. Also use for **cache-miss diagnosis** — "why is my prompt cache thrashing", "which turns missed cache and why", "tools_changed cost" (`cache_miss_reason_type` / `cache_missed_input_tokens`); **per-skill / per-subagent cost** — "how much did skill X cost me", "Opus spend by subagent type" (`attribution_skill` / `attribution_agent`); and **API-error diagnostics** — "rate-limit errors", "529s by day" (`api_error_status`). Also use for **cumulative cost attribution** — "what would I save if I compacted bash outputs / trimmed file reads / shrunk CLAUDE.md", "how much is X costing me over the whole session", "is hook Y net-positive", "ROI of compressing tool outputs" — the cumulative model (cache_read paid on every later turn) and the pricing-JOIN sanity-check pattern live in `references/cumulative-cost-analysis.md`. Do NOT use for advice-shaped questions like "how do I reduce my spend" (that belongs to `optimize-usage`), for rebuilding the DB (that's the `cct` binary), or for questions about the transcripts file format itself. The DB has three critical aggregation footguns: raw `assistant_entries` overcounts cost ~2× (use `assistant_entries_deduped`); the same fan-out inflates `GROUP BY` over `cache_miss_reason_type` / `attribution_*` / `api_error_status` (`DO NOT GROUP BY raw`); naive `model_pricing` LIKE-join inflates rates 2-3× (use longest-prefix match) — this skill prevents all three. The new attribution / cache-miss / api-error / new-attachment columns only populate from ~2026-05-01 onward; historical entries have NULL.
---

# Querying the Claude transcripts DuckDB

`transcripts.duckdb` (≈2 GB) is produced by the `cct` binary (this repo's Rust ingest tool) parsing Claude Code JSONL transcripts from `~/.claude/projects/`. By default it lives at `~/.local/share/cct/transcripts.duckdb` (or `$XDG_DATA_HOME/cct/transcripts.duckdb` if that env var is set). Every assistant turn, user turn, tool call, hook event, and metadata record across all sessions lives here, typed and indexed.

Both `cct` and `duckdb` are required. If either is missing, see [Prerequisites](#prerequisites-cct-and-duckdb).

The raw JSONL line is **not** stored in the DB; every entry keeps `file_path` + `line_no` so the original line can be pulled from disk on demand (see [Accessing the original JSONL line](#accessing-the-original-jsonl-line)).

The schema is heavily self-documented via DuckDB `COMMENT ON` metadata — FK relationships, billing-safety warnings, and semantic notes. When in doubt, query the comments (see [Introspection](#introspection-when-stuck)).

---

## The billing pitfall (read first)

There is exactly one thing that will silently give you wrong numbers:

**Do not `SUM` cost or token columns on raw `assistant_entries`.** A single streaming assistant response writes one JSONL entry per content block (e.g. `thinking` + `text` + `tool_use` → 3 entries), and every one of those entries carries the same `message_id` and the same `usage` values. Summing the raw table double- (or triple-) counts those responses. Empirically this overcounts cost by ~2× on typical data.

**Use the `assistant_entries_deduped` table instead.** It's keyed on `(file_path, message_id)` and keeps the authoritative row per billing event (row with `stop_reason` set first, then largest `output_tokens`, then smallest `entry_id`). Unbilled rows (synthetic error messages from the client — `is_api_error_message = true` or `model = '<synthetic>'`) were never priced, so they have `cost_usd = NULL` and `SUM(cost_usd)` skips them automatically.

```sql
-- Total cost, correctly
SELECT ROUND(SUM(cost_usd), 2) AS total_usd
FROM assistant_entries_deduped;
```

Same rule for `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`, `cache_creation_5m`, `cache_creation_1h`. All marked `⚠ DO NOT SUM raw` in the schema comments.

**Three disjoint input buckets.** `input_tokens` (fresh), `cache_read_input_tokens`, and `cache_creation_input_tokens` are disjoint — the API bills each at its own rate. Don't add them looking for "total input cost"; `cost_usd` already combines them correctly. They're useful separately for diagnostics (cache hit rate, prefix invalidation signal, prompt-cache strategy).

**`DO NOT GROUP BY raw` — the same fan-out hits categorical columns.** Cost/token columns aren't the only ones duplicated across content blocks. `cache_miss_reason_type`, `attribution_agent`, `attribution_plugin`, `attribution_skill`, and `api_error_status` carry the same N-row repetition. A naive `GROUP BY cache_miss_reason_type, COUNT(*)` against `assistant_entries` inflates counts 2-3× the same way `SUM(cost_usd)` does. Use `assistant_entries_deduped` for any distribution, ranking, or per-category aggregation over these columns. Schema comments mark them `⚠ DO NOT GROUP BY on raw`.

---

## Forward coverage of new columns (read second)

A batch of fields was added to the JSONL transcript format on or around **2026-05-01**. Older entries do not carry them — the ingester writes NULL. Affected columns: `assistant_entries.attribution_agent`, `attribution_plugin`, `attribution_skill`, `cache_miss_reason_type`, `cache_missed_input_tokens`, `api_error_status`; `attachment_entries.deferred_readded_names`; `user_entries.image_paste_ids`, `plan_content`. New attachment variants emitted from then onward: `auto_mode`, `auto_mode_exit`, `agent_listing_delta`, `plan_file_reference`, `hook_stopped_continuation`, `hook_system_message`, `todo_reminder` (the latter three from ~2026-05-02).

Practical consequence: any **share / distribution / coverage** query that mixes pre- and post-cutover entries will look like the field is sparsely populated. Either filter to `e.timestamp >= '2026-05-01'`, or use `COUNT(*) FILTER (WHERE col IS NOT NULL)` as the denominator instead of `COUNT(*)`. **Total-cost aggregation is unaffected** (`cost_usd` was always populated); only the new categorical/diagnostic fields have this gap.

Quick activation check before relying on a new column:

```sql
SELECT MIN(e.timestamp) AS first_seen,
       COUNT(*) FILTER (WHERE d.attribution_skill IS NOT NULL) AS with_attr,
       COUNT(*)                                                AS total
FROM assistant_entries_deduped d
JOIN entries e ON e.entry_id = d.entry_id
WHERE d.message_id IS NOT NULL;
```

`cct ingest` overwrites the DB from scratch on every run, so old DBs don't auto-pick-up new columns even after a `cct` upgrade — the schema gets rewritten but the rows are re-parsed from the same JSONL on disk. To pick up newly emitted JSONL fields, the entries themselves must be from after the cutover. Use this DDL-presence sanity check before using a new column at all:

```sql
SELECT COUNT(*) FROM duckdb_columns()
WHERE table_name = 'assistant_entries' AND column_name = 'attribution_skill';
-- 0 ⇒ DB was ingested with an older `cct`; re-run `cct ingest`.
```

---

## Schema model

Three conceptual layers, plus views.

### Root tables

| Table | PK | What |
|-------|-----|------|
| `transcripts` | `file_path` | one row per `.jsonl` file (a session or a subagent run). Carries `is_subagent`, `parent_session_id`, `agent_id`, `first_timestamp`, `last_timestamp`, `entry_count`, `ingested_at`. |
| `entries` | `entry_id` (BIGINT) | one row per JSONL line — the universal join key. |
| `model_pricing` | `model` | per-model USD/Mtok rates (fresh input, output, cache 5m, cache 1h, cache read) + `effective_date`. |

`entries.file_path → transcripts(file_path)` joins entries back to their source transcript.

### Variant tables (1:1 with `entries` by `entry_id`)

Each `entries` row has a `type` (`assistant` / `user` / `system` / `attachment` / `progress` / …) and exactly one row in the matching variant table:

| `entries.type` | Variant table |
|---------------|---------------|
| `assistant` | `assistant_entries` |
| `user` | `user_entries` |
| `system` | `system_entries` |
| `attachment` | `attachment_entries` |
| `progress` | `progress_entries` |

Plus narrow metadata variants (`permission_mode_entries`, `last_prompt_entries`, `ai_title_entries`, `summary_entries`, `pr_link_entries`, `mode_entries`, `tag_entries`, `task_summary_entries`, `worktree_state_entries`, `forked_*`, `marble_origami_*`, `attribution_snapshot_entries`, `content_replacement_entries`, `file_history_snapshot_entries`, `queue_operation_entries`, …). Run `SHOW TABLES` or query `duckdb_columns()` when you need one; most analytical queries don't. The four most recent — `attribution_snapshot_entries` (per-message UI surface + file states + prompt/permission/escape counts), `content_replacement_entries` (text replacements applied to a session), `file_history_snapshot_entries` (file-state snapshots keyed by `message_id`), and `queue_operation_entries` (queue add/remove with timestamps) — are useful for UI/agent-behavior telemetry rather than cost aggregation.

Note on `last_prompt_entries`: as of Claude Code's new `last-prompt` format, `last_prompt` is NULL for rows where the transcript stored only `leaf_uuid`. Filter with `WHERE last_prompt IS NOT NULL` to restrict to old-format rows. `leaf_uuid` is a *soft* reference to `entries(uuid)` and does not point at the prompt-text entry — it is the conversation-tree leaf at session-save time. Always join on **both** `uuid` and `session_id`, because `entries(uuid)` is non-unique across resumed sessions:

```sql
SELECT lpe.session_id, e.type, e.uuid
FROM last_prompt_entries lpe
JOIN entries e
  ON e.uuid = lpe.leaf_uuid
 AND e.session_id = lpe.session_id
WHERE lpe.leaf_uuid IS NOT NULL;
```

Even with the two-column join, expect 1:N fan-out for resumed sessions where the same `(uuid, session_id)` pair is replayed.

**Always join variant tables through `entries`:**
```sql
SELECT e.timestamp, ae.model, ae.cost_usd
FROM entries e
JOIN assistant_entries_deduped ae ON ae.entry_id = e.entry_id
WHERE e.session_id = '<uuid>';
```

### Child tables (1:N with `entries` by `(entry_id, position)`)

One `entries` row can fan out to multiple child rows:

| Table | Holds |
|-------|-------|
| `user_content_blocks` | content blocks of a user message (text, tool_result, image, document) |
| `assistant_content_blocks` | content blocks of an assistant message (text, thinking, tool_use, redacted) |
| `assistant_usage_iterations` | per-iteration token decomposition for the Advisor server-tool beta (NOT billing — see [JSON columns](#json-columns-and-the-iterations-gotcha)) |
| `system_hook_infos` | hook invocations attached to a system entry (one row per hook fired) |
| `attachment_diagnostics_files` | diagnostics files referenced in an attachment entry |
| `attachment_invoked_skills` | skills invoked via an attachment entry — multiple skills can share one `entry_id` (batch-loaded at session start or on demand). `position` orders them; `invocation_metadata` is JSON and may be empty. |

### Views and derived tables

| Name | Kind | Use for |
|------|------|---------|
| `assistant_entries_deduped` | table (materialized at ingest) | billing-safe cost/token aggregation. Same columns as `assistant_entries`, one row per `(file_path, message_id)`. Indexed on `entry_id` and `message_id`. |
| `tool_uses` | view | flat tool-call rows pulled from `assistant_content_blocks` where `block_type='tool_use'`. Exposes `name`, `tool_use_id`, `input` (JSON), plus convenience columns `effective_path`, `file_ext`, `input_command`, `input_path`, `input_file_path`, `input_notebook_path`, `caller_type`. Prefer this over parsing content blocks by hand. |

`assistant_entries_deduped` was a VIEW until the dedup window function dominated dashboard latency; it is now materialized into a real table during ingest. The schema and dedup rule are unchanged, but introspection via `duckdb_views()` no longer lists it — use `duckdb_tables()` or `SHOW TABLES`.

### Schema comment notation

- `→ table(col)` — hard FK; every row has a match.
- `~ table(col)` — **soft FK; the target row may be missing.** A naive JOIN silently drops rows. Most common case: `assistant_entries.model` with a version suffix (`claude-haiku-4-5-20251001`) has no exact match in `model_pricing` (`claude-haiku-4-5`). Use a prefix-LIKE join — see [Joining against `model_pricing`](#joining-against-model_pricing).
- `⚠ DO NOT SUM raw` — use the deduped view.

---

## Common query recipes

### Total cost, total tokens
```sql
SELECT ROUND(SUM(cost_usd), 2) AS usd,
       SUM(input_tokens)                AS fresh_in,
       SUM(cache_read_input_tokens)     AS cache_read,
       SUM(cache_creation_input_tokens) AS cache_create,
       SUM(output_tokens)               AS out_tok
FROM assistant_entries_deduped;
```

### Top sessions by cost
```sql
SELECT e.session_id,
       MIN(e.timestamp)          AS started_at,
       ROUND(SUM(d.cost_usd), 2) AS cost_usd
FROM assistant_entries_deduped d
JOIN entries e ON e.entry_id = d.entry_id
GROUP BY 1 ORDER BY cost_usd DESC LIMIT 20;
```

### Cost by model
```sql
SELECT model,
       COUNT(*)                AS n_responses,
       ROUND(SUM(cost_usd), 2) AS cost_usd,
       SUM(input_tokens)       AS fresh_in,
       SUM(output_tokens)      AS out_tok
FROM assistant_entries_deduped
GROUP BY 1 ORDER BY cost_usd DESC;
```

### Cost per day
```sql
SELECT DATE_TRUNC('day', e.timestamp) AS day,
       ROUND(SUM(d.cost_usd), 2)     AS cost_usd
FROM assistant_entries_deduped d
JOIN entries e ON e.entry_id = d.entry_id
GROUP BY 1 ORDER BY 1;
```

### Cost per project (by `cwd`)
`cwd` appears on user/system entries and many assistant entries, but is sparse on some narrow-variant types. The robust attribution is "session's modal cwd", which tolerates NULLs:

```sql
WITH session_cwd AS (
  SELECT session_id, MODE(cwd) AS cwd
  FROM entries WHERE cwd IS NOT NULL
  GROUP BY 1)
SELECT s.cwd,
       ROUND(SUM(d.cost_usd), 2) AS cost_usd
FROM assistant_entries_deduped d
JOIN entries e     ON e.entry_id  = d.entry_id
JOIN session_cwd s ON s.session_id = e.session_id
GROUP BY 1 ORDER BY cost_usd DESC;
```

### Cache hit rate per model
Denominator is fresh input + cache read (the two "was this token already in cache?" outcomes). `cache_creation_input_tokens` is excluded because it's not a hit/miss event — it's the write that populates the cache for future reads, billed at its own rate.

```sql
SELECT model,
       SUM(cache_read_input_tokens)    AS cache_read,
       SUM(input_tokens)               AS fresh_input,
       ROUND(100.0 * SUM(cache_read_input_tokens) /
             NULLIF(SUM(cache_read_input_tokens) + SUM(input_tokens), 0), 1) AS pct_cached
FROM assistant_entries_deduped
GROUP BY 1;
```

### Tool usage frequency
```sql
SELECT name, COUNT(*) AS n
FROM tool_uses
GROUP BY 1 ORDER BY n DESC LIMIT 30;
```

### Bash commands run today
```sql
SELECT e.timestamp, t.input_command
FROM tool_uses t
JOIN entries e ON e.entry_id = t.entry_id
WHERE t.name = 'Bash'
  AND DATE_TRUNC('day', e.timestamp) = CURRENT_DATE
ORDER BY e.timestamp;
```

### Files most often edited
```sql
SELECT effective_path, COUNT(*) AS n
FROM tool_uses
WHERE name IN ('Edit','Write','NotebookEdit')
  AND effective_path IS NOT NULL
GROUP BY 1 ORDER BY n DESC LIMIT 30;
```

### Session duration and turn count
```sql
SELECT session_id,
       COUNT(*)                                             AS entries,
       DATE_DIFF('minute', MIN(timestamp), MAX(timestamp))  AS minutes
FROM entries
WHERE session_id IS NOT NULL
GROUP BY 1 ORDER BY minutes DESC LIMIT 20;
```

### Skills invoked (frequency)
```sql
SELECT skill_name, COUNT(*) AS n
FROM attachment_invoked_skills
GROUP BY 1 ORDER BY n DESC;
```

### Cost attributed to a skill (post-2026-05-01)

Claude Code now stamps each assistant turn with the skill active in its system block, via `assistant_entries.attribution_skill` (format: `<plugin>:<skill>` for plugin-namespaced skills, bare `<skill>` for built-ins; NULL when no skill is active). This replaces the timestamp-window heuristic for entries from the cutover onward — it's exact, per-turn, and survives concurrent skills.

```sql
SELECT attribution_skill,
       COUNT(*)                AS turns,
       ROUND(SUM(cost_usd), 2) AS cost_usd
FROM assistant_entries_deduped
WHERE attribution_skill IS NOT NULL
GROUP BY 1 ORDER BY cost_usd DESC;
```

What this measures vs what it doesn't:
- **Measures:** time the skill was *active* in the system block (loaded into context, available for the model to use). Each turn cleanly credited to one skill.
- **Doesn't measure:** the cost of *loading* the skill — the `dynamic_skill` attachment that injects it, plus the `cache_bust:dynamic_skill` re-cache it triggers on the next turn. For skill-load ROI ("did pulling this skill into context pay for itself?"), see the cumulative-attribution methodology in `references/cumulative-cost-analysis.md`.
- **Pre-cutover entries are NULL** for this column. Filter to `WHERE e.timestamp >= '2026-05-01' AND attribution_skill IS NOT NULL` for a clean denominator if you're computing percentages.

For the older corpus, the post-load timestamp-window approximation still works as a ceiling — keep both interpretations in mind:

```sql
WITH loads AS (
  SELECT ais.skill_name,
         e.session_id,
         MIN(e.timestamp) AS loaded_at
  FROM attachment_invoked_skills ais
  JOIN entries e ON e.entry_id = ais.entry_id
  GROUP BY 1, 2)
SELECT l.skill_name,
       COUNT(DISTINCT l.session_id)     AS sessions,
       ROUND(SUM(d.cost_usd), 2)        AS attributed_usd
FROM loads l
JOIN entries e              ON e.session_id = l.session_id AND e.timestamp >= l.loaded_at
JOIN assistant_entries_deduped d ON d.entry_id = e.entry_id
GROUP BY 1 ORDER BY attributed_usd DESC;
```

Two interpretations of the timestamp-window query: (1) invocation count is a cleaner signal of "am I using this"; (2) attributed cost is an upper bound — when two skills load at the same attachment entry (common for batch loads), they share an `entry_id` and the post-load window is credited to both.

### Cost attributed to a subagent (post-2026-05-01)

`assistant_entries.attribution_agent` (format: `<plugin>:<agent>` or bare `<agent>`) and `attribution_plugin` mark turns produced by a subagent. This is a per-turn alternative to the `transcripts.is_subagent` + `parent_session_id` join chain — useful when you want a flat per-agent breakdown without unrolling the subagent-file structure:

```sql
SELECT attribution_agent,
       attribution_plugin,
       COUNT(*)                AS turns,
       ROUND(SUM(cost_usd), 2) AS cost_usd
FROM assistant_entries_deduped
WHERE attribution_agent IS NOT NULL
GROUP BY 1, 2 ORDER BY cost_usd DESC;
```

When `attribution_plugin` and the `<plugin>:` prefix of `attribution_agent` disagree, the schema comment is explicit: trust `attribution_plugin`. Pre-cutover subagent transcripts have NULL here — fall back to the `is_subagent`-based recipe in [Subagent transcripts](#subagent-transcripts-separate-files) for full historical coverage.

### Why did the prompt cache miss? (`cache_miss_reason_type`)

When the API skips the prompt cache and rebills input tokens at the fresh-input rate, it now reports the reason on the assistant turn. `assistant_entries.cache_miss_reason_type` is the API's own ground-truth signal — values seen in the wild: `tools_changed`, `messages_changed`, `system_changed`, `params_changed`, `model_changed`, `previous_message_not_found`, `unavailable`. `cache_missed_input_tokens` is the exact token count that paid full price (NULL when reason is `previous_message_not_found` or `unavailable`).

This is the highest-leverage cost-diagnostic field added by the schema update — it tells you *why* the cache thrashed, by category, in dollars:

```sql
SELECT cache_miss_reason_type,
       COUNT(*)                                            AS turns,
       ROUND(SUM(cache_missed_input_tokens) / 1e6, 2)      AS missed_mtok,
       ROUND(SUM(cost_usd), 2)                             AS turn_cost_usd
FROM assistant_entries_deduped
WHERE message_id IS NOT NULL
  AND cache_miss_reason_type IS NOT NULL
GROUP BY 1 ORDER BY turn_cost_usd DESC NULLS LAST;
```

Cross-tab against the attachment-event detector in [Cache-bust events](#cache-bust-events) to attribute *which* event drove a `tools_changed` or `system_changed` miss. The two fields answer different questions — `cache_miss_reason_type` says "did this turn miss, and what kind"; the attachment-event row pair says "which payload changed".

`turn_cost_usd` here is the **whole turn**, not just the miss portion — a `tools_changed` turn still does its normal output and may have other input. To isolate the miss-only dollar amount, weight `cache_missed_input_tokens` by the model's fresh-input rate (longest-prefix join — see [`references/cumulative-cost-analysis.md`](references/cumulative-cost-analysis.md)):

```sql
WITH r AS (
  SELECT d.cache_miss_reason_type,
         d.cache_missed_input_tokens,
         (SELECT input_per_mtok FROM model_pricing p
          WHERE d.model LIKE p.model || '%'
          ORDER BY LENGTH(p.model) DESC LIMIT 1) AS rate
  FROM assistant_entries_deduped d
  WHERE d.cache_miss_reason_type IS NOT NULL
    AND d.cache_missed_input_tokens IS NOT NULL)
SELECT cache_miss_reason_type,
       ROUND(SUM(cache_missed_input_tokens * rate / 1e6), 2) AS miss_only_usd
FROM r GROUP BY 1 ORDER BY miss_only_usd DESC;
```

### API errors by status code

`assistant_entries.api_error_status` (SMALLINT) is the HTTP status the API returned on a failed turn — paired with `is_api_error_message = true` and `error` (the message text). Common values: `429` (rate limit), `401` (auth), `400` (invalid request), `403`, `529` (overload). Useful for sniffing rate-limit pressure or mid-session auth flaps:

```sql
SELECT api_error_status,
       COUNT(*) AS errors,
       ROUND(100.0 * COUNT(*) / SUM(COUNT(*)) OVER (), 1) AS pct
FROM assistant_entries_deduped
WHERE api_error_status IS NOT NULL
GROUP BY 1 ORDER BY errors DESC;
```

Failed turns have `cost_usd = NULL` (never billed) so they're already excluded from cost sums — this view is purely diagnostic.

### Permission-mode usage
```sql
SELECT permission_mode, COUNT(*) AS n
FROM permission_mode_entries
GROUP BY 1 ORDER BY n DESC;
```

### Preceding user message for an assistant turn
Useful when tracing why a turn exploded. `parent_uuid` on an assistant entry is polymorphic — can point at `user`, `assistant`, `attachment`, or `progress`. For the immediately preceding user entry (human prompt or tool result), walk back through `entries` filtered to `type='user'` and ordered by `entry_id` descending. Pass `<asst_entry_id>` as the target.

```sql
WITH target AS (
  SELECT session_id, entry_id AS asst_id
  FROM entries WHERE entry_id = <asst_entry_id>)
SELECT e.entry_id, e.timestamp, e.is_sidechain,
       u.message_content_text,
       LENGTH(u.message_content_text) AS text_chars,
       LENGTH(CAST(u.tool_use_result AS VARCHAR)) AS tool_result_chars
FROM target t
JOIN entries e       ON e.session_id = t.session_id
                    AND e.entry_id < t.asst_id
                    AND e.type = 'user'
JOIN user_entries u  ON u.entry_id = e.entry_id
ORDER BY e.entry_id DESC LIMIT 1;
```

Two flavors of user entry share this table: **plain-text prompts** populate `message_content_text` (tool_use_result NULL); **tool-result injections** populate `tool_use_result` (message_content_text NULL). Both count as "the preceding user turn" for cost-trace purposes — a huge tool result often explains a blown-up assistant turn more than the last human sentence. If you specifically want the last *human* prompt, filter `WHERE u.message_content_text IS NOT NULL AND NOT e.is_sidechain` and know you may skip several tool-result turns to get there.

Adjacent `user_entries` columns worth knowing:
- `is_visible_in_transcript_only = true` marks messages the model never received — they exist for UI scrollback only. Exclude from cost or content sums; otherwise you're crediting bytes that were never billed.
- `is_compact_summary = true` is the auto-generated summary that replaces older turns at compaction. It lives in `message_content_text` and is huge — its turn pair is also a `compact_summary` cache-bust event.
- `image_paste_ids` (JSON array) records images attached to a prompt by paste id; `plan_content` carries plan-mode text submitted with the message. Both are ~2026-05-01-onward (see the coverage caveat).

### Most expensive individual turns
Top-N for spotting runaway single turns (huge tool-result payloads, oversized Agent prompts, cold-cache spawns):

```sql
SELECT e.timestamp, e.session_id, d.model,
       d.entry_id, d.cost_usd,
       d.cache_creation_input_tokens AS cc_tok,
       d.cache_read_input_tokens     AS cr_tok,
       d.output_tokens               AS out_tok
FROM assistant_entries_deduped d
JOIN entries e ON e.entry_id = d.entry_id
ORDER BY d.cost_usd DESC NULLS LAST LIMIT 20;
```

For top-1% (looking at the tail rather than a fixed count), swap the `ORDER BY … LIMIT` for `PERCENT_RANK() OVER (ORDER BY cost_usd)` filtered `>= 0.99`.

### First-turn cache creation per session (system-prompt sniff)
The first assistant turn of a fresh main session pays cache-creation on the whole system prompt (CLAUDE.md + MCP schemas + tool list + hooks). Distribution across sessions fingerprints how heavy the system prompt is. Use `entry_id` (not `timestamp`) to pick the first turn — timestamps can tie. Filter subagent transcripts, which have their own cold-start and aren't "fresh sessions" in the user sense.

```sql
WITH first_turn AS (
  SELECT e.session_id, MIN(e.entry_id) AS entry_id
  FROM entries e
  JOIN transcripts t ON t.file_path = e.file_path
  WHERE e.type = 'assistant' AND NOT t.is_subagent
  GROUP BY 1)
SELECT f.session_id,
       d.model,
       d.cache_creation_input_tokens  AS cc_tokens,
       ROUND(d.cost_usd, 4)           AS cost_usd
FROM first_turn f
JOIN assistant_entries_deduped d ON d.entry_id = f.entry_id
WHERE d.model != '<synthetic>'
ORDER BY cc_tokens DESC NULLS LAST LIMIT 30;
```

For the distribution, wrap the same CTE and aggregate (keep the `model != '<synthetic>'` filter to keep zero-token client errors out of the percentiles):

```sql
-- ... same first_turn CTE ...
SELECT d.model,
       COUNT(*)                                            AS sessions,
       APPROX_QUANTILE(d.cache_creation_input_tokens, 0.5) AS p50,
       APPROX_QUANTILE(d.cache_creation_input_tokens, 0.9) AS p90,
       MAX(d.cache_creation_input_tokens)                  AS max_cc
FROM first_turn f
JOIN assistant_entries_deduped d ON d.entry_id = f.entry_id
WHERE d.model != '<synthetic>'
GROUP BY 1 ORDER BY sessions DESC;
```

### Main-chain vs sidechain cost split
In this schema, `entries.is_sidechain = true` marks entries that belong to a subagent/sub-task branch; subagent-file entries are fully sidechain, main-session entries are not. This lets you split cost without joining through `transcripts`:

```sql
SELECT e.is_sidechain,
       ROUND(SUM(d.cost_usd), 2) AS cost_usd,
       SUM(d.input_tokens)                AS fresh_in,
       SUM(d.cache_read_input_tokens)     AS cache_read,
       SUM(d.cache_creation_input_tokens) AS cache_create,
       SUM(d.output_tokens)               AS out_tok
FROM assistant_entries_deduped d
JOIN entries e ON e.entry_id = d.entry_id
GROUP BY 1;
```

### Hook volume and time
`system_hook_infos` is narrow: `entry_id`, `position`, `command`, `duration_ms`. It records *that* a hook fired and how long it took — not its output. Hook-injected content that lands in the conversation shows up under `attachment_entries` (look for `hook_stdout` / `hook_content` / `hook_command` columns there).

```sql
SELECT command,
       COUNT(*)                                 AS n,
       ROUND(SUM(duration_ms)/1000.0, 1)        AS total_seconds,
       ROUND(AVG(duration_ms), 0)               AS avg_ms,
       MAX(duration_ms)                         AS max_ms
FROM system_hook_infos
GROUP BY 1 ORDER BY n DESC;
```

For content-injection volume, join to `attachment_entries` where `hook_content IS NOT NULL` (or inspect `hook_stdout`):

```sql
SELECT hook_command,
       COUNT(*)                       AS n,
       AVG(LENGTH(hook_content))      AS avg_content_chars,
       AVG(LENGTH(hook_stdout))       AS avg_stdout_chars
FROM attachment_entries
WHERE hook_command IS NOT NULL
GROUP BY 1 ORDER BY n DESC;
```

### Hour-of-day distribution (autonomous-loop sniff)
Regularly-spaced off-hours spawns are a tell for cron/loop setups. Timestamps are stored as UTC — convert to the user's local time before interpreting. Filter subagents out, otherwise a long daytime session spawning subagents at 2am looks like a 2am loop.

```sql
SELECT EXTRACT(hour FROM e.timestamp) AS utc_hour,
       COUNT(*)                      AS turns,
       ROUND(SUM(d.cost_usd), 2)     AS cost_usd
FROM assistant_entries_deduped d
JOIN entries e     ON e.entry_id = d.entry_id
JOIN transcripts t ON t.file_path = e.file_path
WHERE NOT t.is_subagent
GROUP BY 1 ORDER BY 1;
```

For an explicit scheduled-agent signal, look for `system_entries.subtype = 'scheduled_task_fire'` — the `schedule` skill emits one system entry per fire:

```sql
SELECT e.timestamp, e.session_id, se.content
FROM system_entries se
JOIN entries e ON e.entry_id = se.entry_id
WHERE se.subtype = 'scheduled_task_fire'
ORDER BY e.timestamp;
```

For fixed-interval (cron-like) detection, bucket `transcripts.first_timestamp` by `(hour, minute)` rounded to a tolerance, and require the same bucket to recur across several distinct days:

```sql
SELECT EXTRACT(hour FROM first_timestamp)       AS hr,
       (EXTRACT(minute FROM first_timestamp)::INT / 5) * 5 AS min_bucket,
       COUNT(DISTINCT CAST(first_timestamp AS DATE)) AS distinct_days,
       COUNT(*)                                  AS n_sessions
FROM transcripts
WHERE NOT is_subagent AND first_timestamp IS NOT NULL
GROUP BY 1, 2
HAVING COUNT(DISTINCT CAST(first_timestamp AS DATE)) >= 4
ORDER BY distinct_days DESC, n_sessions DESC;
```

---

## Subagents, sidechains, and forks

### Subagent transcripts (separate files)

**Subagent costs are NOT included when you group main-session entries by `session_id`.** They live in separate `.jsonl` files under `<session>/subagents/agent-<id>.jsonl` and ingest treats each as its own transcript (`transcripts.is_subagent = true`, `parent_session_id → main session`, `agent_id → unique run id`). Forgetting this is the most common mis-attribution in cost analysis.

Attribute subagent cost to the parent session:

```sql
WITH sub AS (
  SELECT t.parent_session_id AS session_id,
         SUM(d.cost_usd)     AS subagent_cost
  FROM transcripts t
  JOIN entries e                    ON e.file_path = t.file_path
  JOIN assistant_entries_deduped d  ON d.entry_id  = e.entry_id
  WHERE t.is_subagent
  GROUP BY 1),
main AS (
  SELECT t.session_id,
         SUM(d.cost_usd) AS main_cost
  FROM transcripts t
  JOIN entries e                    ON e.file_path = t.file_path
  JOIN assistant_entries_deduped d  ON d.entry_id  = e.entry_id
  WHERE NOT t.is_subagent
  GROUP BY 1)
SELECT main.session_id,
       main.main_cost,
       COALESCE(sub.subagent_cost, 0)              AS subagent_cost,
       main.main_cost + COALESCE(sub.subagent_cost, 0) AS total_cost
FROM main LEFT JOIN sub USING (session_id)
ORDER BY total_cost DESC LIMIT 20;
```

### `is_sidechain` (entries) vs `is_subagent` (transcripts)

These describe the same phenomenon from two different angles on current Claude Code data:

- `transcripts.is_subagent` — this transcript is a subagent run (separate file).
- `entries.is_sidechain` — this entry belongs to a sidechain branch. In practice, every entry inside a subagent transcript is flagged sidechain; entries in main-session transcripts are not. Treat `is_sidechain` as the entry-level shortcut when you don't want to join `transcripts`.

If you ever see `is_sidechain = true` on an entry whose transcript is `is_subagent = false`, that's a speculative/branched turn within a main session — rare, include it in cost totals (it was billed).

### Forks and compaction

- `entries.forked_from_uuid` / `forked_from_session_id` — explicit session resume/fork (user picked up from a checkpoint). Every row in the forked session carries these.
- `entries.logical_parent_uuid` — preserves the logical parent across a context-compaction boundary, when `parent_uuid` breaks. Typically populated only at the boundary entry.
- `summary_entries` — one row per compaction event (the auto-generated summary that replaces older turns). Joining by `session_id` yields compaction points; absent if no session in the DB has compacted.

Turn-by-turn thread traversal that survives compaction: follow `logical_parent_uuid` when present, else `parent_uuid`.

---

## Cost decomposition

When you need to attribute every billed dollar to a source category — `tool_result:Bash`, `attachment:hook_success`, `asst_thinking`, `system_block_first_turn`, `cache_bust:compact_summary` — the full methodology and reference SQL live in [`references/cost-decomposition-methodology.md`](references/cost-decomposition-methodology.md) (built on top of [`references/cumulative-cost-analysis.md`](references/cumulative-cost-analysis.md) and [`references/token-calibration.md`](references/token-calibration.md)). The implementation is [`scripts/decompose_cost.py`](scripts/decompose_cost.py); [`scripts/flamegraph.py`](scripts/flamegraph.py) renders it as an interactive d3 flamegraph. The patterns below are the building blocks every cost-attribution query needs.

### User text storage is bimodal

Plain-text user content is split across **two disjoint locations** — UNION both, or you'll silently undercount user-text contribution:

- `user_content_blocks.text` — text blocks within structured user messages (mixed text + tool_result entries; ~12M chars in a typical 30d window)
- `user_entries.message_content_text` — plain-text prompts AND compact summaries (`is_compact_summary = true`; ~25M chars)

Verified zero overlap. Older queries that hit only the first source missed ~67% of user text. Compact summaries hide in `message_content_text` and are huge — entire prior sessions collapsed into a single message, which trigger the `compact_summary` cache-bust event below.

```sql
-- Total user text across both sources
SELECT SUM(chars) AS total_user_chars
FROM (
  SELECT LENGTH(text) AS chars
  FROM user_content_blocks
  WHERE block_type = 'text' AND text IS NOT NULL
  UNION ALL
  SELECT LENGTH(message_content_text)
  FROM user_entries
  WHERE message_content_text IS NOT NULL
);
```

### Attachment payload spans many fields

`attachment_entries` is wide. The actual injected text spreads across **12+ nullable columns**, varying by `attachment_type`. Naive `LENGTH(hook_content)` misses ~50% of attachment cost. For total chars per row, sum every populated content field:

```sql
SELECT attachment_type,
       COUNT(*) AS n,
       SUM(
           COALESCE(LENGTH(hook_content), 0)
         + COALESCE(LENGTH(hook_stdout), 0)
         + COALESCE(LENGTH(hook_stderr), 0)
         + COALESCE(LENGTH(hook_command), 0)
         + COALESCE(LENGTH(file_content_text), 0)
         + COALESCE(LENGTH(directory_content), 0)
         + COALESCE(LENGTH(skill_listing_content), 0)
         + COALESCE(LENGTH(CAST(task_reminder_content AS VARCHAR)), 0)
         + COALESCE(LENGTH(nested_memory_content), 0)
         + COALESCE(LENGTH(queued_command_prompt), 0)
         + COALESCE(LENGTH(CAST(diagnostics_files AS VARCHAR)), 0)
         + COALESCE(LENGTH(CAST(invoked_skills AS VARCHAR)), 0)
         + COALESCE(LENGTH(CAST(deferred_added_lines AS VARCHAR)), 0)
         + COALESCE(LENGTH(CAST(deferred_added_names AS VARCHAR)), 0)
         + COALESCE(LENGTH(CAST(deferred_removed_names AS VARCHAR)), 0)
         + COALESCE(LENGTH(CAST(deferred_readded_names AS VARCHAR)), 0)
         + COALESCE(LENGTH(CAST(mcp_added_blocks AS VARCHAR)), 0)
         + COALESCE(LENGTH(CAST(mcp_added_names AS VARCHAR)), 0)
         + COALESCE(LENGTH(CAST(mcp_removed_names AS VARCHAR)), 0)
         + COALESCE(LENGTH(CAST(command_allowed_tools AS VARCHAR)), 0)
         + COALESCE(LENGTH(CAST(skill_names AS VARCHAR)), 0)
       ) AS total_chars
FROM attachment_entries
GROUP BY 1 ORDER BY total_chars DESC;
```

Fields most often missed: `deferred_added_lines` (ToolSearch loaded-tool deltas, millions of chars), `mcp_added_blocks` (MCP-instructions deltas), `hook_command` / `hook_stdout` (hook event content), `file_content_text` / `directory_content` (file/dir reads via `@path` mentions or attached files). Per-type chars/tok calibration in [`references/token-calibration.md`](references/token-calibration.md).

The list above is brittle — every new attachment variant adds at least one column. Before relying on the sum for cost decomposition, audit it against the live schema:

```sql
SELECT column_name, data_type
FROM duckdb_columns()
WHERE table_name = 'attachment_entries'
  AND data_type IN ('VARCHAR', 'JSON')
  AND column_name NOT IN ('attachment_type','hook_event','attribution_skill') -- categoricals
ORDER BY 1;
```

Anything new in the result that isn't in the COALESCE sum above is dropped chars. As of 2026-05-05 the list also includes `deferred_readded_names` (added) and `plan_file_path` (path, not content — leave out of the sum; the body lives in `file_content_text` because `PlanFileReference` flattens into the file-shaped columns). New attachment types since 2026-05-01 — `auto_mode`, `auto_mode_exit`, `agent_listing_delta`, `hook_stopped_continuation`, `hook_system_message`, `todo_reminder` — currently classify-only (no dedicated content columns), so the existing payload sum already covers them via `hook_content` / `hook_stdout` where applicable. Recheck if any of those grow dedicated columns later.

### Tool-result subcat drill-down

To answer "which Bash command", "which file extension", "which subagent type" rather than just "Bash" or "Read":

```sql
SELECT t.name AS tool,
       COALESCE(
         CASE
           WHEN t.name = 'Bash' AND t.input_command IS NOT NULL
             -- strip leading whitespace; strip leading FOO=val env-var assignments;
             -- take first non-whitespace token; drop directory prefix; cap length.
             THEN SUBSTR(
                    REGEXP_REPLACE(
                      REGEXP_EXTRACT(
                        REGEXP_REPLACE(
                          REGEXP_REPLACE(t.input_command, '^\s+', ''),
                          '^([A-Za-z_][A-Za-z0-9_]*=\S*\s+)+', ''),
                        '^\S+', 0),
                      '^.*/', ''),
                    1, 40)
           WHEN t.name IN ('Read','Edit','Write','NotebookEdit')
                AND t.file_ext IS NOT NULL
             -- file_ext extracts everything after the first dot, so dotfile paths
             -- like `.git/hooks/pre-commit` come back with slashes; trim.
             THEN SUBSTR(REGEXP_REPLACE(t.file_ext, '^.*/', ''), 1, 30)
           WHEN t.name = 'Agent'
             THEN JSON_EXTRACT_STRING(t.input, '$.subagent_type')
           WHEN t.name = 'WebFetch'
             THEN REGEXP_EXTRACT(JSON_EXTRACT_STRING(t.input, '$.url'), '://([^/]+)', 1)
           ELSE NULL
         END,
         '') AS subcat,
       COUNT(*) AS n
FROM tool_uses t
GROUP BY 1, 2 ORDER BY n DESC LIMIT 30;
```

Trade-offs to know about: (1) Bash compound chains like `cd /x && git status` collapse to `cd` — first-token only is a deliberate choice for flamegraph clarity over completeness; layer a "skip leading `cd` / `source`" pass if it pollutes results. (2) Use `''` (empty string) as the default, not `NULL`, so PARTITION BY / USING joins downstream don't drop rows under SQL NULL semantics. (3) `mcp__*` tool names are already specific — leave subcat empty.

### Cache-bust events

A set of events deterministically invalidate the cached prefix and force re-injection of the system block (CLAUDE.md + tool definitions + MCP schemas + Claude Code preamble — none of which appears as a JSONL `content_block`). They show up as `cc(T+1)` spikes far above the typical `user_intervening = cc(T+1) − output_tokens(T)` token count. Knowing which event fired makes the spike *interpretable* rather than noise.

**Ground truth, post-2026-05-01:** `assistant_entries.cache_miss_reason_type` is the API's own report of *whether and why* a turn missed cache (see [Why did the prompt cache miss?](#why-did-the-prompt-cache-miss-cache_miss_reason_type)). Use it as the trigger ("did T miss?") and the attachment-event detector below as the attribution ("which payload changed in the user-side window before T?"). Pre-cutover entries have only the attachment-event signal — the table below is still the only available detector for that corpus.

Known events, in priority order (first match wins when multiple fire on the same turn):

| Event | Detected by | What changes |
|-------|-------------|--------------|
| `compact_summary` | `user_entries.is_compact_summary = true` | Prior turns collapsed into a summary; full re-cache. |
| `deferred_tools_delta` | `attachment_type = 'deferred_tools_delta'` | ToolSearch loaded a deferred tool; system-block tool list grew. (Schema also exposes `deferred_readded_names` — tools previously dropped that came back in the same delta — useful for spotting churn.) |
| `agent_listing_delta` | `attachment_type = 'agent_listing_delta'` | Available-agent listing payload changed (analogous to `deferred_tools_delta` but for agents). Post-2026-05-01. |
| `mcp_instructions_delta` | `attachment_type = 'mcp_instructions_delta'` | MCP server reloaded instructions. |
| `dynamic_skill` | `attachment_type = 'dynamic_skill'` | Plugin/skill content injected into system block. |
| `auto_mode` / `auto_mode_exit` | `attachment_type IN ('auto_mode', 'auto_mode_exit')` | AutoMode reminder added to / removed from the system block. Post-2026-05-01. |
| `plan_mode` / `plan_mode_exit` | `attachment_type IN ('plan_mode', 'plan_mode_exit')` | Plan-mode reminder added to / removed from the system block. |
| `date_change` | `attachment_type = 'date_change'` | Day rolled over; `currentDate` in system block updated. |

The list is empirical — there's no enumerated registry of cache-bust events in the source. New events ship with new Claude Code versions; treat unexplained `cache_miss_reason_type = system_changed` / `tools_changed` spikes that this list doesn't account for as evidence of an unmodelled event and check `attachment_type` distinct values around the spike.

Detection per turn pair — find which events fired in the user-side window between the previous and current assistant turn:

```sql
WITH turn_pairs AS (
  SELECT e.file_path, e.entry_id AS aT_eid,
         LAG(e.entry_id) OVER (PARTITION BY e.file_path ORDER BY e.entry_id) AS prev_eid
  FROM entries e
  JOIN assistant_entries_deduped d ON d.entry_id = e.entry_id
  WHERE d.model != '<synthetic>')
SELECT tp.file_path, tp.aT_eid,
       BOOL_OR(uet.is_compact_summary)                          AS has_compact,
       BOOL_OR(att.attachment_type = 'deferred_tools_delta')    AS has_dtd,
       BOOL_OR(att.attachment_type = 'mcp_instructions_delta')  AS has_mcp_delta,
       BOOL_OR(att.attachment_type = 'dynamic_skill')           AS has_dyn_skill,
       BOOL_OR(att.attachment_type = 'date_change')             AS has_date_change
FROM turn_pairs tp
LEFT JOIN entries ue ON ue.file_path = tp.file_path
                    AND ue.entry_id BETWEEN tp.prev_eid + 1 AND tp.aT_eid - 1
                    AND ue.type = 'user'
LEFT JOIN user_entries uet ON uet.entry_id = ue.entry_id
LEFT JOIN entries ae ON ae.file_path = tp.file_path
                    AND ae.entry_id BETWEEN tp.prev_eid + 1 AND tp.aT_eid - 1
                    AND ae.type = 'attachment'
LEFT JOIN attachment_entries att ON att.entry_id = ae.entry_id
WHERE tp.prev_eid IS NOT NULL
GROUP BY 1, 2;
```

The cost of an event on a turn pair is `cc(T+1) − prev_out_tok − Σ chars-evidence`: the system-block re-injection minus what visible content blocks account for. The decomposition pipeline rolls these into a `cache_bust:<event>` category — see [`references/cost-decomposition-methodology.md`](references/cost-decomposition-methodology.md) for the two-regime attribution that handles event vs non-event turns differently.

---

## Joining against `model_pricing`

`model_pricing.model` is a short name (e.g. `claude-haiku-4-5`) but `assistant_entries.model` is often a dated revision (`claude-haiku-4-5-20251001`). A naive `JOIN ON d.model = p.model` silently drops every revisioned row. Prefix-match instead — but be aware of the dup trap below.

> **Footgun.** A simple `LIKE p.model || '%'` join matches *multiple* pricing rows when the family has both versioned and unversioned entries (`claude-haiku-4-5-20251001` matches `claude-haiku` *and* `claude-haiku-4-5`). Every cost computed downstream gets inflated 2-3× and still looks plausible. For any non-trivial cost analysis use a longest-prefix match and a recompute-vs-`SUM(cost_usd)` sanity check — see [`references/cumulative-cost-analysis.md`](references/cumulative-cost-analysis.md) for the safe pattern.

The query below is safe specifically because both branches of the delta cancel the row-fan-out: actual `SUM(cost_usd)` and the recomputed "no cache" total are both N-times-counted by the multi-match bug, so the *ratio* is correct even when the *absolute totals* are inflated. **Do not copy this LIKE-join into a query that reports absolute dollars** — it will read 2-3× high. For absolute numbers always use the longest-prefix correlated-subquery pattern from `cumulative-cost-analysis.md` — that is what the cache-creation 5m-vs-1h split query later in this section uses.

```sql
-- Actual vs "no caching ever existed": apply fresh-input rate to all three input buckets;
-- output cost is identical in both scenarios so it cancels out of the delta (but include
-- it on both sides if you want absolute totals).
WITH rated AS (
  SELECT d.model                         AS db_model,
         p.model                         AS pricing_model,
         p.input_per_mtok, p.output_per_mtok,
         d.input_tokens, d.cache_read_input_tokens, d.cache_creation_input_tokens,
         d.output_tokens, d.cost_usd
  FROM assistant_entries_deduped d
  LEFT JOIN model_pricing p ON d.model LIKE p.model || '%')
SELECT db_model,
       ROUND(SUM(cost_usd), 2) AS actual_usd,
       ROUND(SUM(
         (input_tokens + cache_read_input_tokens + cache_creation_input_tokens) * input_per_mtok / 1e6
         + output_tokens * output_per_mtok / 1e6
       ), 2) AS no_cache_usd
FROM rated
GROUP BY 1 ORDER BY actual_usd DESC NULLS LAST;
```

Models absent from `model_pricing` (e.g. older revisions not loaded) show `NULL` on the `no_cache_usd` side. The ingester may also have skipped pricing them, in which case `actual_usd` is NULL too — surface those rows explicitly rather than silently dropping them.

`model_pricing` has two cache-creation rate columns: `cache_creation_5m_per_mtok` (default, cheaper) and `cache_creation_1h_per_mtok` (opt-in, longer TTL, pricier). If you want the actual cache-creation cost broken out, bill `cache_creation_5m` tokens at the 5m rate and `cache_creation_1h` tokens at the 1h rate — they're disjoint sub-buckets of `cache_creation_input_tokens`.

```sql
-- Cache-creation cost split: 5m vs 1h
WITH model_rates AS (
  SELECT m AS asst_model,
         (SELECT cache_creation_5m_per_mtok FROM model_pricing p
          WHERE m LIKE p.model || '%' ORDER BY LENGTH(p.model) DESC LIMIT 1) AS cc5m_rate,
         (SELECT cache_creation_1h_per_mtok FROM model_pricing p
          WHERE m LIKE p.model || '%' ORDER BY LENGTH(p.model) DESC LIMIT 1) AS cc1h_rate
  FROM (SELECT DISTINCT model AS m FROM assistant_entries_deduped
        WHERE model IS NOT NULL AND model != '<synthetic>'))
SELECT d.model,
       SUM(d.cache_creation_5m)                                       AS tok_5m,
       SUM(d.cache_creation_1h)                                       AS tok_1h,
       ROUND(SUM(d.cache_creation_5m * mr.cc5m_rate / 1e6), 2)        AS cc5m_usd,
       ROUND(SUM(d.cache_creation_1h * mr.cc1h_rate / 1e6), 2)        AS cc1h_usd,
       ROUND(SUM((d.cache_creation_5m * mr.cc5m_rate
                + d.cache_creation_1h * mr.cc1h_rate) / 1e6), 2)      AS cc_usd
FROM assistant_entries_deduped d
JOIN model_rates mr ON mr.asst_model = d.model
GROUP BY 1 ORDER BY cc_usd DESC NULLS LAST;
```

The 1h rate is roughly 2× the 5m rate (model-dependent), so a session that opts into 1h cache to amortize over many turns can shift the cc breakdown materially. Rolling cc into a single rate hides this.

---

## JSON columns and the `iterations` gotcha

Polymorphic data is stored as DuckDB `JSON`. Query with `->`, `->>`, `json_extract`, `json_extract_string`.

| Column | Shape |
|--------|-------|
| `assistant_entries.iterations` | array of `{input_tokens, output_tokens, cache_*, type}` — Advisor server-tool beta decomposition |
| `assistant_content_blocks.tool_input` | per-tool input object (varies by tool) |
| `user_content_blocks.tool_use_result` | tool result payload |
| `assistant_entries.service_tier`, `speed`, `stop_details`, `inference_geo`, `container` | API response metadata |

**Do not sum `iterations`.** When the Advisor server-tool beta is active, a single assistant response may internally split into several "iterations" — the JSON array. Top-level `input_tokens` / `output_tokens` is the aggregate; iteration elements are a decomposition. Summing elements double-counts against the top-level (and single-iteration responses have top-level equal to `iterations[0]`, so summing appears to work on those and silently breaks on multi-iteration ones). If you need iteration-level data, use the flattened child table `assistant_usage_iterations` — it's pre-joined to `entry_id`.

---

## Accessing the original JSONL line

The raw JSONL line is intentionally not stored — it would roughly double DB size and every field is already parsed into typed columns. When you genuinely need the untouched line (debugging a parse failure, inspecting a field ingest dropped, reproducing an edge case), reconstruct it from `entries.file_path` + `entries.line_no` (1-indexed, matches `sed -n 'Np'` and `awk 'NR==N'`).

```sql
SELECT file_path, line_no
FROM entries
WHERE entry_id = <id>;        -- or: WHERE uuid = '<uuid>';
```

Then on disk:

```bash
awk "NR==<line_no>" "<file_path>" | jq '.'          # pretty
awk "NR==<line_no>" "<file_path>" | jq '.message.content'
```

Bulk extract (e.g. all entries of a session):

```sql
COPY (
  SELECT file_path, line_no
  FROM entries
  WHERE session_id = '<uuid>'
  ORDER BY line_no
) TO '/tmp/lines.csv' (HEADER, DELIMITER ',');
```

```bash
tail -n +2 /tmp/lines.csv | while IFS=, read -r fp ln; do
  awk "NR==$ln" "${fp//\"/}"
done | jq -c '.'
```

Notes on the source files:
- `transcripts.file_path` is absolute — under `~/.claude/projects/<slug>/` for main sessions, `<session>/subagents/agent-<id>.jsonl` for subagents.
- Source JSONL is append-only during a live session and immutable after. Line numbers are stable. If ingest ran and then the session continued, re-ingest to pick up new lines.
- If a source file is missing, the session was cleaned up on disk — nothing in the DB will recover the raw content.

---

## Prerequisites: `cct` and `duckdb`

Two tools need to be on `PATH`. Run these checks once per session; skip them if you've already queried the DB successfully in this conversation.

```bash
command -v cct    >/dev/null && cct --version    || echo "cct not installed"
command -v duckdb >/dev/null && duckdb --version || echo "duckdb not installed"
```

If `cct` is missing — ask the user first (external install):

```bash
curl -fsSL https://raw.githubusercontent.com/Alfredvc/cct/main/install.sh | sh
# installs prebuilt binary to ~/.local/bin (override with CCT_INSTALL_DIR=..., pin with CCT_VERSION=v0.2.0)
```

If `duckdb` is missing:

```bash
curl https://install.duckdb.org | sh
# or https://duckdb.org/install/?platform=macos&environment=cli
```

### Locate the DB

If the user already gave a path, use it and skip this section. Otherwise check in order:

```bash
# XDG default (what cct ingest writes to by default)
ls -1 "${XDG_DATA_HOME:-$HOME/.local/share}/cct/transcripts.duckdb" 2>/dev/null
# legacy / manually placed
ls -1 ./transcripts.duckdb 2>/dev/null
ls -1 ./*.duckdb 2>/dev/null
find . -maxdepth 3 -name '*.duckdb' -not -path '*/target/*' 2>/dev/null
```

Branches:

- **`~/.local/share/cct/transcripts.duckdb` (or `$XDG_DATA_HOME/cct/transcripts.duckdb`) exists** → use it. This is the default output of `cct ingest`. Only re-check freshness (`SELECT MAX(timestamp) FROM entries`) when the user is asking about "recent" / "today" / "latest session" — pure historical analytics don't need it.
- **`./transcripts.duckdb` or another `*.duckdb` exists** → ask the user whether it's the transcripts DB before querying.
- **No `*.duckdb` found** → ask where the DB lives, or whether to generate one now with `cct ingest`. Don't silently run `cct ingest` — it scans `~/.claude/projects/` and writes a multi-GB file to `~/.local/share/cct/transcripts.duckdb`.

Once confirmed, use the same path in every `duckdb <path>` invocation. `cct ingest` **overwrites** the DB from scratch on every run — it is not incremental. After upgrading `cct` (new columns or new attachment variants), re-run `cct ingest` so the schema and parsed columns match — querying an old DB with a new SKILL.md will return "Binder Error: column not found" on the new fields. Quick check that picks up most schema-update misses:

```sql
SELECT
  (SELECT COUNT(*) FROM duckdb_columns()
   WHERE table_name = 'assistant_entries' AND column_name = 'attribution_skill') AS has_attribution,
  (SELECT COUNT(*) FROM duckdb_columns()
   WHERE table_name = 'assistant_entries' AND column_name = 'cache_miss_reason_type') AS has_cache_miss;
-- both 1 ⇒ DB is at the 2026-05-05 schema or later.
```

---

## Running queries

One file, no server. From the repo root:

```bash
duckdb transcripts.duckdb                            # interactive
duckdb transcripts.duckdb "SELECT COUNT(*) FROM entries;"
duckdb transcripts.duckdb < query.sql
```

Output modes that matter:

```bash
duckdb -csv   transcripts.duckdb "..."   # pipe-friendly
duckdb -json  transcripts.duckdb "..."   # structured scripting
duckdb -line  transcripts.duckdb "..."   # vertical: one field per line, great for wide rows
duckdb -list  transcripts.duckdb "..."   # unadorned rows
```

In interactive mode, `.mode box|markdown|json|csv`, `.headers on`, `.timer on`, `.maxwidth 200`, `.schema <name>` are the common knobs. `EXPLAIN <query>` and `EXPLAIN ANALYZE <query>` check plan and timings.

### Performance

On a ~2 GB DB, full scans run in a few hundred ms to a few seconds. For interactive work, filter early — `session_id`, `timestamp`, `model`, `file_path` are all selective and DuckDB's column store handles them well. Heavy tables are `assistant_content_blocks` and `user_content_blocks`; unfiltered sorts there are noticeably slower. Use `EXPLAIN ANALYZE` when something feels slow.

---

## Introspection (when stuck)

The DB documents itself — lean on that before guessing column names or relationships.

```sql
-- all tables and views
SHOW TABLES;
SELECT view_name, comment FROM duckdb_views() WHERE NOT internal;

-- columns + comments for a specific table
SELECT column_name, data_type, comment
FROM duckdb_columns()
WHERE table_name = 'assistant_entries' AND comment IS NOT NULL;

-- every column with a billing warning
SELECT table_name, column_name, comment
FROM duckdb_columns()
WHERE comment LIKE '%DO NOT SUM%';

-- discover FK relationships (hard and soft)
SELECT table_name, column_name, comment
FROM duckdb_columns()
WHERE comment LIKE '%→%' OR comment LIKE '%~%';

-- sample a table
SELECT * FROM assistant_entries_deduped USING SAMPLE 5 ROWS;
```

If a question feels like "what does column X mean?" or "how does table Y link to Z?" — ask the DB. Schema comments are authoritative; this skill is a guide on top of them.

---

## References

Deeper methodology that doesn't belong in the main flow lives in `references/`. Load the relevant file when the question matches.

| File | Read when |
|------|-----------|
| [`references/cumulative-cost-analysis.md`](references/cumulative-cost-analysis.md) | The user asks "how much does X cost me cumulatively?" or "what would I save if I trimmed/compacted Y?" — anything where the cost of a thing injected into context (tool result, file read, hook output, system-prompt section, skill body) needs to be summed across all subsequent turns that re-read the prefix. Covers the pricing-JOIN footgun, longest-prefix rate match, the cumulative-attribution SQL pattern, hook ROI math, and the recompute-vs-`SUM(cost_usd)` sanity check that catches silent 2-3× errors. |
| [`references/token-calibration.md`](references/token-calibration.md) | The user asks how to convert chars to tokens for a category, or wants to understand why naive `chars / 4` is wrong. Empirical chars-per-token and per-block-overhead per category (Bash, Read, Grep, Glob, WebFetch, attachments, asst text/tool_use). Recalibration SQL via `REGR_*` for when a new tool dominates usage. |
| [`references/cost-decomposition-methodology.md`](references/cost-decomposition-methodology.md) | The user asks "where is my money going?" by category — `tool_result:Bash`, `attachment:hook_success`, `asst_thinking`, `system_block_first_turn`, `cache_bust:compact_summary`. Documents the billing-exact split by chars-share, the two-regime methodology (event vs non-event turns), the five known cache-bust events, the subcat extraction patterns, and the full pipeline stages from `scripts/decompose_cost.py`. Builds on the two references above. |
