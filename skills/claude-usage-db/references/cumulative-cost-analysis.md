# Cumulative cost attribution

Use this reference when answering "how much does X cost me cumulatively?" — questions where X is something that gets injected into the conversation (tool result, file content, hook output, system prompt section, skill body) and stays there for the rest of the session. Examples:

- "Would compacting Bash outputs save money?"
- "How much is my CLAUDE.md actually costing me?"
- "What's the cumulative cost of MCP tool definitions?"
- "If I trim file reads to 50%, what would I save?"
- "Is loading skill X eating my budget?"

The answer is never just "the cost of the assistant turn that produced it." It's the cost of that turn **plus every later turn in the same session that re-reads the prefix containing it**. That second term usually dominates.

---

## Why it's cumulative: cache TTL refreshes on hit

From Anthropic's prompt caching docs (verified):

> The cache is refreshed for no additional cost each time the cached content is used.

Default TTL is 5 minutes, but **every cache hit resets the timer**. In an active session where each turn comes within 5 min of the last, the prefix stays cached for the entire session. Hierarchical invalidation (tools → system → messages) only kicks in when an earlier prefix layer actually changes — appending new turns at the end does not invalidate the earlier ones.

Consequences:

1. A token added at turn `p` of a session that runs `N` more turns is paid roughly `N + 1` times: once as `cache_creation` on turn `p+1`, then once as `cache_read` on each of turns `p+2 .. N`.
2. The `cache_creation` cost is per-token cheap relative to fresh input but ~12× the `cache_read` rate. Cache-read is the dominant cumulative term in long sessions.
3. Sessions with >5 min idle gaps break this. After a gap the prefix is rebuilt as `cache_creation` on the next turn, which costs *more* than this model attributes (12× the cr rate). So the model **under-attributes** when sessions are bursty.
4. Context compaction (rare — ~3% of sessions in a typical DB) drops older turns from the prefix. After compaction, the original tokens are gone, replaced by the summary. This **over-attributes** the original tokens past the compaction boundary.

In practice (1) and (2) make the model directionally correct as an upper bound; (3) and (4) are the main correction terms.

---

## The pricing JOIN footgun

`assistant_entries.model` is dated (`claude-haiku-4-5-20251001`). `model_pricing.model` is short (`claude-haiku-4-5`, `claude-haiku`). The natural prefix-match join

```sql
LEFT JOIN model_pricing p ON d.model LIKE p.model || '%'
```

**matches multiple pricing rows** for any model whose family has both a versioned and an unversioned entry. `claude-haiku-4-5-20251001` matches `claude-haiku` *and* `claude-haiku-4-5`. Each assistant row is duplicated, every cost computed from those rows is inflated proportionally, and your totals can be off by 3× — silently.

This is the worst kind of bug because the result still looks plausible. If you don't sanity check (next section), you ship a wrong answer.

### The fix: longest-prefix match

Materialize a one-row-per-model rate table that picks the longest-prefix match per assistant model:

```sql
WITH model_rates AS (
  SELECT m AS asst_model,
         (SELECT input_per_mtok             FROM model_pricing p
          WHERE m LIKE p.model || '%' ORDER BY LENGTH(p.model) DESC LIMIT 1) AS in_rate,
         (SELECT output_per_mtok            FROM model_pricing p
          WHERE m LIKE p.model || '%' ORDER BY LENGTH(p.model) DESC LIMIT 1) AS out_rate,
         (SELECT cache_read_per_mtok        FROM model_pricing p
          WHERE m LIKE p.model || '%' ORDER BY LENGTH(p.model) DESC LIMIT 1) AS cr_rate,
         (SELECT cache_creation_5m_per_mtok FROM model_pricing p
          WHERE m LIKE p.model || '%' ORDER BY LENGTH(p.model) DESC LIMIT 1) AS cc_rate
  FROM (SELECT DISTINCT model AS m FROM assistant_entries_deduped WHERE model IS NOT NULL) x
)
```

Now `JOIN model_rates mr ON mr.asst_model = d.model` is a clean 1:1 join. Every cost query downstream uses this CTE.

If a model is missing from `model_pricing` entirely, all four rate columns are NULL — surface those rows explicitly rather than letting them silently zero out.

---

## Sanity check: recompute observed total

Before trusting any attribution number, verify the rate-join by recomputing total observed cost from rates × tokens and comparing to `SUM(cost_usd)`. They should match within ~10% (the residual is mostly `cache_creation_1h` if you billed at the 5m rate, plus rounding).

```sql
WITH model_rates AS ( … as above … )
SELECT
  ROUND(SUM(d.cost_usd), 2) AS observed_total,
  ROUND(SUM(d.cache_read_input_tokens     * mr.cr_rate
          + d.cache_creation_input_tokens * mr.cc_rate
          + d.input_tokens                * mr.in_rate
          + d.output_tokens               * mr.out_rate) / 1e6, 2) AS recomputed_total
FROM assistant_entries_deduped d
JOIN model_rates mr ON mr.asst_model = d.model
WHERE d.model != '<synthetic>';
```

If `recomputed_total` is 2-3× the observed, you have a JOIN duplicate (almost always the pricing match). If it's far below, you're missing a model in `model_pricing` or the wrong rate column.

A second sanity bound: any token-attribution result must satisfy
```
sum_of_attributed_cache_read_tokens  ≤  SUM(cache_read_input_tokens) on assistant_entries_deduped
```
If you exceed observed, your model is double-counting.

---

## Pattern: cumulative attribution to an injection event

The general shape. Replace `injection_events` with the table/CTE that locates each thing whose cost you want to attribute. Each row needs `(session_id, b_eid, T)` — the session it landed in, the entry it landed at, and its token count.

```sql
WITH model_rates AS ( … ),
injection_events AS (
  -- one row per thing-you're-attributing.
  -- T = token estimate; chars/4 is the usual proxy for text/JSON.
  SELECT e.session_id, e.entry_id AS b_eid,
         CAST(LENGTH(<text or json>) / 4.0 AS DOUBLE) AS T
  FROM <wherever the event lives>
  …
),
asst AS (
  SELECT e.session_id, e.entry_id AS a_eid, mr.cr_rate, mr.cc_rate
  FROM assistant_entries_deduped d
  JOIN entries e ON e.entry_id = d.entry_id
  JOIN model_rates mr ON mr.asst_model = d.model
  WHERE NOT e.is_sidechain AND d.model != '<synthetic>'
),
-- cache_read: one row per (event, every later asst turn in same session)
cr_per_event AS (
  SELECT b.b_eid, b.session_id, ANY_VALUE(b.T) AS T,
         SUM(a.cr_rate) AS sum_cr_rate
  FROM injection_events b
  JOIN asst a ON a.session_id = b.session_id AND a.a_eid > b.b_eid
  GROUP BY 1, 2
),
-- cache_creation: at the FIRST asst turn after the event
first_after AS (
  SELECT b.b_eid, b.session_id, ANY_VALUE(b.T) AS T, MIN(a.a_eid) AS first_aid
  FROM injection_events b
  JOIN asst a ON a.session_id = b.session_id AND a.a_eid > b.b_eid
  GROUP BY 1, 2
),
cc_per_event AS (
  SELECT f.b_eid, f.session_id, f.T, a.cc_rate
  FROM first_after f
  JOIN asst a ON a.a_eid = f.first_aid
)
SELECT
  COUNT(*) AS n_events,
  ROUND(SUM(cr.T * cr.sum_cr_rate) / 1e6, 2) AS cumulative_cr_usd,
  ROUND(SUM(cc.T * cc.cc_rate)     / 1e6, 2) AS one_shot_cc_usd,
  ROUND(SUM(cr.T * cr.sum_cr_rate
          + cc.T * cc.cc_rate)     / 1e6, 2) AS total_attributed_usd
FROM cr_per_event cr
LEFT JOIN cc_per_event cc USING (b_eid, session_id);
```

What this does:

- `asst` is the rated event stream — every billed assistant turn with the right cr/cc rates attached. Filtering subagents/sidechains out keeps attribution to main-chain spend; if you're attributing something that propagates into subagents (rare), drop the `is_sidechain` filter and join through `transcripts.parent_session_id`.
- `cr_per_event` is the cumulative term: for each injection, sum up the cache-read rate of every assistant turn after it in the same session. Multiplying by `T` gives the cache-read dollars that injection cost over the rest of the session.
- `cc_per_event` is the one-shot cache-creation cost on the first turn after the injection, when its tokens enter the cached prefix.
- The final SELECT is `Σ T·N·cr + Σ T·cc` — the cumulative formula.

---

## Replays in forked sessions

When a user forks/resumes a session, the original transcript is replayed into the new `session_id` with new `entry_id`s. The same `tool_use_id` (or text content) appears in multiple sessions. Each replayed copy is a separate billing event in its session — the fork really does pay cache-read on those tokens during its own subsequent turns.

Therefore: **do not deduplicate by `tool_use_id`** when attributing. Group by `(b_eid, session_id)` so replays count as separate events. The total attribution naturally sums across all session occurrences, which is what you want.

But for **hook cost** (next section), use `COUNT(DISTINCT tool_use_id)` — the hook fires once per real execution, not once per replay.

---

## Hook / compaction ROI

For "would a hook that compresses X save me money?" questions, the calculation is:

```
net_savings = r · attributed_cumulative_cost  −  Σ hook_cost_per_call
```

where `r` is the reduction fraction (e.g. 0.5 for half-size).

### Per-call hook cost

If the hook calls a model M with input rate `in_M` and output rate `out_M` (per Mtok):

```
hook_cost(T) = (T + overhead) · in_M / 1e6  +  (1 − r) · T · out_M / 1e6
```

`overhead` is the system instruction the compressor needs (usually 100-300 tok). The output is the compressed payload, sized `(1−r)·T`.

Closed form for the sum across `K` triggering calls with total input tokens `Σ T`:

```
Σ hook_cost = K · overhead · in_M / 1e6  +  Σ T · (in_M + (1−r)·out_M) / 1e6
```

For Haiku 4.5 ($1/$5) at r=0.5 the multiplier on `Σ T` is `(1 + 0.5·5) = 3.5` per Mtok. At r=0.75 it's `2.25` per Mtok.

### Threshold gating

If the hook only fires when input is above some threshold (e.g. >500 tok), recompute both sides over only the gated subset:

- Savings = `r · attributed_cumulative_cost_for_gated_events`
- Hook cost = sum over gated calls only

Gating almost always wins on ROI: small payloads save little (their cumulative cost is bounded by the rate they're saving, and they pay full hook overhead). Compute several thresholds and compare.

### What's NOT in this model (be honest)

- **Quality tax.** Lossy compression can cause Claude to re-run the command, ask follow-ups, or take a worse path. Each of those is a new turn that adds its own cumulative cost. A bad compressor erases the savings. There is no way to estimate this from the DB; flag it as a risk.
- **Latency tax.** Every Bash call now waits for a model round-trip. Not a dollar cost, but real UX impact.
- **Idle-gap underbilling.** When sessions go cold for >5 min, the bash output gets re-cached as `cache_creation` (12× cr rate) on resume. This model accounts for it as cache-read. So the realistic cumulative cost is somewhat higher than this query reports.

---

## Token estimate sensitivity

`chars / 4` is the standard proxy and good to ~25%. JSON-wrapped tool results have overhead (quotes, type fields, escapes) that can make `chars/5` more accurate; dense code can be `chars/3`. When the answer matters, run a sensitivity:

```sql
-- inside cr_per_event, before the final SELECT
SELECT
  ROUND(SUM(chars/3.0 * sum_cr_rate)/1e6, 2) AS cr_usd_per3,
  ROUND(SUM(chars/4.0 * sum_cr_rate)/1e6, 2) AS cr_usd_per4,
  ROUND(SUM(chars/5.0 * sum_cr_rate)/1e6, 2) AS cr_usd_per5
FROM …
```

The spread is your honest uncertainty band.

---

## Worked example: was Bash output worth compacting?

This is the analysis the skill author ran on their own DB (~85 days, $9k total cost, ~58k Bash calls). Numbers will differ on yours; the *methodology* is what to copy.

1. Locate the injection events: every `tool_result` block whose `tool_use_id` came from a `tool_name='Bash'` `tool_use` block. Token count = `chars/4` of the JSON `tool_result_content`.
2. Build `model_rates` (longest-prefix match) and `asst` (rated, main-chain only).
3. Apply the cumulative-attribution pattern above.
4. Sanity check: recompute total observed cost. First attempt was 3× off — that revealed the pricing-JOIN dup bug. After fixing: recomputed total $8.3k vs observed $9.0k (9% gap, mostly 1h cache).
5. Result: bash injections account for **~$470 over 85 days, ≈5% of total spend**. Cumulative cache-read dominates (`$406`); one-shot cache-creation is small (`$64`).
6. Hook ROI: at r=0.5 blanket Haiku 4.5 hook, savings = $235, hook cost = $70, net = $165 (~$709/yr). r=0.75 with threshold >500 tok: savings = $215, hook cost = $26, net = $189 (~$812/yr).
7. Verdict: hook is net-positive but small. The first instinct ($3-5k savings) was wrong by 13× because of the JOIN dup. The real lever was elsewhere.

What this says about methodology: **always sanity-check the recomputed total before reporting attribution numbers**. A single bad join silently inflates everything downstream by an integer factor, and the result will still look plausible.
