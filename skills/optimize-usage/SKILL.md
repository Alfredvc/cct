---
name: optimize-usage
description: Investigate a user's Claude Code setup to find ROOT causes of spend and recommend cost optimizations. Use whenever the user asks to "reduce my Claude cost", "why is Claude so expensive", "optimize my usage", "analyze my spend", "audit my setup", "what's driving my bill", "cost insights", or any variant of diagnosing their Claude Code habits and configuration. This is a diagnostic methodology skill — it sits on top of `cct-db` (which covers raw SQL) and guides the investigation itself so the agent doesn't declare victory on shallow findings. Trigger even if the user only mentions part of this (e.g. "my Opus bill is high" or "are my skills expensive?") — partial cost questions still need the full root-cause discipline.
---

# Optimizing a user's Claude Code usage cost

## What this skill is for

Investigate a user's Claude Code setup (via the transcripts DuckDB at `~/.local/share/cct/transcripts.duckdb` by default) to find **root causes** of cost, then translate them into **behavior changes** the user can actually make. The goal is not a breakdown; the goal is to change their bill.

Category rollups ("cost by model", "cost by project") are trivially produced and trivially misleading. They say where money went, not why. The why lives in the workflow — which skills run, what artifacts they produce, how those artifacts propagate to subagents, how they sit in context for what fraction of a session, whether the cache prefix is being invalidated mid-session, which system-prompt bloat (MCP schemas, hooks, CLAUDE.md) gets re-cached every turn. That chain is what you must reconstruct.

This is methodology. It sits on `cct-db` for raw SQL — **read that first** for the billing-safety rule. Every cost aggregation must use `assistant_entries_deduped` with `message_id IS NOT NULL`, or numbers are ~2× too high.

## The core premise

Claude is very good at *testing* hypotheses given data; Claude is bad at *generating* them. The default failure pattern when you run this skill is: form one plausible hypothesis in the first query, spend the rest of the investigation confirming it, miss two or three bigger drivers that were equally visible.

Every phase of this skill is built around that premise. You will enumerate candidate hypotheses **before** you look at data; you will pre-register what each probe should show under each hypothesis; you will run a surprise gate that forces contradicting findings; you will generate rebuttals to your own conclusions. The DB is a discriminator, not an oracle.

---

## Failure modes to avoid

Top five inline. The rest — and expanded discussion — in `references/failure-modes.md`. Read that before Phase 4.

1. **Category-sum trap.** `SUM(cost) GROUP BY model/cwd/tool_name` tells you which bucket is expensive, not why. "Opus is 94% of spend" is useless alone — actionable question is _what is Opus being used for that could be downgraded_.

2. **Artifact blindness.** Treating tool calls as opaque counts. "Skill X invoked 91 times" is data. "Skill X produces a 30k-token artifact re-ingested by every downstream subagent" is a finding. Follow the artifact, including on the *result* side — a single `WebFetch` of a 100k-char page matters more than ten small reads.

3. **Mechanism-over-behavior.** The worst failure. After tracing cache boundaries, token-type taxonomies, and invalidation events, the agent presents the mechanism as the recommendation ("reduce cache-read tax by improving prefix stability"). The user cannot act on that. Every recommendation must be a behavior change the user does with their hands, drawn from the vocabulary in `references/behavior-vocabulary.md`. Mechanism explains *why*; it is never itself the recommendation.

4. **Confirmation over investigation.** Taking the user's framing ("I think it's the plans") and building evidence for it instead of stepping back to the mechanism space. User hints are symptoms, not diagnoses. Reframe to "what would have to be true about the system for that symptom to appear?" and enumerate alternatives.

5. **Wrong unit.** Users do not have a mental model of dollars or tokens — they pay a subscription and feel the cap, or they pay per-token and watch a dashboard. Either way, what they can judge is *how much of their current usage is this costing me*. Present every impact number as **% of the user's weekly usage** (denominator = their total spend in the recent active window). Never cite Anthropic plan names, plan caps, or $-thresholds to the user — those are internal heuristics for you, not facts. You may still compute in $ or tokens during investigation; you translate to % before presenting.

---

## Methodology

Five phases. Each has a gate that is evidence-based — passing requires citing specific files, entry IDs, or numbers, not asserting completion.

### Phase 1 — Survey, mechanism sweep, pre-commit hypotheses

This phase produces a **written hypothesis list before you touch aggregate SQL**. The whole point is to force breadth so the rest of the investigation is discrimination, not confirmation.

**1a. Silent fingerprint.** No interactive questions unless blocked.
- Confirm DB location and recency (`MAX(last_timestamp)` from `transcripts`).
- Read `~/.claude/CLAUDE.md` (global) and any `CLAUDE.md` in top-spend projects.
- List installed skills (`~/.claude/skills/`, plugin skill dirs, project-level `.claude/skills/`). Classify each: workflow-shaping (brainstorming, plan-writing, TDD, subagent-driven-development) vs content-shaping (frontend-design, caveman). Read the top 2–3 workflow-shaping skill bodies — they often mandate artifact production that drives cost.
- Enumerate `~/.claude/commands/*.md` sizes. Large slash commands invoked frequently are an invisible tax.
- Enumerate MCP servers (`~/.claude.json`, project `.mcp.json`). Every MCP tool's schema lives in the system prompt on every turn.
- Enumerate hooks (`settings.json` hook entries). PostToolUse/SessionStart hooks inject content per call.
- Compute the **baseline total usage** for the recent active window (sum of `cost_usd` across `assistant_entries_deduped` with `message_id IS NOT NULL` over the window). This is the denominator for every percentage you present. You may keep $-totals internally for math, but the user-facing unit is % of this baseline.

**1b. Mechanism taxonomy sweep.** The full taxonomy is below. Rate each row Likely / Unlikely / Unknown for this user based on Phase 1a findings alone (no SQL yet). This prevents missing whole categories.

| Mechanism | Signal in Phase 1a |
|---|---|
| Big CLAUDE.md (>3k tokens) | file size |
| Always-load skill bloat | skill count × avg SKILL.md size |
| MCP schema bloat | MCP server count, tool count |
| Hook-injected content per turn | hook configs |
| Large slash-command definitions | command file sizes |
| Plan/artifact blast radius | plan-producing skill present |
| Subagent-heavy workflow | subagent-driven skill present |
| Long-session workflow | conversational-style skills, no /clear discipline |
| 1M-context sessions (2× price above 200k) | env var / tier-hint / large attachments |
| Extended thinking tokens | thinking-budget env var |
| Screenshots / images | playwright / axe / design-review skills |
| External-LLM loops (codex MCP) | codex or external LLM tools |
| Autonomous loops / scheduled work | cron / loop / ScheduleWakeup usage |
| Parent-model inheritance on subagents | `/model opus` default + many Agent calls |
| Compaction cycles | long sessions, observed in Phase 2 |

For each "Likely" row, write a one-sentence mechanism hypothesis. For each "Unknown" row, note what would move it to Likely/Unlikely. Every "Unlikely" must have a reason.

**1c. Pre-commit hypothesis list.** Write ≥7 candidate root causes to a timestamped block, with predicted signal per hypothesis.

```
H1: <mechanism + proposed user-visible behavior>
    Predicted signal: <what probe/query would show this>
H2: ...
...
H7+: ...
```

Subsequent queries in Phase 2 must be labeled "testing H3" or "adding H8 (outside original list)". Do not delete entries later; mark them SUPPORTED / REFUTED / INCONCLUSIVE with the probe that decided.

**Gate:**
- You can describe the user's workflow in 2–3 sentences without querying the DB.
- Mechanism taxonomy has a rating on every row.
- Pre-commit list has ≥7 hypotheses, each with predicted signal.

If you cannot pass this gate from files alone, read more files. Only ask the user if still blocked.

### Phase 2 — Pre-registered outlier probes (not aggregate-first)

Aggregates come last, and only to rank outliers already found. First DB pass hunts **unusually large things** and tests the pre-commit hypotheses.

For every probe, before running: fill in a pre-registration block.

```
Probe: <name>
Tests: H1, H3 (pre-commit)
Expected under H1: <concrete signal, e.g. "top write path = **/plans/**, avg >5k tokens">
Expected under H3: <concrete signal>
Result that would refute both: <what you'd see>
```

Run. Record outcome. If the result matches neither, do **not** retrofit — add a new hypothesis to the list and re-probe.

**2a. Archetype dispatch (run first, 2–3 queries).** The three archetypes (plan-heavy, agent-team-heavy, long-session) are canned hypotheses with known behavioral fixes. Cheap signals triage which apply. See `references/archetypes.md` for symptom lists, probes, and levers.

Important nuance: an archetype matches if the signal is **distributional**, not if one session crosses a threshold. "12 sessions out of 50 have >200 turns" is not long-session archetype unless those 12 sessions account for a meaningful share of cost. Median and p90 matter more than any-cross.

A user can match zero, one, two, or all three archetypes. Archetype matches seed recommendations (pre-committed in Phase 5) but do not finish the investigation.

**2b. Required outlier probes.** Full SQL in `references/probes.md`. By investigative intent:

**Context composition (often overlooked)**
- First-turn cache-creation across fresh sessions → system-prompt size (CLAUDE.md + MCP schemas + tool list + hooks)
- MCP tool_result size distribution by tool-name prefix (`mcp__*`)
- Image/document content blocks per session
- Extended-thinking block size per turn

**Artifact outliers**
- Top 30 `Write` calls by content length
- Top 30 `Read` calls by file × read-count AND by per-call result size
- Longest `Agent` prompts (subagent briefings are often books)
- Longest bash commands and most repeated ones
- Largest tool_result payloads (Read, WebFetch, WebSearch, Bash, Grep)

**Session-level outliers**
- Top 1% most expensive individual assistant turns — pull raw JSONL line for top 5
- Session turn-count distribution AND session context-size distribution (fat-context ≠ long-session)
- Top 20 most expensive sessions — trace their workflow
- Sessions that crossed 200k tokens (1M-context 2× pricing)

**Two-regime session analysis** (volume vs per-session cost)
- Sessions per week AND avg/p90 cost per session over time
- Volume growth and per-session cost growth need different fixes

**Stream-separated token-type cost**
- Cost split across input / cache-creation-5m / cache-creation-1h / cache-read / output — separately for main-chain and sidechain
- Verify stream purity on this user's DB (cc1h could be main-only or sidechain-only depending on setup)
- Mechanism implications table in `references/probes.md`

**Cache-reset probe (TTL-aware)**
- Non-first main-chain turns with large cache-creation = prefix invalidation events
- Threshold should be the user's own p90 of cc per turn, not a hardcoded 20k
- Bucket by gap since previous turn AND by cache-type (cc5m vs cc1h):
  - cc5m with gap <5min = true invalidation
  - cc1h with gap 55–65min = TTL expiry, not invalidation
  - cc5m with gap 5–60min = session seam OR cc5m TTL expiry
- Compaction events (`summary_entries`) cause cold-cache spikes — identify separately

**Agent spawn model reality**
- Count `Agent` calls where `subagent_type` has explicit `model` vs inheritance
- Cross-reference: what was the parent's actual model at the spawn timestamp? Join subagent's first assistant_entry to observe executed model. Parent-model-at-spawn ≠ user's /model default.

**Hook-injected content**
- `system_hook_infos` count × average output size, grouped by hook name
- Correlate hook-heavy turns with cost

**Autonomous loops**
- Hour-of-day distribution; flag regularly-spaced spawns (fixed inter-turn intervals off-hours)

Only after outliers are logged, run aggregate rollups (model, project, tool, sidechain vs main, day) — to **rank** anomalies, not discover new ones.

**Gate:**
- Written list of ≥5 specific anomalies (file paths, patterns, session IDs, subagent types — not just bucket names).
- ≥2 of them **surprised you** (contradicted a Phase 1 prediction). If zero surprises, your probes are too narrow — query along axes you haven't touched (hour-of-day, weekday, permission_mode, git branch, file_ext, bash command prefix, first-turn vs non-first, skill-invoked vs not).
- Every probe in 2b was pre-registered with expected signals.

### Phase 3 — Trace to roots, verify currency, negative-baseline

For each anomaly, write a one-paragraph trace with **citations**:

- **Origin.** Which skill, slash command, subagent, CLAUDE.md rule, MCP server, or hook produces it? Quote the specific CLAUDE.md line / skill path / JSONL entry_id / config file. "I don't know what creates this" = keep digging.
- **Marginal cost per occurrence**, not just total. Two regimes — pick the right one:
  - **Spawn** (fresh ingestion, doesn't persist): cold-cache subagent, single tool result, one-off Read. A 30k-token artifact ≈ $0.56 cache-create per spawn on Opus. Cost = `cc × frequency`.
  - **Lifecycle** (persistent in the session prefix): CLAUDE.md, MCP schemas, hook outputs in transcript, skill bodies, plans loaded mid-session. Pay cache-create once + cache-read on every later turn the prefix survives. Use the `cc + Σ_{T'>T} cr` SQL pattern in [`../cct-db/references/cumulative-cost-analysis.md`](../cct-db/references/cumulative-cost-analysis.md). Cache-read tail dominates 5–10× over cc alone; quoting only cc undercounts.
- **Propagation.** Where does the artifact get re-ingested (subagents, reviewers, repeated Reads, compaction reincorporation)?
- **Substitute.** Would a pointer (path + line numbers) do the same job at 1/10 the size? Smaller model? Batched work? See `references/behavior-vocabulary.md` for the action space.
- **Currency.** Older data may implicate a skill the user uninstalled. Verify the cause appears in the **most recent active window**. Key the currency check on the *artifact pattern* (path glob, output shape), not the skill name — a user may still produce the same artifact via a different producer.

**Same-user negative baseline.** Pick the user's cheapest 10 non-trivial sessions (p20–p40 by cost, >20 turns) and diff against the most-expensive 10: skills invoked, CLAUDE.md versions active, models used, artifacts written. Mechanisms present in *both* are not the root cause. This diff eliminates confounders that archetype matching cannot.

**Gate:** every anomaly has a named origin with a quoted citation (file+line or entry_id) and a rough marginal-cost number. The negative-baseline diff is written out with at least one mechanism ruled out.

### Phase 4 — Falsify your own conclusions

Before presenting:

- **Selection bias.** For every "X costs more than Y", control for session length or turn count. If the delta survives, real. If it collapses, X is a proxy for hard tasks.
- **Falsification probe per hypothesis.** One query that could reject. "Before blaming skill X for cc1h spikes, check cc1h spikes in sessions where skill X was never invoked." If the spike appears there too, the hypothesis is wrong.
- **Adversarial rebuttal (required).** Write a 150-word rebuttal as a skeptical reviewer: "The real cause is probably X, which you didn't test because Y. Three hypotheses not in your H-list that could also explain the data: …" Add any survivors to the H-list and re-probe.
- **Inversion check.** If this user's bill were halved next week, what three things would be absent from the data? If your recommendations don't match that counterfactual, something's missing.
- **Compounding vs additive.** Two 30% savings do not equal 60%. Order and multiply.
- **Billing recheck.** Every number from `assistant_entries_deduped` with `message_id IS NOT NULL`? Redo any that weren't.

**Gate:** at least one place where you say "I can't rule out X" with a reason. At least one number changed from an earlier draft because of a falsification probe or deduped-view recheck (state which one, not a cosmetic rounding). Frictionless = shallow.

### Phase 5 — Translate to behavior, present with calibration

Every recommendation must be a **behavior** — a verb from `references/behavior-vocabulary.md`. Mechanisms are not recommendations. If your top recommendation reads like a system-architecture observation, rewrite it.

For each root cause, produce **three options at different effort levels** so the user has a choice — single config edits, habit shifts, and workflow redesigns all deserve representation. Users reject all-or-nothing pitches.

**Required shape per recommendation** (pattern-match your output against `references/exemplar.md`):

```
Action:        <verb from vocabulary> (e.g. "Trim global CLAUDE.md from 11k → <3k tokens")
Why it bites:  <one-sentence mechanism + the probe result that shows it>
Measured now:  <concrete threshold: "current p90 = 340 turns">
Target:        <concrete threshold: "end at 150 turns">
Saving:        X–Y% of weekly usage (adoption-adjusted)
Effort:        Low / Medium / High (one config edit / habit / workflow redesign)
Risk:          <tagged: capability loss / quality loss / habit friction / reversibility>
Verify:        <exact probe to re-run in 7 days + expected direction of change>
```

**Saving estimate formula (internal; report as %).** Pick the regime — spawn vs lifecycle, same split as Phase 3.

**Spawn cost** (per-call savings — subagent prompt size, repeated `Read` of the same file, single Bash compression):
```
raw_weekly_saving_usd = marginal_cost_per_occurrence × occurrences_in_recent_window
```

**Lifecycle cost** (persistent-injection savings — CLAUDE.md trim, MCP schema removal, hook-output gating, skill-body shrink):
```
raw_weekly_saving_usd = Σ_sessions_in_window ( cumulative_cost_of_trimmed_tokens_for_this_session )
```
The per-session cumulative term is the `cc + Σ cr` quantity from the SQL in [`../cct-db/references/cumulative-cost-analysis.md`](../cct-db/references/cumulative-cost-analysis.md) — set the injection event to the trimmed item, set `T` to the token reduction. Using the spawn formula on a persistent injection under-attributes by 5–10× because it omits the cache-read tail.

Both regimes then:
```
adjusted              = raw_weekly_saving_usd × adoption_rate
saving_percent        = 100 × adjusted / baseline_weekly_usd      ← this is what you present
```
Bracket ±40% or wider on the range. Never single-point. Adoption rates:
- Config change (one-time edit): ≈ 90%
- Habit change (session length, /clear discipline): ≈ 50%
- Workflow redesign (fork skill, restructure plans): ≈ 30%

**Unit rules — percentages of weekly usage, nothing else.**
- Every saving, every root-cause impact, every combined-total is a **percentage of the user's current weekly usage** (the baseline you computed in Phase 1).
- Format: whole-percent ranges (`8–12%`), or half-percent for small items (`1.5–2.5%`). No decimals below tenths.
- Do not present $, tokens, "hours freed", token counts, or plan-tier references in user-facing output. Those are investigative intermediates only.
- Bounds must differ meaningfully — if low and high round to the same whole percent, widen the range or write "<1% of weekly usage".
- Combined savings across multiple recommendations: compute as `1 − ∏(1 − s_i)` (they compound, not sum), and present as a range bracketing the adoption-rate variance. Combined is always a range.
- Absolute token and $ sizes of artifacts (e.g. "11k-token CLAUDE.md", "30k-token plan") are fine in the *explanation* of why something bites — they describe a thing, not an impact. The impact number is always %.

**Ordering.** Rank recommendations by **adoption-adjusted absolute dollar saving per week** (the `adjusted` number from the formula above — internal, never displayed). Biggest-fish-first. The user-facing display shows only the percentage-of-weekly-usage figure; the $ ordering happens in your head. Do not reorder by effort or by ease; a harder change that saves more weekly dollars ranks above an easier change that saves fewer. Let the user decide which to actually pick up — your job is to put the biggest impacts at the top.

---

### Output structure: two documents

The user wants a *decision-grade* read in chat plus a full audit trail they can dig into. Produce both.

1. **Chat summary** — the main response the user sees. Long enough to fully explain each prioritized recommendation; terse enough to skim. Roughly 40–70 lines is right. Format below.
2. **Full report** — written to `./claude-usage-report-YYYY-MM-DD.md` in the user's cwd (or the current project root if cwd isn't a good place; reuse the filename if a prior run wrote it). Everything the chat summary omits lives here.

**Chat summary template.** Per-recommendation blocks for the prioritized ones are substantial — they are how the user decides. The rest are named briefly so the user knows they exist.

```
## Usage audit — <active window, explicit dates>

Baseline: roughly steady at the current weekly level. Top drivers (% of weekly usage):
<driver A> ~X%, <driver B> ~Y%, <driver C> ~Z%. Full numbers and traces in the full
report path below.

## Prioritized recommendations

### 1. <Action verb + concrete target>
What to do: <1–2 sentences — the specific change, concrete enough to execute today>.
Why it bites: <1–2 sentences — mechanism + the measured evidence (e.g. "CLAUDE.md
is 11k tokens and sits in the prefix of every turn; p90 session is 340 turns")>.
Saving: ~X–Y% of weekly usage (adoption-adjusted). Effort: Low/Medium/High.
Risk: <capability/quality/habit/reversibility, 1 sentence>.
Verify in 7 days: <what probe to re-run and which direction the number should move>.

### 2. <Action verb + concrete target>
(same shape)

### 3. <Action verb + concrete target>
(same shape)

[4–5 if they're genuinely distinct actions; otherwise stop at 3]

**Combined if all prioritized adopted:** ~A–B% of weekly usage (compounded, not summed).

## Other recommendations in the full report

Briefer one-liners for the rest — enough for the user to know whether to open it:
- <action> — ~X% saving, <effort> effort
- <action> — ~X% saving, <effort> effort
- ...

## What I could not rule out
One or two sentences of the biggest open confounders (from Phase 4 adversarial rebuttal).

Full report: `./claude-usage-report-YYYY-MM-DD.md` — includes per-root-cause traces
with citations, falsification probes, two additional effort-tiered options per cause,
and methodology notes.
```

**Full report template** (write to markdown file):

```
# Claude Code usage report — <date>

## Current state
Recent active window (explicit dates). Baseline weekly usage (describe qualitatively:
"steady", "rising ~N% week over week", "burst followed by quiet"). Session volume
and per-session trajectory. Big buckets (model, project, tool) presented as context
for the reader, NOT as findings.

## Root causes, ranked by impact
For each:
  - what it is, where it originates (with quoted citation: file+line, skill path, or entry_id)
  - currency (still active? last seen when?)
  - share of weekly usage attributable (range, as %)
  - what would falsify this conclusion (from Phase 4)

## Recommendations
Three options per root cause (low / medium / high effort).
Each as a full block using the "Required shape per recommendation" template above.
Ordered by adoption-adjusted absolute dollar saving (biggest first). The displayed
saving per recommendation remains a % of weekly usage.

## What I could not rule out
Confounders and alternative explanations still open (from Phase 4 adversarial rebuttal).
Any hypothesis marked INCONCLUSIVE from the Phase 1 pre-commit list.

## What I did not investigate
Explicit. Projects skipped, periods ignored, data not looked at, assumptions I made.

## Methodology notes
Active window, queries run, hypotheses pre-committed in Phase 1 (with their final
SUPPORTED/REFUTED/INCONCLUSIVE verdicts), surprises from Phase 2 gate, falsification
probes from Phase 4.
```

Never present recommendations as certainties. Every estimate is a range. Every cause is "likely" unless the counter-argument was ruled out by data. The user knows their workflow better than you do — leave room to reject.

---

## Stop-signals in your own reasoning

If any of these thoughts surface, stop and recheck.

- **"The Opus share is huge, obvious issue."** → bucket. What's in it?
- **"Found the cause, writing recommendations."** → Phase 2 demanded ≥5 anomalies with ≥2 surprises.
- **"Standard cost-by-model query first."** → category trap. Pre-registered outliers first.
- **"It's probably long sessions."** → that's from the archetype list, did you check the distribution and rule out fat-context?
- **"User thinks X, data agrees, done."** → confirmation. Run the adversarial rebuttal.
- **"Close enough on the numbers."** → deduped view? `message_id IS NOT NULL`?
- **"CLAUDE.md / MCP server / hook output is N tokens, that's a small cc cost per session."** → cc-only quoting. Add the cache-read tail across all turns × all sessions in the window — `cc + Σ_{T'>T} cr` from `../cct-db/references/cumulative-cost-analysis.md`. Usually 5–10× the cc number for persistent injections.
- **"The root cause is cache-boundary invalidation / cache-read tax / token-type X."** → mechanism, not recommendation. What does the user *do*? Consult `references/behavior-vocabulary.md`.
- **"Pragmatic fix here would be…"** → stop. Over-simplifying. Re-check Phase 4.
- **"Fascinating pattern in the data."** → fascination is a tell. Re-anchor on behavior.

If your draft recommendation contains any of "cache boundary", "invalidation", "prefix", "tax", "token-type", "blast radius" — **stop and rewrite**. For each, apply: verb-first, then mechanism as "why":

- ❌ "Cache-read tax dominates (58% of spend)."
- ✅ "End sessions at ~150 turns. Why: long sessions re-pay cache-read on the full prefix every turn; current p90 is 340 turns, driving 58% of weekly spend."

---

## Reference files

- `references/probes.md` — every SQL query, billing-safe, with intent + mechanism implication
- `references/archetypes.md` — full archetype details (symptoms, probes, levers)
- `references/behavior-vocabulary.md` — closed list of user-actionable verbs + mechanism → behavior lookup
- `references/failure-modes.md` — extended failure-mode taxonomy (beyond the top 5 inline)
- `references/exemplar.md` — a full worked GOOD recommendation to pattern-match against

Read them when you hit the phase that references them, not preemptively.

---

## One lesson from a real investigation

In one audit, the agent ran cost-by-model, cost-by-project, cost-by-tool, declared "Opus on subagents, 49% of spend" as the root cause, and stopped. Real finding — not the biggest one. The user hinted at plans; a 10-minute artifact hunt found 175 plan files averaging 7k tokens (some 30k), re-ingested into 412 subagent sessions covering 61% of spend, with 2,927 wasted in-session re-reads.

The agent then jumped on "plan artifact blast radius" as the root cause — exactly the hint the user had dropped. User pushed back: *that is the symptom*. The mechanism was cache boundaries = payment boundaries, with multiple independent triggers (mid-session invalidations, subagent cold-cache spawns, long-session cache-read tax).

Then the agent presented the mechanism as the finding: "invalidation events account for $229/week." The user's actual answer was blunt and behavioral: **sessions are too long, plans are too long**. Both were visible in the data from the first hour.

Three lessons, which Phases 1, 3, and 5 of this skill exist to enforce: (1) pre-commit a wide hypothesis list before category aggregates blind you; (2) user hints are symptoms, reframe to mechanism; (3) translate mechanism back into behavior — the user needs an instruction, not an architecture diagram.

The biggest cost drivers in a Claude Code setup are rarely obvious. Enumerate the mechanism space, pre-commit, probe with pre-registration, trace with citations, falsify, translate.
