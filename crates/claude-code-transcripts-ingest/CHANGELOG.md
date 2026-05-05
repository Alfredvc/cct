# Changelog

All notable changes to this project will be documented in this file.

## [0.1.11] - 2026-05-05

### 🚀 Features

- *(transcripts)* Add AgentListingDelta attachment variant
- *(transcripts)* Add AutoMode and AutoModeExit attachment variants
- *(transcripts)* Add readdedNames on deferred_tools_delta attachment
- *(transcripts)* Add imagePasteIds on queued_command attachment
- *(transcripts)* Add PlanFileReference attachment variant
- *(ingest)* Add attribution and cache_miss_reason columns to assistant_entries
- *(ingest)* Populate attribution and cache_miss_reason columns from typed fields
- *(ingest)* Plumb api_error_status, plan_file_reference, readdedNames to DB
- *(serve)* Cost-decomposition flamegraph at /cost
- *(serve)* Replace /cost flamegraph with sorted-row drilldown
- *(web)* Redesign /cost page with editorial hero and unified hierarchy
- *(cct)* Add `report usage` and `extract sessions` subcommands

### 🐛 Bug Fixes

- *(build)* Re-run npm ci when package-lock.json changes

### ⚙️ Miscellaneous Tasks

- Rename repo to cct and reframe README
- Backfill v0.1.10 changelog section
- *(skills)* Rename claude-usage-db -> cct-db

## [0.1.10] - 2026-05-03

### 🚀 Features

- *(ingest)* Add `cct update` self-updater
- *(ingest)* Version_check module with cache, TTL, banner, gates, fetcher, public API
- *(ingest)* Call version_check from main

### 🐛 Bug Fixes

- *(ingest)* Mirror /releases/latest semantics in version_check fetcher
- *(ingest)* Address reviewer follow-ups for update-notifier
- *(ingest)* Use NamedTempFile in parse tests to eliminate pid+nanos collisions

### 📚 Documentation

- Document cct update-notifier banner

### ⚙️ Miscellaneous Tasks

- Release v0.1.10

## [0.1.9] - 2026-05-02

### 🚀 Features

- *(transcripts)* Support last-prompt leafUuid format
- *(transcripts)* Support hook_stopped_continuation, hook_system_message, todo_reminder attachments

### 🐛 Bug Fixes

- *(transcripts)* Tolerate optional hook duration and image-laden queued prompts

### 💼 Other

- Add tempfile workspace dev-dep for ingest tests

### 🚜 Refactor

- *(ingest)* Extract run_ingest with RunSummary for test-callability

### 🧪 Testing

- *(ingest)* Integration test for last_prompt format matrix

### ⚙️ Miscellaneous Tasks

- Address review nits
- Release v0.1.9

## [0.1.8] - 2026-04-17

### ⚙️ Miscellaneous Tasks

- Release v0.1.8

## [0.1.7] - 2026-04-17

### ⚙️ Miscellaneous Tasks

- Release v0.1.7

## [0.1.6] - 2026-04-17

### ⚙️ Miscellaneous Tasks

- Release v0.1.6

## [0.1.5] - 2026-04-17

### 🐛 Bug Fixes

- *(ci)* Pin toolchain to 1.95.0 and fix unnecessary_sort_by lint
- *(build)* Route all npm writes through OUT_DIR to fix cargo publish

### ⚙️ Miscellaneous Tasks

- Release v0.1.5

## [0.1.4] - 2026-04-17

### 🚀 Features

- *(web)* Vite + React migration with session-first transcripts UX
- *(session)* Transcripts → sessions rename + rich filter/sort panel
- *(ingest)* Recognize nested_memory attachment variant

### 💼 Other

- *(web)* Compact filter bar, sort beside summary, consistent subheaders

### 📚 Documentation

- Refresh viewer screenshots, add sessions-list panel

### ⚡ Performance

- *(serve)* Precompute session summary + LRU transcript cache
- *(ingest)* Materialize assistant_entries_deduped as table

### ⚙️ Miscellaneous Tasks

- Release v0.1.4

## [0.1.3] - 2026-04-17

### 🚀 Features

- *(pricing)* Explicit per-model rates + family fallback

### 🐛 Bug Fixes

- *(pricing)* Correct opus-4-6 rate, add opus-4-7 and sonnet-4-5

### ⚙️ Miscellaneous Tasks

- Release v0.1.3

## [0.1.2] - 2026-04-16

### 🚀 Features

- *(ui)* Consistent tool/agent cards, eager subagent preloading
- *(ui)* Full CSS polish — Inter/JetBrains Mono fonts, larger sizes, depth, grouped controls
- *(dashboard)* Add typical-week baseline bar for grounding
- *(dashboard)* Add token-stream cost split panel (main vs sidechain)
- *(dashboard)* Add artifact leaderboard (writes, agent prompts, tool results)
- *(dashboard)* Add context-size distribution + 200k+ session flag
- *(dashboard)* Top 1% most-expensive turns leaderboard
- *(dashboard)* Add two-regime time-series (volume vs per-session cost)
- *(dashboard)* Add first-turn cache-creation distribution
- *(dashboard)* Add mid-session cache-invalidation event analysis
- *(dashboard)* Add compaction events panel
- *(dashboard)* Add hour-of-day cost histogram
- *(dashboard)* Add hook frequency/duration panel
- *(dashboard)* Add MCP tool result size panel
- *(dashboard)* Add parent-model-at-spawn breakdown to agent panel
- *(dashboard)* Add top Reads by per-call size
- *(dashboard)* Add Bash longest + most-repeated leaderboards
- *(dashboard)* Add skill invocation stats
- *(dashboard)* Final Overview/Outliers layout + README updates
- *(dashboard)* Baseline shows per-week actuals + median + mean
- *(cct)* XDG-compliant default DB path + cct info subcommand
- *(dashboard)* Deep-link top turns to specific transcript entry
- *(timeline)* Show compact boundary events as cards

### 🐛 Bug Fixes

- *(ingest)* Move web/ into crate so publish verification works
- *(ingest)* Gracefully drop unknown serde variants instead of failing
- *(types)* Harden serde parsing against future transcript layout changes
- *(dashboard)* Fix URL sync stale project param + remove dead goHome callback
- *(dashboard)* Correct context-size formula + add Recharts null guards
- *(dashboard)* Add flex column gap to GlobalDashboard outer div for panel spacing
- *(dashboard)* Replace per-week actuals with titled median/mean baseline

### 🚜 Refactor

- *(dashboard)* Remove project filter, consolidate to single time-scoped dashboard

### 📚 Documentation

- Tighten skill blurbs, add viewer + release sections, move cct reference to crate README

### ⚙️ Miscellaneous Tasks

- Add git-cliff changelog automation
- Release v0.1.2

<!-- generated by git-cliff -->
