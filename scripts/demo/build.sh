#!/usr/bin/env bash
# Build a small, anonymized DuckDB fixture from the real local transcript DB.
#
# Usage:
#   scripts/demo/build.sh                                # default paths
#   SRC_DB=/path/src.duckdb DEST_DB=/path/demo.duckdb scripts/demo/build.sh
#
# - Keeps only the session_ids listed in scripts/demo/sessions.txt (plus their
#   subagent transcripts).
# - Rewrites file paths, cwd, project dirs, branch, agent names to synthetic
#   values. All human-readable text columns (messages, thinking, tool I/O,
#   summaries, hooks, …) replaced with deterministic lorem-ipsum. JSON blobs
#   with free-form content are nulled/stubbed. Numeric columns (tokens, cost,
#   timings) and structural fields (session_id, uuid, tool_name, model,
#   timestamps) kept intact so the UI renders realistic layouts and charts.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"

SRC_DB="${SRC_DB:-${XDG_DATA_HOME:-$HOME/.local/share}/cct/transcripts.duckdb}"
DEST_DB="${DEST_DB:-$ROOT/out/demo.duckdb}"
SESSIONS_FILE="${SESSIONS_FILE:-$HERE/sessions.txt}"

if [[ ! -f "$SRC_DB" ]]; then
  echo "error: source DB not found: $SRC_DB" >&2
  exit 1
fi
if [[ ! -f "$SESSIONS_FILE" ]]; then
  echo "error: sessions file not found: $SESSIONS_FILE" >&2
  exit 1
fi

mkdir -p "$(dirname "$DEST_DB")"
rm -f "$DEST_DB" "$DEST_DB.wal"

# Build SQL list of session ids: 'id1','id2',...
SESSION_LIST="$(grep -v '^[[:space:]]*\(#\|$\)' "$SESSIONS_FILE" \
  | awk 'NF {printf "%s\x27%s\x27", (NR>1?",":""), $1}')"
if [[ -z "$SESSION_LIST" ]]; then
  echo "error: sessions.txt is empty" >&2
  exit 1
fi

echo "src:      $SRC_DB"
echo "dest:     $DEST_DB"
echo "sessions: $(grep -cv '^[[:space:]]*\(#\|$\)' "$SESSIONS_FILE")"

duckdb "$DEST_DB" <<SQL
ATTACH '$SRC_DB' AS src (READ_ONLY);

-- ── chosen sessions + files ──────────────────────────────────────────────
CREATE TEMP TABLE keep_sessions AS
  SELECT unnest([$SESSION_LIST]) AS session_id;

-- files belonging to the chosen sessions (parent transcripts)
CREATE TEMP TABLE keep_files AS
  SELECT DISTINCT file_path
  FROM src.transcripts
  WHERE session_id IN (SELECT session_id FROM keep_sessions)
     OR parent_session_id IN (SELECT session_id FROM keep_sessions);

-- entries in those files
CREATE TEMP TABLE keep_entries AS
  SELECT entry_id, file_path, session_id
  FROM src.entries
  WHERE file_path IN (SELECT file_path FROM keep_files);

-- every session_id that survived (includes subagents)
CREATE TEMP TABLE all_sessions AS
  SELECT DISTINCT session_id FROM src.transcripts
  WHERE file_path IN (SELECT file_path FROM keep_files);

-- ── scrub helpers ────────────────────────────────────────────────────────
-- Deterministic lorem-ipsum, length-bucketed by input size.
CREATE OR REPLACE TEMP MACRO fakepara(orig) AS
  CASE
    WHEN orig IS NULL THEN NULL
    WHEN length(orig) = 0 THEN ''
    ELSE trim(repeat(
      CASE abs(hash(orig)) % 4
        WHEN 0 THEN 'Lorem ipsum dolor sit amet, consectetur adipiscing elit. '
        WHEN 1 THEN 'Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris. '
        WHEN 2 THEN 'Duis aute irure dolor in reprehenderit in voluptate velit esse cillum. '
        ELSE 'Excepteur sint occaecat cupidatat non proident, sunt in culpa qui deserunt. '
      END,
      GREATEST(1, LEAST(20, CAST(length(orig)/80 AS INT)))
    ))
  END;

-- Rewrite /Users/<u>/.claude/projects/<slug>/<uuid>.jsonl → synthetic slug.
-- Stable per original project dir so all files in one project map together.
CREATE OR REPLACE TEMP MACRO fakepath(p) AS
  CASE
    WHEN p IS NULL THEN NULL
    WHEN regexp_matches(p, '\\.claude/projects/')
      THEN '/Users/demo/.claude/projects/-Users-demo-src-demo-app-'
        || (abs(hash(regexp_extract(p, '\\.claude/projects/([^/]+)/', 1))) % 5)::VARCHAR
        || regexp_replace(p, '^.*\\.claude/projects/[^/]+', '')
    ELSE '/Users/demo/unknown/' || regexp_extract(p, '([^/]+)$', 1)
  END;

CREATE OR REPLACE TEMP MACRO fakecwd(p) AS
  CASE
    WHEN p IS NULL THEN NULL
    ELSE '/Users/demo/src/demo-app-' || (abs(hash(p)) % 5)::VARCHAR
  END;

-- ── model_pricing: copy as-is (public pricing seed, not sensitive) ───────
CREATE TABLE model_pricing AS SELECT * FROM src.model_pricing;

-- ── transcripts ──────────────────────────────────────────────────────────
CREATE TABLE transcripts AS
SELECT
  fakepath(file_path)        AS file_path,
  session_id,
  is_subagent,
  agent_id,
  parent_session_id,
  entry_count,
  first_timestamp,
  last_timestamp,
  mtime,
  ingested_at
FROM src.transcripts
WHERE file_path IN (SELECT file_path FROM keep_files);

-- ── entries ──────────────────────────────────────────────────────────────
CREATE TABLE entries AS
SELECT
  entry_id,
  fakepath(file_path)        AS file_path,
  line_no,
  type,
  subtype,
  uuid,
  parent_uuid,
  logical_parent_uuid,
  is_sidechain,
  session_id,
  timestamp,
  user_type,
  entrypoint,
  fakecwd(cwd)               AS cwd,
  version,
  CASE WHEN git_branch IS NULL THEN NULL ELSE 'main' END AS git_branch,
  CASE WHEN slug IS NULL THEN NULL ELSE 'demo' END       AS slug,
  agent_id,
  CASE WHEN team_name IS NULL THEN NULL ELSE 'demo-team' END AS team_name,
  CASE WHEN agent_name IS NULL THEN NULL ELSE 'demo-agent' END AS agent_name,
  agent_color,
  prompt_id,
  is_meta,
  forked_from_uuid,
  forked_from_session_id
FROM src.entries
WHERE file_path IN (SELECT file_path FROM keep_files);

-- ── user_entries ─────────────────────────────────────────────────────────
CREATE TABLE user_entries AS
SELECT
  ue.entry_id,
  ue.message_role,
  fakepara(ue.message_content_text) AS message_content_text,
  ue.message_has_blocks,
  '{}'::JSON                        AS tool_use_result,
  ue.source_tool_assistant_uuid,
  ue.source_tool_use_id,
  ue.permission_mode,
  NULL::JSON                        AS origin,
  ue.is_compact_summary,
  ue.is_visible_in_transcript_only,
  '[]'::JSON                        AS image_paste_ids,
  fakepara(ue.plan_content)         AS plan_content
FROM src.user_entries ue
WHERE ue.entry_id IN (SELECT entry_id FROM keep_entries);

-- ── assistant_entries ────────────────────────────────────────────────────
CREATE TABLE assistant_entries AS
SELECT
  ae.entry_id,
  ae.message_id,
  ae.role,
  ae.model,
  NULL::JSON                        AS container,
  ae.stop_reason,
  ae.stop_sequence,
  NULL::JSON                        AS stop_details,
  NULL::JSON                        AS context_management,
  ae.request_id,
  ae.is_api_error_message,
  ae.api_error_status,
  fakepara(ae.error)                AS error,
  ae.tool_use_count,
  ae.cost_usd,
  ae.input_tokens,
  ae.output_tokens,
  ae.cache_creation_input_tokens,
  ae.cache_read_input_tokens,
  ae.cache_creation_5m,
  ae.cache_creation_1h,
  ae.web_search_requests,
  ae.web_fetch_requests,
  NULL::JSON                        AS service_tier,
  NULL::JSON                        AS inference_geo,
  NULL::JSON                        AS iterations,
  NULL::JSON                        AS speed,
  CASE WHEN ae.attribution_agent  IS NULL THEN NULL ELSE 'demo-agent'  END AS attribution_agent,
  CASE WHEN ae.attribution_plugin IS NULL THEN NULL ELSE 'demo-plugin' END AS attribution_plugin,
  CASE WHEN ae.attribution_skill  IS NULL THEN NULL ELSE 'demo-skill'  END AS attribution_skill,
  ae.cache_miss_reason_type,
  ae.cache_missed_input_tokens,
  ae.cost_per_tool_use
FROM src.assistant_entries ae
WHERE ae.entry_id IN (SELECT entry_id FROM keep_entries);

-- ── system_entries ───────────────────────────────────────────────────────
CREATE TABLE system_entries AS
SELECT
  se.entry_id,
  se.subtype,
  fakepara(se.content)              AS content,
  se.level,
  se.is_meta,
  NULL::JSON                        AS cause,
  NULL::JSON                        AS error,
  se.retry_in_ms,
  se.retry_attempt,
  se.max_retries,
  se.hook_count,
  NULL::JSON                        AS hook_errors,
  se.prevented_continuation,
  se.stop_reason,
  se.has_output,
  se.tool_use_id,
  se.duration_ms,
  se.message_count,
  NULL                              AS url,
  se.upgrade_nudge,
  se.compact_trigger,
  se.compact_pre_tokens,
  se.compact_post_tokens,
  se.compact_duration_ms,
  se.compact_preserved_head_uuid,
  se.compact_preserved_anchor_uuid,
  se.compact_preserved_tail_uuid,
  NULL::JSON                        AS compact_pre_discovered_tools
FROM src.system_entries se
WHERE se.entry_id IN (SELECT entry_id FROM keep_entries);

-- ── attachment_entries ───────────────────────────────────────────────────
CREATE TABLE attachment_entries AS
SELECT
  ae.entry_id,
  ae.attachment_type,
  CASE WHEN ae.hook_name IS NULL THEN NULL ELSE 'demo-hook' END AS hook_name,
  ae.tool_use_id,
  ae.hook_event,
  fakepara(ae.hook_content)         AS hook_content,
  fakepara(ae.hook_stdout)          AS hook_stdout,
  fakepara(ae.hook_stderr)          AS hook_stderr,
  ae.hook_exit_code,
  CASE WHEN ae.hook_command IS NULL THEN NULL ELSE 'demo-cmd' END AS hook_command,
  ae.hook_duration_ms,
  ae.decision,
  fakepath(ae.filename)             AS filename,
  fakepara(ae.file_content_text)    AS file_content_text,
  NULL::JSON                        AS file_content_metadata,
  fakepath(ae.display_path)         AS display_path,
  fakecwd(ae.directory_path)        AS directory_path,
  fakepara(ae.directory_content)    AS directory_content,
  NULL::JSON                        AS command_allowed_tools,
  ae.plan_reminder_type,
  ae.plan_is_sub_agent,
  fakepath(ae.plan_file_path)       AS plan_file_path,
  ae.plan_exists,
  fakepara(ae.skill_listing_content) AS skill_listing_content,
  ae.skill_listing_is_initial,
  ae.skill_listing_count,
  fakecwd(ae.skill_dir)             AS skill_dir,
  '["demo-skill"]'::JSON            AS skill_names,
  NULL::JSON                        AS invoked_skills,
  NULL::JSON                        AS task_reminder_content,
  ae.task_reminder_item_count,
  NULL::JSON                        AS diagnostics_files,
  ae.diagnostics_is_new,
  ae.date_change_new_date,
  NULL::JSON                        AS deferred_added_names,
  NULL::JSON                        AS deferred_added_lines,
  NULL::JSON                        AS deferred_removed_names,
  NULL::JSON                        AS mcp_added_names,
  NULL::JSON                        AS mcp_added_blocks,
  NULL::JSON                        AS mcp_removed_names,
  ae.ultrathink_level,
  fakepara(ae.queued_command_prompt) AS queued_command_prompt,
  ae.queued_command_mode,
  fakepath(ae.nested_memory_path)        AS nested_memory_path,
  ae.nested_memory_memory_type,
  fakepara(ae.nested_memory_content)     AS nested_memory_content,
  ae.nested_memory_differs_from_disk,
  NULL::JSON                             AS deferred_readded_names
FROM src.attachment_entries ae
WHERE ae.entry_id IN (SELECT entry_id FROM keep_entries);

-- ── progress_entries ─────────────────────────────────────────────────────
CREATE TABLE progress_entries AS
SELECT
  pe.entry_id,
  pe.parent_tool_use_id,
  pe.tool_use_id,
  pe.data_type,
  pe.hook_event,
  CASE WHEN pe.hook_name IS NULL THEN NULL ELSE 'demo-hook' END AS hook_name,
  CASE WHEN pe.command IS NULL THEN NULL ELSE 'demo-cmd' END    AS command,
  pe.agent_id,
  fakepara(pe.prompt)               AS prompt,
  NULL::JSON                        AS message,
  fakepara(pe.query)                AS query,
  pe.result_count,
  pe.elapsed_time_seconds,
  fakepara(pe.full_output)          AS full_output,
  fakepara(pe.output)               AS output,
  pe.timeout_ms,
  pe.total_lines,
  pe.total_bytes,
  pe.task_id,
  pe.server_name,
  pe.status,
  pe.tool_name,
  pe.elapsed_time_ms,
  fakepara(pe.task_description)     AS task_description,
  pe.task_type
FROM src.progress_entries pe
WHERE pe.entry_id IN (SELECT entry_id FROM keep_entries);

-- ── user_content_blocks ──────────────────────────────────────────────────
CREATE TABLE user_content_blocks AS
SELECT
  ucb.entry_id,
  ucb.position,
  ucb.block_type,
  fakepara(ucb.text)                AS text,
  ucb.tool_use_id,
  CASE
    WHEN ucb.tool_result_content IS NULL THEN NULL
    ELSE json_array(json_object('type', 'text', 'text', fakepara(ucb.tool_result_content::VARCHAR)))
  END                               AS tool_result_content,
  ucb.is_error,
  NULL::JSON                        AS image_source,
  NULL::JSON                        AS document_source,
  fakepara(ucb.document_title)      AS document_title
FROM src.user_content_blocks ucb
WHERE ucb.entry_id IN (SELECT entry_id FROM keep_entries);

-- ── assistant_content_blocks ─────────────────────────────────────────────
-- tool_input rebuilt per tool_name so Bash/Read/Grep cards still render.
CREATE TABLE assistant_content_blocks AS
SELECT
  acb.entry_id,
  acb.position,
  acb.block_type,
  fakepara(acb.text)                AS text,
  fakepara(acb.thinking)            AS thinking,
  NULL                              AS signature,
  NULL                              AS redacted_data,
  acb.tool_use_id,
  acb.tool_name,
  CASE
    WHEN acb.tool_input IS NULL THEN NULL
    WHEN acb.tool_name = 'Bash' THEN json_object('command', 'cargo test', 'description', 'Run tests')
    WHEN acb.tool_name IN ('Read','Edit','Write','NotebookEdit')
      THEN json_object('file_path', '/Users/demo/src/demo-app-0/src/lib.rs')
    WHEN acb.tool_name = 'Grep'
      THEN json_object('pattern', 'TODO', 'path', '/Users/demo/src/demo-app-0')
    WHEN acb.tool_name = 'Glob'
      THEN json_object('pattern', '**/*.rs')
    WHEN acb.tool_name = 'TodoWrite'
      THEN json_object('todos', json_array(
        json_object('content', fakepara('example task item'), 'status', 'pending', 'activeForm', 'Working')
      ))
    WHEN acb.tool_name = 'Task'
      THEN json_object('description', fakepara('subagent task'), 'prompt', fakepara(acb.tool_input::VARCHAR), 'subagent_type', 'general-purpose')
    WHEN acb.tool_name = 'WebFetch'
      THEN json_object('url', 'https://example.com', 'prompt', fakepara(acb.tool_input::VARCHAR))
    WHEN acb.tool_name = 'WebSearch'
      THEN json_object('query', 'example search query')
    ELSE json_object('placeholder', true)
  END                               AS tool_input,
  acb.caller_type
FROM src.assistant_content_blocks acb
WHERE acb.entry_id IN (SELECT entry_id FROM keep_entries);

-- ── assistant_usage_iterations (numeric only) ────────────────────────────
CREATE TABLE assistant_usage_iterations AS
SELECT * FROM src.assistant_usage_iterations
WHERE entry_id IN (SELECT entry_id FROM keep_entries);

-- ── system_hook_infos ────────────────────────────────────────────────────
CREATE TABLE system_hook_infos AS
SELECT entry_id, position, 'demo-hook' AS command, duration_ms
FROM src.system_hook_infos
WHERE entry_id IN (SELECT entry_id FROM keep_entries);

-- ── attachment_diagnostics_files ─────────────────────────────────────────
CREATE TABLE attachment_diagnostics_files AS
SELECT entry_id, position, fakepath(uri) AS uri, NULL::JSON AS diagnostics
FROM src.attachment_diagnostics_files
WHERE entry_id IN (SELECT entry_id FROM keep_entries);

-- ── attachment_invoked_skills ────────────────────────────────────────────
CREATE TABLE attachment_invoked_skills AS
SELECT entry_id, position, 'demo-skill' AS skill_name, NULL::JSON AS invocation_metadata
FROM src.attachment_invoked_skills
WHERE entry_id IN (SELECT entry_id FROM keep_entries);

-- ── metadata *_entries tables (filter by entry_id, scrub text) ───────────
CREATE TABLE permission_mode_entries AS
SELECT entry_id, permission_mode, session_id
FROM src.permission_mode_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE last_prompt_entries AS
SELECT entry_id, fakepara(last_prompt) AS last_prompt, leaf_uuid, session_id
FROM src.last_prompt_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE ai_title_entries AS
SELECT entry_id,
       'Demo session ' || (abs(hash(session_id)) % 1000)::VARCHAR AS ai_title,
       session_id
FROM src.ai_title_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE custom_title_entries AS
SELECT entry_id, fakepara(custom_title) AS custom_title, session_id
FROM src.custom_title_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE agent_name_entries AS
SELECT entry_id, 'demo-agent' AS agent_name, session_id
FROM src.agent_name_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE agent_color_entries AS
SELECT entry_id, agent_color, session_id
FROM src.agent_color_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE agent_setting_entries AS
SELECT entry_id, 'demo-setting' AS agent_setting, session_id
FROM src.agent_setting_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE tag_entries AS
SELECT entry_id, 'demo' AS tag, session_id
FROM src.tag_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE summary_entries AS
SELECT entry_id, leaf_uuid, fakepara(summary) AS summary, session_id
FROM src.summary_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE task_summary_entries AS
SELECT entry_id, fakepara(summary) AS summary, session_id, timestamp
FROM src.task_summary_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE pr_link_entries AS
SELECT entry_id, session_id, pr_number,
       'https://github.com/demo/demo/pull/' || pr_number::VARCHAR AS pr_url,
       'demo/demo' AS pr_repository, timestamp
FROM src.pr_link_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE mode_entries AS
SELECT entry_id, mode, session_id
FROM src.mode_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE worktree_state_entries AS
SELECT entry_id, session_id, NULL::JSON AS worktree_session
FROM src.worktree_state_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE content_replacement_entries AS
SELECT entry_id, session_id, NULL::JSON AS replacements, agent_id
FROM src.content_replacement_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE file_history_snapshot_entries AS
SELECT entry_id, message_id, NULL::JSON AS snapshot, is_snapshot_update
FROM src.file_history_snapshot_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE attribution_snapshot_entries AS
SELECT entry_id, message_id, surface, NULL::JSON AS file_states,
       prompt_count, prompt_count_at_last_commit,
       permission_prompt_count, permission_prompt_count_at_last_commit,
       escape_count, escape_count_at_last_commit
FROM src.attribution_snapshot_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE queue_operation_entries AS
SELECT entry_id, operation, timestamp, session_id, fakepara(content) AS content
FROM src.queue_operation_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE marble_origami_commit_entries AS
SELECT entry_id, session_id, collapse_id, summary_uuid,
       fakepara(summary_content) AS summary_content,
       fakepara(summary)         AS summary,
       first_archived_uuid, last_archived_uuid
FROM src.marble_origami_commit_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE marble_origami_snapshot_entries AS
SELECT entry_id, session_id, NULL::JSON AS staged, armed, last_spawn_tokens
FROM src.marble_origami_snapshot_entries WHERE entry_id IN (SELECT entry_id FROM keep_entries);

CREATE TABLE speculation_accept_entries AS
SELECT * FROM src.speculation_accept_entries
WHERE entry_id IN (SELECT entry_id FROM keep_entries);

-- ── views + indexes + PKs (must match run.rs post-ingest setup) ─────────
CREATE OR REPLACE VIEW assistant_entries_deduped AS
SELECT ae.*
FROM assistant_entries ae
JOIN entries e ON e.entry_id = ae.entry_id
QUALIFY ROW_NUMBER() OVER (
    PARTITION BY e.file_path, COALESCE(ae.message_id, 'anon-' || CAST(ae.entry_id AS TEXT))
    ORDER BY
        CASE WHEN ae.stop_reason IS NOT NULL THEN 0 ELSE 1 END,
        ae.output_tokens DESC NULLS LAST,
        ae.entry_id ASC
) = 1;

CREATE OR REPLACE VIEW tool_uses AS
SELECT
    entry_id, position, tool_use_id,
    tool_name AS name, tool_input AS input, caller_type,
    json_extract_string(tool_input, '\$.file_path')     AS input_file_path,
    json_extract_string(tool_input, '\$.notebook_path') AS input_notebook_path,
    json_extract_string(tool_input, '\$.path')          AS input_path,
    json_extract_string(tool_input, '\$.command')       AS input_command,
    COALESCE(
        json_extract_string(tool_input, '\$.file_path'),
        json_extract_string(tool_input, '\$.notebook_path'),
        json_extract_string(tool_input, '\$.path')
    ) AS effective_path,
    regexp_extract(
        COALESCE(
            json_extract_string(tool_input, '\$.file_path'),
            json_extract_string(tool_input, '\$.notebook_path'),
            json_extract_string(tool_input, '\$.path')),
        '\\.([^.]+)\$', 1
    ) AS file_ext
FROM assistant_content_blocks
WHERE block_type = 'tool_use';

ALTER TABLE transcripts   ADD PRIMARY KEY (file_path);
ALTER TABLE entries       ADD PRIMARY KEY (entry_id);
ALTER TABLE model_pricing ADD PRIMARY KEY (model);

CREATE INDEX IF NOT EXISTS idx_entries_session_id    ON entries(session_id);
CREATE INDEX IF NOT EXISTS idx_entries_timestamp     ON entries(timestamp);
CREATE INDEX IF NOT EXISTS idx_entries_type          ON entries(type);
CREATE INDEX IF NOT EXISTS idx_entries_parent_uuid   ON entries(parent_uuid);
CREATE INDEX IF NOT EXISTS idx_entries_file_path     ON entries(file_path);
CREATE INDEX IF NOT EXISTS idx_assistant_model       ON assistant_entries(model);
CREATE INDEX IF NOT EXISTS idx_assistant_cost        ON assistant_entries(cost_usd);
CREATE INDEX IF NOT EXISTS idx_assistant_block_tool  ON assistant_content_blocks(tool_name);
CREATE INDEX IF NOT EXISTS idx_attachment_type       ON attachment_entries(attachment_type);
CREATE INDEX IF NOT EXISTS idx_transcripts_session   ON transcripts(session_id);

-- ── sanity counts ────────────────────────────────────────────────────────
SELECT 'transcripts' AS t, count(*) FROM transcripts UNION ALL
SELECT 'entries',           count(*) FROM entries UNION ALL
SELECT 'assistant_entries', count(*) FROM assistant_entries UNION ALL
SELECT 'user_entries',      count(*) FROM user_entries UNION ALL
SELECT 'assistant_content_blocks', count(*) FROM assistant_content_blocks
ORDER BY 1;

DETACH src;
SQL

echo "built: $DEST_DB"
ls -lh "$DEST_DB"
