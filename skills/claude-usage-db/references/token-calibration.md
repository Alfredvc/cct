# Token estimation from chars: per-category calibration

The DB stores content as chars but Anthropic bills in tokens. When you need to estimate token contributions for proportional attribution (e.g., "how much of the cumulative cost came from Bash output vs file Reads"), naive `chars / 4` is wrong in two ways: the **chars-per-token ratio** varies by content type, and there's a **per-block structural overhead** that's invisible in chars.

This file documents the empirical calibration derived from regressing actual `cache_creation_input_tokens` against content chars across ~30 days of transcripts.

---

## TL;DR

```
tokens(category) = intercept(category) + slope(category) × chars
                 ≈ overhead_per_block × N_blocks + chars / chars_per_tok(category)
```

For most text content: **~3 chars/tok**, not 4. For attribution, use **effective_chars**:

```
effective_chars(cat) = raw_chars + overhead_tok(cat) × 3 × N_blocks
share(cat) = effective_chars(cat) / Σ effective_chars
```

Then distribute billing-exact tokens by `share`. This corrects both bias sources without needing to match absolute tokens.

---

## Per-category calibration table

Derived by isolating turn pairs `(aT, aT+1)` with a single intervening block of the target type, then regressing `cc(T+1) − output_tokens(T)` (= billing-exact intervening tokens) against `LENGTH(content)`.

For assistant-side blocks: regress `output_tokens` against `LENGTH(text or tool_input)` for turns containing only that block type with no thinking.

| Category | chars/tok | overhead/block | R² | Notes |
|----------|-----------|----------------|-----|-------|
| **Assistant text** | 3.0 | ~0 | 0.88 | Intercept ≈ 1, very clean |
| **Assistant tool_use JSON** | 2.93 | ~100 tok | 0.95 | JSON-args envelope per block |
| **Assistant thinking** | n/a | n/a | — | Text not stored in DB. Derive as residual: `output_tokens − text_tok − tool_use_tok` |
| `tool_result:Bash` | 2.6 | ~200 tok | 0.35 | Noisy: text vs binary stdout, varied formats |
| `tool_result:Read` | ~3.0 | ~200 tok | 0.14 | Very noisy: code vs prose vs config; trust overhead more than slope |
| `tool_result:Grep` | 2.6 | ~190 tok | 0.64 | Match-list format, fairly consistent |
| `tool_result:Glob` | 2.9 | ~75 tok | 0.21 | Small payloads, overhead-dominated |
| `tool_result:Edit` / `tool_result:Write` | n/a | ~120 tok | 0.0 | Chars usually < overhead — the tool_use INPUT is the cost driver, not the result |
| `tool_result:WebFetch` | 3.17 | ~0 | 0.90 | HTML is dense, no per-block overhead detected |
| `tool_result:Agent` | 2.77 | ~200 tok | 0.45 | Subagent summary text |
| `tool_result:WebSearch` | 2.31 | — | 0.11 | Small sample, low confidence |
| `tool_result:AskUserQuestion` | 1.93 | — | 0.09 | Small sample |
| `tool_result:TaskUpdate` / `TaskCreate` | n/a | — | 0.0 | Overhead-dominated; chars meaningless |
| `tool_result:Skill` / `ToolSearch` | n/a | n/a | n/a | Confounded — these inject content via `attachment_entries`, not the tool_result text. Calibrate against attachment chars instead |
| `tool_result:mcp__livekit-docs__get_pages` | 3.35 | ~0 | 0.96 | Doc HTML/markdown |
| `tool_result:mcp__livekit-docs__docs_search` | 3.66 | ~0 | 0.76 | Search-result snippets |
| `tool_result:mcp__plugin_context7_context7__query-docs` | 3.43 | ~0 | 0.61 | Doc snippets |
| `tool_result:mcp__plugin_context7_context7__resolve-library-id` | 2.97 | ~0 | 0.92 | Index lookup |
| **Attachments (aggregate)** | 3.29 | ~108 tok | 0.52 | Calibrated on n=1079 clean pairs (attachment-only intervening, no tool_results). Per-type factors below. |
| User text (plain prompts) | ~3.0 (rough) | ~50 tok | — | Small contribution, not tightly calibrated |

**Variance summary:** chars/tok range 2.6–3.7 across categories. Overhead range 0–200 tok per block. Naive `chars/4` underestimates absolute tokens by ~30% on average and biases categorical shares against high-overhead-low-content tools (Bash, Edit, Glob).

### Attachment subtypes — chars and frequency (30d main sessions)

`attachment_entries.attachment_type` discriminates loaded content. Top contributors by total char volume:

| Subtype | n events | avg_chars | total_chars (30d) | Notes |
|---------|----------|-----------|---------------------|-------|
| `skill_listing` | 1,454 | 9,981 | **14.5 M** | Skill lookup table loaded into context. Biggest single attachment contributor. |
| `hook_success` | 29,369 | 334 | 9.8 M | Frequent small payloads — overhead-heavy bucket. |
| `hook_additional_context` | 5,959 | 750 | 4.5 M | Hook stdout injected as context. |
| `file` (mention via `@path`) | 360 | 7,538 | 2.7 M | Larger but rarer. |
| `task_reminder` | 5,117 | 375 | 1.9 M | TaskCreate state echoed back. |
| `edited_text_file` | 300 | 3,964 | 1.2 M | Diff after Edit/Write. |
| `queued_command` | 322 | 2,115 | 0.7 M | `/command` payload. |
| `invoked_skills` | 62 | 12,001 | 0.7 M | Big when triggered. |
| `nested_memory` | 4 | 10,257 | 41 K | Nested CLAUDE.md loads. |
| `hook_*` (other variants), `*_delta`, `*_permissions`, `date_change`, etc. | varies | mostly 0–100 | small | Marker-only entries, no real content. |

For per-subtype calibration (separate intercept/slope by attachment_type), the sample-size constraint matters — only `hook_success`, `hook_additional_context`, `task_reminder`, and `skill_listing` have enough events for a clean regression. Others are sparse.

The aggregate factor (3.29 chars/tok, 108 tok/block) is reasonable for general attribution. Subdivide only if a specific subtype dominates a session you're investigating.

---

## Why per-block overhead exists

Each content block in the API has structural framing:
- `tool_use_id` reference (UUID-length string)
- block-type markers (`tool_result`, `tool_use`)
- role tags, content-block envelope
- optional `cache_control: ephemeral` marker
- per-message metadata (role, type)

These tokenize to ~50–200 tokens **regardless of payload size**. The overhead value reflects the API's structural cost per call.

Why it matters: a session with **many small tool calls** has much higher real cost than `chars / 3` predicts. Example:

```
1 Bash call returning "ok"    (2 chars)     → ~213 tok    (almost all framing)
1 Bash call returning 100 KB  (100,000 ch.) → ~38,700 tok (overhead negligible)
```

Same payload-per-byte rate, vastly different per-call efficiency. A loop of 1000 small Bash calls adds ~213,000 tokens of pure framing — invisible in `LENGTH(content)`.

The same applies to **assistant `tool_use` blocks** (~100 tok each). Every Edit / Write / Bash the model emits costs ~100 tokens of JSON-block envelope before the actual arguments.

---

## Why per-category factors matter less when you're SPLITTING actual tokens

The methodology in `cumulative-cost-analysis.md` doesn't use chars to ESTIMATE absolute tokens — it uses chars to SPLIT billing-exact tokens across categories. So:

- Per-turn-pair, the actual tokens added to cache are **exact**: `cc(T+1) − output_tokens(T)` for user-side, `output_tokens(T)` for assistant-side.
- We just need a sensible PROPORTION to assign each category's slice.

If all categories had the same chars/tok and zero overhead, raw chars-share would be perfect. They don't, so the **effective-chars correction** (chars + overhead × 3 × N_blocks) keeps the split honest.

---

## Effective-chars formula

For category `c` in a turn pair:
```
overhead_chars_equivalent(c) = overhead_tok(c) × chars_per_tok(c)
                             ≈ overhead_tok(c) × 3
effective_chars(c) = raw_chars(c) + overhead_chars_equivalent(c) × N_blocks(c)
share(c) = effective_chars(c) / Σ effective_chars(all categories)
attributed_tok(c) = share(c) × actual_tokens_to_split
```

Where `actual_tokens_to_split` is `cc(T+1) − output_tokens(T)` for user-side or `output_tokens(T)` for assistant-side.

This makes:
- Shares ≈ true category proportions
- Sum across categories = exact billing tokens (no double-count, no leak)
- Calibration errors only affect WITHIN-category fine-grained breakdowns, not top-level

---

## Worked example

Session has between two consecutive assistant turns:
- 100 Bash calls, 1,000 chars output each → 100,000 chars
- 50 WebFetch calls, 5,000 chars each → 250,000 chars

`cc(T+1) − output_tokens(T) = 136,000` tokens (from billing).

**Naive chars-share:**
- Bash: 100K / 350K = 28.6% → 38.9k tok
- WebFetch: 71.4% → 97.1k tok

**Effective-chars share:**
- Bash effective = 100K + 200 × 3 × 100 = 160K
- WebFetch effective = 250K + 0 × 50 = 250K
- Bash share: 160K / 410K = 39.0% → 53.0k tok
- WebFetch share: 61.0% → 83.0k tok

**Truth (per category factors):**
- Bash actual = 100 × 200 + 100,000 / 2.6 = 20K + 38.5K = 58.5K tok
- WebFetch actual = 50 × 0 + 250,000 / 3.17 = 78.9K tok
- (Total 137.4K ≈ 136K observed ✓)

Effective-chars matches truth within 10%. Naive chars-share is off by 25–35%.

---

## When you can skip this

For quick top-level questions ("which tool dominates my cost?"), naive chars-share with chars/3 is good enough — categorical shares are usually within 25%, sufficient to identify the top contributor.

Use the effective-chars formula when:
- You want to compare categories with very different per-block overhead (Bash/Edit vs WebFetch)
- You're sizing an intervention (compaction hook, cache strategy) and ROI must be ±15%
- The session has many small tool calls — overhead share grows

---

## Recalibrating

These numbers are from a specific user's transcripts (~30 days, mostly Sonnet 4.6 / Haiku 4.5). To recalibrate on different data, run:

```sql
-- Per-tool tool_result calibration (single-block intervening)
WITH asst AS (
  SELECT e.session_id, e.entry_id, d.output_tokens AS out_tok,
         d.cache_creation_input_tokens AS cc,
         LEAD(e.entry_id) OVER w AS next_eid,
         LEAD(d.cache_creation_input_tokens) OVER w AS next_cc
  FROM assistant_entries_deduped d
  JOIN entries e ON e.entry_id = d.entry_id
  WHERE d.model != '<synthetic>' AND NOT e.is_sidechain
  WINDOW w AS (PARTITION BY e.session_id ORDER BY e.entry_id)),
intervening AS (
  SELECT a.entry_id AS aT_eid, a.next_cc, a.out_tok,
         COUNT(*) AS n_blocks,
         ANY_VALUE(tu.tool_name) AS tool_name,
         SUM(LENGTH(CAST(ucb.tool_result_content AS VARCHAR))) AS result_chars
  FROM asst a
  JOIN entries ue ON ue.session_id = a.session_id
                  AND ue.entry_id > a.entry_id
                  AND ue.entry_id < a.next_eid
                  AND ue.type = 'user'
  JOIN user_content_blocks ucb ON ucb.entry_id = ue.entry_id
                              AND ucb.block_type = 'tool_result'
  LEFT JOIN assistant_content_blocks tu ON tu.tool_use_id = ucb.tool_use_id
  WHERE a.next_eid IS NOT NULL
  GROUP BY a.entry_id, a.next_cc, a.out_tok)
SELECT i.tool_name,
       COUNT(*) AS n,
       ROUND(REGR_INTERCEPT(i.next_cc - i.out_tok, i.result_chars), 1) AS overhead_tok,
       ROUND(1.0 / NULLIF(REGR_SLOPE(i.next_cc - i.out_tok, i.result_chars), 0), 2) AS chars_per_tok,
       ROUND(REGR_R2(i.next_cc - i.out_tok, i.result_chars), 3) AS r2
FROM intervening i
WHERE i.n_blocks = 1
GROUP BY 1
HAVING COUNT(*) >= 50
ORDER BY n DESC;
```

For assistant-side calibration:

```sql
-- Text-only assistant turns (no thinking, no tool_use)
WITH msg_blocks AS (
  SELECT e.file_path, ae.message_id,
         SUM(CASE WHEN acb.block_type='text'     THEN LENGTH(acb.text) ELSE 0 END) AS text_chars,
         SUM(CASE WHEN acb.block_type='tool_use' THEN LENGTH(CAST(acb.tool_input AS VARCHAR)) ELSE 0 END) AS tool_chars,
         BOOL_OR(acb.block_type='thinking') AS has_thinking
  FROM assistant_content_blocks acb
  JOIN entries e ON e.entry_id = acb.entry_id
  JOIN assistant_entries ae ON ae.entry_id = e.entry_id
  WHERE NOT e.is_sidechain
  GROUP BY 1, 2)
SELECT
  REGR_INTERCEPT(d.output_tokens, m.text_chars) AS text_overhead,
  ROUND(1.0 / REGR_SLOPE(d.output_tokens, m.text_chars), 2) AS text_chars_per_tok,
  REGR_R2(d.output_tokens, m.text_chars) AS r2_text
FROM assistant_entries_deduped d
JOIN entries e ON e.entry_id = d.entry_id
JOIN msg_blocks m ON m.file_path = e.file_path AND m.message_id = d.message_id
WHERE d.model != '<synthetic>' AND NOT m.has_thinking
  AND m.tool_chars = 0 AND m.text_chars > 0;
```

The numbers will shift with model mix, tooling changes, and content distribution. Re-run quarterly or whenever a new tool dominates usage.

---

## Open calibration gaps

1. **Read variance**: R²=0.14 means file content density varies hugely (binary, code, JSON, prose). Subdividing by `file_ext` from `tool_uses.effective_path` would tighten the calibration, but adds N categories.
2. **Bash subcategorization**: same issue — `git status` output looks nothing like `cat large.json`. Subdivide by `input_command` first token if you need fine-grained Bash attribution.
3. **Per-subtype attachment factors**: aggregate 3.29 chars/tok + 108 tok/block works for top-level. For per-subtype precision, run the calibration query restricted to one `attachment_type` at a time (only `hook_success`, `hook_additional_context`, `task_reminder`, `skill_listing` have enough sample for clean regression).
4. **Subagent-specific calibration**: subagents have shorter system prompts and different tool mixes; recalibrate separately if attributing subagent costs precisely.

These are fine to leave loose for trend-level analysis. Tighten them only if a specific decision needs <15% accuracy on a particular category.
