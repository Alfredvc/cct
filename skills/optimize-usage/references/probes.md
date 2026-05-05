# Probe catalog

Every probe assumes the billing-safe dedup rule: use `assistant_entries_deduped` and filter `message_id IS NOT NULL` for cost aggregates. See `cct-db` for schema details.

Before running any probe, fill in the pre-registration block from Phase 2:

```
Probe: <name>
Tests: H1, H3 (pre-commit)
Expected under H1: <concrete signal>
Expected under H3: <concrete signal>
Result that would refute both: <what you'd see>
```

If the result matches neither pre-registered hypothesis, do not retrofit a narrative — add a new hypothesis and re-probe.

---

## Table of contents

1. Stream-separated token-type cost
2. Cache-reset probe (TTL-aware)
3. System-prompt size estimate (first-turn cache-creation)
4. MCP tool_result size distribution
5. Hook-injected content
6. Two-regime session analysis (volume vs per-session)
7. Session size distributions (turns AND tokens)
8. Biggest write artifacts
9. Wasted re-reads within a session
10. Largest tool_result payloads
11. Agent spawn model reality (explicit vs inherited vs actual)
12. Extended-thinking tokens
13. Image / document content blocks
14. Autonomous loop detection
15. Compaction cycles
16. Most expensive individual turns
17. Skill use controlled for session length
18. Artifact end-to-end cost (template)

---

## 1. Stream-separated token-type cost (last N active days)

```sql
SELECT CASE WHEN e.is_sidechain THEN 'sidechain' ELSE 'main' END AS stream,
       ROUND(SUM(d.input_tokens * p.input_per_mtok)/1e6, 0)               AS input_usd,
       ROUND(SUM(d.cache_creation_5m * p.cache_creation_5m_per_mtok)/1e6, 0) AS cc5m_usd,
       ROUND(SUM(d.cache_creation_1h * p.cache_creation_1h_per_mtok)/1e6, 0) AS cc1h_usd,
       ROUND(SUM(d.cache_read_input_tokens * p.cache_read_per_mtok)/1e6, 0)  AS cr_usd,
       ROUND(SUM(d.output_tokens * p.output_per_mtok)/1e6, 0)             AS output_usd,
       ROUND(SUM(d.cost_usd), 0) AS total_usd
FROM assistant_entries_deduped d
JOIN entries e        USING(entry_id)
JOIN model_pricing p  USING(model)
WHERE d.message_id IS NOT NULL
  AND e.timestamp > CURRENT_TIMESTAMP - INTERVAL 14 DAY
GROUP BY 1;
```

### Mechanism implication table (consult after running)

| Dominant stream + token | Likely mechanism | Next probe |
|---|---|---|
| main-chain cache-read | long sessions + heavy prefix (CLAUDE.md / skills / MCP) | session size distributions (#7) |
| main-chain cache-creation-5m | mid-session prefix invalidation | cache-reset probe (#2) |
| main-chain cache-creation-1h | prefix is set up 1h then re-cached | cache-reset probe (#2), check for TTL crossing |
| sidechain cache-creation | cold subagent spawns | Agent spawn model (#11), Agent prompt length (#8) |
| output (any stream) | verbose generation / thinking / recitation | thinking tokens (#12), Write sizes (#8) |
| input (uncached) | cache routinely missing | cache-reset probe (#2), first-turn cc (#3) |

Do not assume a pattern generalizes across users. Verify stream purity on this user's DB before claiming it.

---

## 2. Cache-reset probe (TTL-aware)

Threshold is the user's own p90 of main-chain cc per turn, not a hardcoded 20k. A user with an 8k-token prefix still experiences death-by-a-thousand-cuts.

```sql
WITH p90 AS (
  SELECT QUANTILE_CONT(d.cache_creation_5m + d.cache_creation_1h, 0.9) AS thresh
  FROM assistant_entries_deduped d JOIN entries e USING(entry_id)
  WHERE d.message_id IS NOT NULL AND NOT e.is_sidechain
),
main_turns AS (
  SELECT e.session_id, e.timestamp,
         d.cache_creation_5m AS cc5m,
         d.cache_creation_1h AS cc1h,
         LAG(e.timestamp) OVER (PARTITION BY e.session_id ORDER BY e.timestamp) AS prev_ts,
         ROW_NUMBER()      OVER (PARTITION BY e.session_id ORDER BY e.timestamp) AS rn
  FROM assistant_entries_deduped d JOIN entries e USING(entry_id)
  WHERE d.message_id IS NOT NULL AND NOT e.is_sidechain
)
SELECT CASE WHEN prev_ts IS NULL THEN 'first_turn'
            WHEN cc5m > cc1h AND DATE_DIFF('minute', prev_ts, timestamp) < 5 THEN 'cc5m <5min (true invalidation)'
            WHEN cc5m > cc1h AND DATE_DIFF('minute', prev_ts, timestamp) < 60 THEN 'cc5m 5-60min (TTL or seam)'
            WHEN cc1h > cc5m AND DATE_DIFF('minute', prev_ts, timestamp) BETWEEN 55 AND 65 THEN 'cc1h ~60min (TTL expiry, normal)'
            WHEN cc1h > cc5m AND DATE_DIFF('minute', prev_ts, timestamp) < 55 THEN 'cc1h <55min (true invalidation)'
            ELSE 'other' END AS bucket,
       COUNT(*) AS events,
       ROUND(SUM(cc5m + cc1h)/1e6, 1) AS mtok
FROM main_turns, p90
WHERE rn > 1 AND (cc5m + cc1h) > p90.thresh
GROUP BY 1;
```

High `<5min` or `<55min cc1h` counts = something is invalidating the prefix mid-session. Hunt causes: CLAUDE.md edits, plugin reconnects, tool-list changes, mode switches, MCP reloads, hook-triggered injections.

---

## 3. System-prompt size estimate (first-turn cache-creation)

First-turn cache-creation on a fresh session approximates the system prompt + CLAUDE.md + MCP schemas + tool list + hooks footprint.

```sql
WITH first_turns AS (
  SELECT e.session_id, e.timestamp, d.cache_creation_input_tokens AS cc,
         ROW_NUMBER() OVER (PARTITION BY e.session_id ORDER BY e.timestamp) AS rn,
         e.cwd
  FROM assistant_entries_deduped d JOIN entries e USING(entry_id)
  WHERE d.message_id IS NOT NULL AND NOT e.is_sidechain
)
SELECT cwd, COUNT(*) AS sessions,
       ROUND(AVG(cc)/1000, 1) AS avg_k_tokens,
       ROUND(MAX(cc)/1000, 1) AS max_k_tokens
FROM first_turns
WHERE rn = 1
GROUP BY 1
ORDER BY avg_k_tokens DESC;
```

Large variance across `cwd` = some projects have bigger CLAUDE.md or more MCP servers loaded. Compare first-turn cc against the sum of (global CLAUDE.md + project CLAUDE.md + skill sizes + MCP schema estimates) to see what's unaccounted for.

---

## 4. MCP tool_result size distribution

Many users' MCP tool calls return multi-kilotoken JSON that re-caches on every subsequent turn. Invisible to write-side probes.

```sql
SELECT SPLIT_PART(tu.name, '__', 1) || '__' || SPLIT_PART(tu.name, '__', 2) AS mcp_prefix,
       COUNT(*) AS calls,
       ROUND(AVG(LENGTH(json_extract_string(ucb.tool_use_result, 'content'))) / 1000, 1) AS avg_k_chars,
       ROUND(MAX(LENGTH(json_extract_string(ucb.tool_use_result, 'content'))) / 1000, 1) AS max_k_chars
FROM tool_uses tu
JOIN user_content_blocks ucb USING(entry_id)
WHERE tu.name LIKE 'mcp__%'
GROUP BY 1
ORDER BY avg_k_chars DESC;
```

---

## 5. Hook-injected content

```sql
SELECT hook_name,
       COUNT(*) AS fires,
       ROUND(AVG(LENGTH(output)) / 1000, 1) AS avg_k_chars,
       ROUND(SUM(LENGTH(output)) / 1000, 1) AS total_k_chars
FROM system_hook_infos
GROUP BY 1
ORDER BY total_k_chars DESC;
```

A PostToolUse hook firing on every Edit with a 2k-char lint report adds up fast. Cross-reference total_k_chars with the sessions where that hook fires to estimate its cache-read contribution.

---

## 6. Two-regime session analysis

```sql
WITH sess AS (
  SELECT e.session_id,
         DATE_TRUNC('week', MIN(e.timestamp)) AS wk,
         SUM(d.cost_usd) AS cost
  FROM assistant_entries_deduped d JOIN entries e USING(entry_id)
  WHERE d.message_id IS NOT NULL
  GROUP BY e.session_id
)
SELECT wk, COUNT(*) AS sessions,
       ROUND(SUM(cost), 0) AS total_usd,
       ROUND(AVG(cost), 2) AS avg_per_sess,
       ROUND(QUANTILE_CONT(cost, 0.9), 2) AS p90
FROM sess GROUP BY 1 ORDER BY 1 DESC;
```

Volume growth (more sessions) ≠ per-session cost growth. Volume = habit/task decomposition. Per-session cost = context bloat, model mix, artifact bloat.

---

## 7. Session size distributions

Turns distribution:

```sql
SELECT CASE WHEN turns <= 50 THEN '1-50'
            WHEN turns <= 200 THEN '51-200'
            WHEN turns <= 500 THEN '201-500'
            ELSE '500+' END AS bucket,
       COUNT(*) AS sessions,
       ROUND(SUM(cost)) AS total_usd
FROM (
  SELECT e.session_id, COUNT(*) AS turns, SUM(d.cost_usd) AS cost
  FROM assistant_entries_deduped d JOIN entries e USING(entry_id)
  WHERE d.message_id IS NOT NULL AND NOT e.is_sidechain
  GROUP BY 1
)
GROUP BY 1 ORDER BY 1;
```

Peak-token-size distribution (fat-context detection — 1M-context 2× pricing kicks in above 200k):

```sql
SELECT CASE WHEN peak <= 50000 THEN '<=50k'
            WHEN peak <= 200000 THEN '50k-200k'
            ELSE '>200k (2x tier)' END AS bucket,
       COUNT(*) AS sessions,
       ROUND(SUM(cost)) AS total_usd
FROM (
  SELECT e.session_id,
         MAX(d.input_tokens + d.cache_creation_input_tokens + d.cache_read_input_tokens) AS peak,
         SUM(d.cost_usd) AS cost
  FROM assistant_entries_deduped d JOIN entries e USING(entry_id)
  WHERE d.message_id IS NOT NULL AND NOT e.is_sidechain
  GROUP BY 1
)
GROUP BY 1 ORDER BY 1;
```

A 30-turn session at 500k tokens is not a long-session archetype — it's a fat-context one. Different lever.

---

## 8. Biggest write artifacts

```sql
SELECT tu.input_file_path AS path,
       COUNT(*)  AS writes,
       ROUND(AVG(LENGTH(json_extract_string(tu.input,'content')))/1000, 1) AS avg_k_chars,
       ROUND(MAX(LENGTH(json_extract_string(tu.input,'content')))/1000, 1) AS max_k_chars
FROM tool_uses tu
WHERE tu.name = 'Write' AND tu.input_file_path IS NOT NULL
GROUP BY 1
HAVING MAX(LENGTH(json_extract_string(tu.input,'content'))) > 20000
ORDER BY max_k_chars DESC LIMIT 30;
```

Also check Agent tool prompt length (subagent briefings):

```sql
SELECT ROUND(LENGTH(json_extract_string(tu.input, 'prompt')) / 1000, 1) AS k_chars,
       tu.input
FROM tool_uses tu
WHERE tu.name = 'Agent'
ORDER BY LENGTH(json_extract_string(tu.input, 'prompt')) DESC
LIMIT 20;
```

---

## 9. Wasted re-reads within a session

```sql
WITH r AS (
  SELECT e.session_id, tu.input_file_path, COUNT(*) AS reads
  FROM tool_uses tu JOIN entries e USING(entry_id)
  WHERE tu.name = 'Read' AND tu.input_file_path IS NOT NULL
  GROUP BY 1,2
  HAVING COUNT(*) > 1
)
SELECT input_file_path, SUM(reads - 1) AS wasted_reads, MAX(reads) AS peak
FROM r GROUP BY 1 ORDER BY wasted_reads DESC LIMIT 30;
```

---

## 10. Largest tool_result payloads

Complement to write-side artifact probes. A single `WebFetch` of a 100k-char page or a `Bash` dumping a log matters more than many small reads.

```sql
SELECT tu.name AS tool,
       ROUND(MAX(LENGTH(json_extract_string(ucb.tool_use_result, 'content'))) / 1000, 1) AS max_k_chars,
       ROUND(AVG(LENGTH(json_extract_string(ucb.tool_use_result, 'content'))) / 1000, 1) AS avg_k_chars,
       COUNT(*) AS calls
FROM tool_uses tu
JOIN user_content_blocks ucb USING(entry_id)
GROUP BY 1
ORDER BY max_k_chars DESC
LIMIT 30;
```

---

## 11. Agent spawn model reality

Explicit-vs-inherited, then the *actually executed* model.

Explicit vs inherited:

```sql
SELECT json_extract_string(tu.input,'subagent_type') AS subtype,
       json_extract_string(tu.input,'model')         AS explicit_model,
       COUNT(*) AS calls
FROM tool_uses tu
WHERE tu.name = 'Agent'
GROUP BY 1, 2
ORDER BY calls DESC;
```

Rows with `explicit_model IS NULL` inherit the parent's current `/model`. That parent model is not always Opus — it tracks whatever the user had configured at spawn time. The observed subagent model is the ground truth; join to the subagent's first assistant entry to verify:

```sql
-- Join Agent call → spawned session (via user_id linkage in transcripts) → first assistant_entry.model
-- Schema-dependent; see cct-db for the exact join keys.
```

---

## 12. Extended-thinking tokens

```sql
SELECT e.session_id,
       ROUND(SUM(LENGTH(acb.text)) / 1000, 1) AS thinking_k_chars,
       COUNT(*) AS thinking_blocks
FROM assistant_content_blocks acb JOIN entries e USING(entry_id)
WHERE acb.block_type = 'thinking'
GROUP BY 1
ORDER BY thinking_k_chars DESC
LIMIT 20;
```

Thinking tokens are billed at output rate. A thinking-heavy user will otherwise be misdiagnosed as "verbose generation" with the wrong lever.

---

## 13. Image / document content blocks

```sql
SELECT block_type, COUNT(*) AS blocks,
       COUNT(DISTINCT entry_id) AS entries
FROM user_content_blocks
WHERE block_type IN ('image', 'document')
GROUP BY 1;
```

Image tokens are priced per-image and persist in the cache. A UI-heavy user (Playwright, axe, design-review skills) accrues this silently.

---

## 14. Autonomous loop detection

Regularly-spaced turns off-hours suggest a loop/cron not a human:

```sql
WITH t AS (
  SELECT e.session_id, e.timestamp,
         LAG(e.timestamp) OVER (PARTITION BY e.session_id ORDER BY e.timestamp) AS prev_ts,
         EXTRACT(hour FROM e.timestamp) AS hr
  FROM assistant_entries_deduped d JOIN entries e USING(entry_id)
  WHERE d.message_id IS NOT NULL
)
SELECT hr, COUNT(*) AS turns,
       ROUND(STDDEV(EXTRACT(epoch FROM (timestamp - prev_ts))), 0) AS gap_stddev_s,
       ROUND(AVG(EXTRACT(epoch FROM (timestamp - prev_ts))), 0) AS gap_avg_s
FROM t
WHERE prev_ts IS NOT NULL
GROUP BY 1
ORDER BY 1;
```

Low stddev at a fixed hour-of-day = loop. Cross-reference with `scheduled_triggers`, `cron_jobs`, or the `loop` / `ScheduleWakeup` / `CronCreate` tool usage.

---

## 15. Compaction cycles

```sql
SELECT e.session_id, COUNT(*) AS summaries
FROM summary_entries se JOIN entries e USING(entry_id)
GROUP BY 1
HAVING COUNT(*) > 1
ORDER BY summaries DESC;
```

Multiple summaries per session = recurring compaction = recurring cold-cache payment. Behavioral fix: `/clear` at task boundary rather than letting auto-compact fire.

---

## 16. Most expensive individual turns

```sql
SELECT e.session_id, e.entry_id, d.model, d.cost_usd,
       d.input_tokens + d.cache_creation_input_tokens + d.cache_read_input_tokens AS ctx_tok,
       d.output_tokens
FROM assistant_entries_deduped d JOIN entries e USING(entry_id)
WHERE d.message_id IS NOT NULL
ORDER BY d.cost_usd DESC NULLS LAST
LIMIT 20;
```

Then extract the raw JSONL line for the top 5 to see contents (tool results, thinking, long output).

---

## 17. Skill use controlled for session length

```sql
WITH sess_with_skill AS (
  SELECT DISTINCT e.session_id
  FROM tool_uses tu JOIN entries e USING(entry_id)
  WHERE tu.name = 'Skill' AND json_extract_string(tu.input,'skill') = '<name>'
),
turns AS (
  SELECT e.session_id,
         ROW_NUMBER() OVER (PARTITION BY e.session_id ORDER BY e.timestamp) AS turn,
         d.cost_usd, d.model
  FROM assistant_entries_deduped d JOIN entries e USING(entry_id)
  WHERE d.message_id IS NOT NULL
)
SELECT CASE WHEN s.session_id IS NOT NULL THEN 'with' ELSE 'without' END AS grp,
       CASE WHEN turn <= 50 THEN '1-50'
            WHEN turn <= 200 THEN '51-200'
            ELSE '201+' END AS bucket,
       ROUND(AVG(t.cost_usd)*100, 2) AS avg_cents
FROM turns t LEFT JOIN sess_with_skill s USING(session_id)
WHERE t.model LIKE 'claude-opus%'
GROUP BY 1, 2 ORDER BY 2, 1;
```

Controls for turn position so you don't conflate skill-use with long-session effects. "Sessions that used X cost 4.5× more" is often just a proxy for harder tasks.

---

## 18. Artifact end-to-end cost (template, not a single query)

For any artifact (plan, design doc, research output, long CLAUDE.md):

1. **Write cost:** output tokens of the Write calls on that path × output rate.
2. **Ingest cost:** distinct subagent sessions that Read it × size × cache-create rate.
3. **Recitation cost:** Agent prompts that embed excerpts (`LENGTH(prompt)` minus template overhead).
4. **Context-tax cost:** once in a subagent context, every turn cache-reads it (≈ size × cache-read rate × turn count).

Sum these to get the true marginal cost of producing the artifact one more time. This is the per-occurrence number for Phase 3 traces.
