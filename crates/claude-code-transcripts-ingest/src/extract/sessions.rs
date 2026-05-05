//! `cct extract sessions` — structured per-turn session metadata.
//!
//! Heavy lifting (turn bucketing, aggregation) runs as DuckDB SQL; Rust just
//! issues queries, deserializes JSON, and emits the final document.

use std::collections::BTreeMap;
use std::path::Path;

use duckdb::{params_from_iter, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::ExtractSessionsArgs;
use crate::scope::Scope;

// ── Output shape ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct Output {
    project: Option<String>,
    generated_at: String,
    conversation_count: usize,
    session_count: usize,
    conversations: Vec<Conversation>,
}

#[derive(Debug, Serialize)]
struct Conversation {
    slug: Option<String>,
    sessions: Vec<Session>,
}

#[derive(Debug, Serialize)]
struct Session {
    session_id: String,
    file: String,
    is_continuation: bool,
    version: Option<String>,
    git_branch: Option<String>,
    start: Option<String>,
    end: Option<String>,
    turns: Vec<Value>,
}

// ── Entry point ──────────────────────────────────────────────────────────────

pub fn run(args: ExtractSessionsArgs) {
    if let Err(e) = run_inner(args) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run_inner(args: ExtractSessionsArgs) -> Result<(), String> {
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

    let sessions_meta = list_sessions(&conn, &scope, args.session.as_deref())?;

    let mut sessions: Vec<(Session, Option<String>)> = Vec::new();
    for m in sessions_meta {
        match build_session(&conn, &m) {
            Ok(Some(pair)) => sessions.push(pair),
            Ok(None) => {}
            Err(e) => eprintln!("warning: skipping {}: {e}", m.file_path),
        }
    }
    let conversations = group_conversations(sessions);
    let out = Output {
        project: scope.cwd_exact.clone(),
        generated_at: now_iso(),
        conversation_count: conversations.len(),
        session_count: conversations.iter().map(|c| c.sessions.len()).sum(),
        conversations,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn open_readonly(path: &Path) -> Result<Connection, String> {
    let cfg = duckdb::Config::default()
        .access_mode(duckdb::AccessMode::ReadOnly)
        .map_err(|e| format!("config: {e}"))?;
    Connection::open_with_flags(path, cfg).map_err(|e| format!("open {}: {e}", path.display()))
}

// ── Session listing ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct SessionMeta {
    file_path: String,
    session_id: String,
}

fn list_sessions(
    conn: &Connection,
    scope: &Scope,
    session_filter: Option<&str>,
) -> Result<Vec<SessionMeta>, String> {
    let mut sql = String::from(
        "SELECT t.file_path, t.session_id, MIN(e.timestamp) AS start_ts \
         FROM transcripts t JOIN entries e ON e.file_path = t.file_path \
         WHERE NOT t.is_subagent",
    );
    let mut params: Vec<String> = Vec::new();
    scope.append_where(&mut sql, &mut params);
    if let Some(s) = session_filter {
        sql.push_str(" AND t.session_id LIKE ? || '%'");
        params.push(s.to_string());
    }
    sql.push_str(" GROUP BY t.file_path, t.session_id ORDER BY start_ts");

    let mut stmt = conn.prepare(&sql).map_err(|e| format!("prepare: {e}"))?;
    let rows = stmt
        .query_map(params_from_iter(params.iter()), |r| {
            Ok(SessionMeta {
                file_path: r.get(0)?,
                session_id: r.get(1)?,
            })
        })
        .map_err(|e| format!("query: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("rows: {e}"))?;
    Ok(rows)
}

// ── Per-session build ────────────────────────────────────────────────────────

/// CTE pipeline that buckets every entry into a turn and emits one row per
/// turn with all aggregates pre-rolled into JSON. Used for both top-level
/// sessions and subagent transcripts.
const TURNS_SQL: &str = r#"
WITH marked AS (
  SELECT
    e.entry_id, e.type, e.timestamp, e.uuid,
    ue.message_content_text AS user_text,
    CASE
      WHEN e.type = 'user'
       AND ue.message_content_text IS NOT NULL
       AND ue.message_content_text NOT LIKE '<local-command%'
       AND ue.message_content_text NOT LIKE '<command-name>%'
       AND ue.message_content_text NOT LIKE '<command-message>%'
       AND ue.message_content_text NOT LIKE '<task-notification>%'
       AND ue.message_content_text NOT LIKE '<bash-input>%'
       AND ue.message_content_text NOT LIKE '<bash-stdout>%'
       AND ue.message_content_text NOT LIKE '<teammate-message%'
      THEN 1 ELSE 0
    END AS is_boundary
  FROM entries e
  LEFT JOIN user_entries ue ON ue.entry_id = e.entry_id
  WHERE e.file_path = ?
),
bucketed AS (
  SELECT *,
         SUM(is_boundary) OVER (ORDER BY entry_id ROWS UNBOUNDED PRECEDING) AS bucket
  FROM marked
),
-- 0-indexed turn id; rows before the first real-user message are bucket=0 → drop.
turn_rows AS (SELECT * FROM bucketed WHERE bucket >= 1),
turn_meta AS (
  SELECT
    (bucket - 1)::BIGINT AS idx,
    arg_min(uuid, entry_id)      FILTER (WHERE is_boundary = 1) AS uuid,
    CAST(MIN(timestamp)          FILTER (WHERE is_boundary = 1) AS VARCHAR) AS ts,
    arg_min(user_text, entry_id) FILTER (WHERE is_boundary = 1) AS user_text,
    CAST(epoch(MAX(timestamp)) - epoch(MIN(timestamp)) AS BIGINT) AS duration_seconds,
    BOOL_OR(type = 'user' AND user_text LIKE '[Request interrupted%') AS interrupted
  FROM turn_rows
  GROUP BY bucket
),
turn_tokens AS (
  SELECT (b.bucket - 1)::BIGINT AS idx,
         COUNT(*) AS api_calls,
         mode(d.model) AS model,
         COALESCE(SUM(d.input_tokens), 0)             AS input,
         COALESCE(SUM(d.cache_read_input_tokens), 0)  AS cache_read,
         COALESCE(SUM(d.cache_creation_5m), 0)
           + COALESCE(SUM(d.cache_creation_1h), 0)    AS cache_write,
         COALESCE(SUM(d.output_tokens), 0)            AS output
  FROM turn_rows b
  JOIN assistant_entries_deduped d ON d.entry_id = b.entry_id
  GROUP BY b.bucket
),
-- Tool calls: count by name per turn, exclude Skill (it's a skill, not a tool).
turn_tools_pre AS (
  SELECT (b.bucket - 1)::BIGINT AS idx, acb.tool_name AS name, COUNT(*) AS cnt
  FROM turn_rows b
  JOIN assistant_content_blocks acb ON acb.entry_id = b.entry_id
  WHERE acb.block_type = 'tool_use' AND acb.tool_name IS NOT NULL AND acb.tool_name <> 'Skill'
  GROUP BY 1, 2
),
turn_tools AS (
  SELECT idx, json_group_object(name, cnt) AS tools FROM turn_tools_pre GROUP BY idx
),
-- Skills come from three sources: model (Skill tool_use), CLI (<command-name>
-- tags inside user content), and attachments (batch-loaded skill listings).
-- We merge into a single deduped list per turn.
turn_skills_model AS (
  SELECT DISTINCT (b.bucket - 1)::BIGINT AS idx,
         json_extract_string(acb.tool_input, '$.skill') AS name,
         'model' AS source
  FROM turn_rows b
  JOIN assistant_content_blocks acb ON acb.entry_id = b.entry_id
  WHERE acb.block_type = 'tool_use' AND acb.tool_name = 'Skill'
    AND json_extract_string(acb.tool_input, '$.skill') IS NOT NULL
),
turn_skills_cli AS (
  SELECT DISTINCT (b.bucket - 1)::BIGINT AS idx,
         trim(both '/' FROM regexp_extract(b.user_text, '<command-name>(/?[^<]+)</command-name>', 1)) AS name,
         'cli' AS source
  FROM turn_rows b
  WHERE b.user_text LIKE '%<command-name>%'
    AND lower(regexp_extract(b.user_text, '<command-name>(/?[^<]+)</command-name>', 1)) NOT IN
        ('clear','model','compact','login','mcp','plugin','hooks','context','sandbox',
         'terminal-setup','help','config','permissions','cost','doctor','bug','init',
         'review','memory','status','fast','voice','logout','listen',
         '/clear','/model','/compact','/login','/mcp','/plugin','/hooks','/context','/sandbox',
         '/terminal-setup','/help','/config','/permissions','/cost','/doctor','/bug','/init',
         '/review','/memory','/status','/fast','/voice','/logout','/listen')
),
turn_skills_attach AS (
  SELECT DISTINCT (b.bucket - 1)::BIGINT AS idx, ais.skill_name AS name, 'attachment' AS source
  FROM turn_rows b
  JOIN attachment_invoked_skills ais ON ais.entry_id = b.entry_id
),
turn_skills_all AS (
  SELECT * FROM turn_skills_model
  UNION ALL SELECT * FROM turn_skills_cli
  UNION ALL SELECT * FROM turn_skills_attach
),
turn_skills AS (
  SELECT idx,
         json_group_array(json_object('name', name, 'source', source)) AS skills
  FROM (SELECT DISTINCT idx, name, source FROM turn_skills_all WHERE name IS NOT NULL AND name <> '')
  GROUP BY idx
),
turn_errors AS (
  SELECT (b.bucket - 1)::BIGINT AS idx,
         json_group_array(substring(coalesce(ucb.text, CAST(ucb.tool_result_content AS VARCHAR), ''), 1, 300)) AS errors
  FROM turn_rows b
  JOIN user_content_blocks ucb ON ucb.entry_id = b.entry_id
  WHERE ucb.is_error = TRUE
  GROUP BY b.bucket
),
-- Subagent launches: tool_use blocks named 'Agent' attribute to the launching turn.
turn_agents_pre AS (
  SELECT
    (b.bucket - 1)::BIGINT AS launch_idx,
    acb.entry_id AS launch_entry_id,
    json_extract_string(acb.tool_input, '$.description') AS description,
    acb.tool_use_id
  FROM turn_rows b
  JOIN assistant_content_blocks acb ON acb.entry_id = b.entry_id
  WHERE acb.block_type = 'tool_use' AND acb.tool_name = 'Agent'
)
SELECT
  tm.idx,
  tm.uuid,
  tm.ts,
  tm.user_text,
  tm.duration_seconds,
  COALESCE(tm.interrupted, FALSE) AS interrupted,
  COALESCE(tk.api_calls, 0) AS api_calls,
  tk.model,
  tk.input, tk.cache_read, tk.cache_write, tk.output,
  COALESCE(tt.tools, '{}'::JSON) AS tools,
  COALESCE(ts.skills, '[]'::JSON) AS skills,
  COALESCE(te.errors, '[]'::JSON) AS errors,
  COALESCE(
    (SELECT json_group_array(json_object(
       'launch_entry_id', launch_entry_id,
       'description', description,
       'tool_use_id', tool_use_id))
     FROM (SELECT * FROM turn_agents_pre p WHERE p.launch_idx = tm.idx
           ORDER BY p.launch_entry_id) ordered),
    '[]'::JSON
  ) AS agents
FROM turn_meta tm
LEFT JOIN turn_tokens tk ON tk.idx = tm.idx
LEFT JOIN turn_tools  tt ON tt.idx = tm.idx
LEFT JOIN turn_skills ts ON ts.idx = tm.idx
LEFT JOIN turn_errors te ON te.idx = tm.idx
ORDER BY tm.idx
"#;

#[derive(Debug, Deserialize)]
struct AgentLaunch {
    launch_entry_id: i64,
    description: Option<String>,
    tool_use_id: Option<String>,
}

fn build_session(
    conn: &Connection,
    meta: &SessionMeta,
) -> Result<Option<(Session, Option<String>)>, String> {
    // Top-of-session metadata: slug, version, git_branch, start, end,
    // first_user_text — fetched in one row, deconstructed below.
    type Head = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let head: Option<Head> = conn
        .query_row(
            "SELECT \
                MAX(slug) FILTER (WHERE slug IS NOT NULL), \
                MAX(version) FILTER (WHERE version IS NOT NULL), \
                MAX(git_branch) FILTER (WHERE git_branch IS NOT NULL), \
                CAST(MIN(timestamp) AS VARCHAR), \
                CAST(MAX(timestamp) AS VARCHAR), \
                arg_min((SELECT message_content_text FROM user_entries ue WHERE ue.entry_id = e.entry_id), \
                        e.entry_id) FILTER (WHERE e.type = 'user') \
             FROM entries e WHERE e.file_path = ?",
            [meta.file_path.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .ok();
    let (slug, version, git_branch, start, end, first_user) = head.unwrap_or_default();
    let is_continuation = first_user
        .as_deref()
        .map(|t| t.contains("continued from a previous conversation"))
        .unwrap_or(false);

    // Turn rows.
    let mut turns = query_turns(conn, &meta.file_path)?;
    if turns.is_empty() {
        return Ok(None);
    }

    // Resolve agent launches → subagent metadata (one query each; sessions
    // typically have a small number).
    let turn_count = turns.len();
    for turn in &mut turns {
        let agents_val = turn.as_object_mut().and_then(|o| o.remove("agents"));
        let launches: Vec<AgentLaunch> = match agents_val {
            Some(v) => serde_json::from_value(v).unwrap_or_default(),
            None => Vec::new(),
        };
        if launches.is_empty() {
            continue;
        }
        let mut subs: Vec<Value> = Vec::with_capacity(launches.len());
        for ag in launches {
            subs.push(resolve_agent(conn, meta, &ag, turn_count)?);
        }
        if let Some(o) = turn.as_object_mut() {
            o.insert("subagents".to_string(), Value::Array(subs));
        }
    }

    let session = Session {
        session_id: meta.session_id.clone(),
        file: meta.file_path.clone(),
        is_continuation,
        version,
        git_branch,
        start,
        end,
        turns,
    };
    Ok(Some((session, slug)))
}

fn query_turns(conn: &Connection, file_path: &str) -> Result<Vec<Value>, String> {
    let mut stmt = conn
        .prepare(TURNS_SQL)
        .map_err(|e| format!("prepare: {e}"))?;
    let rows = stmt
        .query_map([file_path], |r| {
            let mut obj = serde_json::Map::new();
            obj.insert("index".into(), Value::from(r.get::<_, i64>(0)?));
            obj.insert("uuid".into(), opt_string(r.get::<_, Option<String>>(1)?));
            obj.insert(
                "timestamp".into(),
                opt_string(r.get::<_, Option<String>>(2)?),
            );
            obj.insert(
                "user_text".into(),
                Value::String(r.get::<_, Option<String>>(3)?.unwrap_or_default()),
            );
            obj.insert(
                "duration_seconds".into(),
                opt_i64(r.get::<_, Option<i64>>(4)?),
            );
            obj.insert("interrupted".into(), Value::Bool(r.get::<_, bool>(5)?));
            obj.insert("api_calls".into(), Value::from(r.get::<_, i64>(6)?));
            obj.insert("model".into(), opt_string(r.get::<_, Option<String>>(7)?));
            let mut tokens = serde_json::Map::new();
            tokens.insert(
                "input".into(),
                Value::from(r.get::<_, Option<i64>>(8)?.unwrap_or(0)),
            );
            tokens.insert(
                "cache_read".into(),
                Value::from(r.get::<_, Option<i64>>(9)?.unwrap_or(0)),
            );
            tokens.insert(
                "cache_write".into(),
                Value::from(r.get::<_, Option<i64>>(10)?.unwrap_or(0)),
            );
            tokens.insert(
                "output".into(),
                Value::from(r.get::<_, Option<i64>>(11)?.unwrap_or(0)),
            );
            obj.insert("tokens".into(), Value::Object(tokens));
            obj.insert("tools".into(), parse_json_col(r.get::<_, String>(12)?));
            obj.insert("skills".into(), parse_json_col(r.get::<_, String>(13)?));
            obj.insert("errors".into(), parse_json_col(r.get::<_, String>(14)?));
            obj.insert("agents".into(), parse_json_col(r.get::<_, String>(15)?));
            Ok(Value::Object(obj))
        })
        .map_err(|e| format!("query: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("rows: {e}"))?;
    Ok(rows)
}

fn opt_string(v: Option<String>) -> Value {
    v.map(Value::String).unwrap_or(Value::Null)
}
fn opt_i64(v: Option<i64>) -> Value {
    v.map(Value::from).unwrap_or(Value::Null)
}
fn parse_json_col(s: String) -> Value {
    serde_json::from_str(&s).unwrap_or(Value::Null)
}

// ── Subagent resolution ──────────────────────────────────────────────────────

fn resolve_agent(
    conn: &Connection,
    parent: &SessionMeta,
    launch: &AgentLaunch,
    turn_count: usize,
) -> Result<Value, String> {
    // Resolve agent_id. Two cases:
    //   (a) sync launch — the tool_result for this tool_use_id carries
    //       "agentId: <id>" inside its content. Match directly on tool_use_id.
    //   (b) async launch — the result returns immediately ("scheduled") and
    //       the agentId arrives later in a free-form user message. Fall back
    //       to scanning the next 30 user entries.
    let tool_use_id = launch.tool_use_id.as_deref().unwrap_or("");
    let agent_id: Option<String> = conn
        .query_row(
            "SELECT regexp_extract(CAST(ucb.tool_result_content AS VARCHAR), \
                                  'agentId:\\s*([^\\s.\"\\\\]+)', 1) AS aid \
             FROM user_content_blocks ucb \
             JOIN entries e ON e.entry_id = ucb.entry_id \
             WHERE e.file_path = ? AND ucb.tool_use_id = ? \
               AND regexp_extract(CAST(ucb.tool_result_content AS VARCHAR), \
                                  'agentId:\\s*([^\\s.\"\\\\]+)', 1) <> '' \
             LIMIT 1",
            duckdb::params![parent.file_path.as_str(), tool_use_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
        .or_else(|| {
            conn.query_row(
                "SELECT regexp_extract(ue.message_content_text, \
                                       'agentId:\\s*([^\\s.]+)', 1) AS aid \
                 FROM entries e JOIN user_entries ue ON ue.entry_id = e.entry_id \
                 WHERE e.file_path = ? AND e.entry_id > ? AND e.entry_id <= ? + 30 \
                   AND e.type = 'user' \
                   AND regexp_extract(ue.message_content_text, \
                                      'agentId:\\s*([^\\s.]+)', 1) <> '' \
                 ORDER BY e.entry_id LIMIT 1",
                duckdb::params![
                    parent.file_path.as_str(),
                    launch.launch_entry_id,
                    launch.launch_entry_id,
                ],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
        });

    let mut out = serde_json::Map::new();
    out.insert("agent_id".into(), opt_string(agent_id.clone()));
    out.insert(
        "description".into(),
        Value::String(launch.description.clone().unwrap_or_default()),
    );

    let Some(aid) = agent_id else {
        out.insert("file".into(), Value::Null);
        out.insert("completed_in_turn".into(), Value::Null);
        return Ok(Value::Object(out));
    };

    // Locate completion turn via <task-id>{aid}</task-id> in user content.
    let completed_in_turn = find_completion_turn(conn, &parent.file_path, &aid, turn_count)?;
    out.insert("completed_in_turn".into(), opt_i64(completed_in_turn));

    // Subagent transcript file.
    let sa_path: Option<String> = conn
        .query_row(
            "SELECT file_path FROM transcripts \
             WHERE is_subagent AND parent_session_id = ? AND agent_id = ? LIMIT 1",
            [parent.session_id.as_str(), aid.as_str()],
            |r| r.get(0),
        )
        .ok();
    out.insert("file".into(), opt_string(sa_path.clone()));

    if let Some(path) = sa_path {
        let agg = subagent_aggregate(conn, &path)?;
        if let Some(o) = agg.as_object() {
            for (k, v) in o {
                out.insert(k.clone(), v.clone());
            }
        }
    }

    Ok(Value::Object(out))
}

fn find_completion_turn(
    conn: &Connection,
    file_path: &str,
    agent_id: &str,
    turn_count: usize,
) -> Result<Option<i64>, String> {
    // Find the user entry whose content carries `<task-id>{aid}</task-id>`,
    // then map to its turn by walking forward through turn_meta. Cheap path:
    // grab the entry_id of the matching user message and bucket it via the
    // same SQL turn-bucketing predicate.
    let target_entry_id: Option<i64> = conn
        .query_row(
            "SELECT MIN(e.entry_id) \
             FROM entries e LEFT JOIN user_entries ue ON ue.entry_id = e.entry_id \
             WHERE e.file_path = ? AND e.type = 'user' \
               AND ue.message_content_text LIKE ?",
            [file_path, &format!("%<task-id>{agent_id}</task-id>%")],
            |r| r.get(0),
        )
        .unwrap_or(None);
    let Some(eid) = target_entry_id else {
        return Ok(None);
    };

    let bucket: Option<i64> = conn
        .query_row(
            "WITH marked AS ( \
               SELECT e.entry_id, \
                 CASE WHEN e.type = 'user' \
                      AND ue.message_content_text IS NOT NULL \
                      AND ue.message_content_text NOT LIKE '<local-command%' \
                      AND ue.message_content_text NOT LIKE '<command-name>%' \
                      AND ue.message_content_text NOT LIKE '<command-message>%' \
                      AND ue.message_content_text NOT LIKE '<task-notification>%' \
                      AND ue.message_content_text NOT LIKE '<bash-input>%' \
                      AND ue.message_content_text NOT LIKE '<bash-stdout>%' \
                      AND ue.message_content_text NOT LIKE '<teammate-message%' \
                 THEN 1 ELSE 0 END AS is_boundary \
               FROM entries e \
               LEFT JOIN user_entries ue ON ue.entry_id = e.entry_id \
               WHERE e.file_path = ? AND e.entry_id <= ? \
             ) \
             SELECT SUM(is_boundary) - 1 FROM marked",
            duckdb::params![file_path, eid],
            |r| r.get(0),
        )
        .unwrap_or(None);

    // Sanity check against turn count.
    if let Some(b) = bucket {
        if b >= 0 && (b as usize) < turn_count {
            return Ok(Some(b));
        }
    }
    Ok(None)
}

const SUBAGENT_AGG_SQL: &str = r#"
WITH ts AS (
  SELECT MIN(e.timestamp) AS t0, MAX(e.timestamp) AS t1
  FROM entries e WHERE e.file_path = ?
),
toks AS (
  SELECT
    COUNT(*) AS api_calls,
    mode(d.model) AS model,
    COALESCE(SUM(d.input_tokens), 0) AS input,
    COALESCE(SUM(d.cache_read_input_tokens), 0) AS cache_read,
    COALESCE(SUM(d.cache_creation_5m), 0) + COALESCE(SUM(d.cache_creation_1h), 0) AS cache_write,
    COALESCE(SUM(d.output_tokens), 0) AS output
  FROM assistant_entries_deduped d
  JOIN entries e ON e.entry_id = d.entry_id
  WHERE e.file_path = ?
),
tools_pre AS (
  SELECT acb.tool_name AS name, COUNT(*) AS cnt
  FROM assistant_content_blocks acb JOIN entries e ON e.entry_id = acb.entry_id
  WHERE e.file_path = ? AND acb.block_type = 'tool_use'
    AND acb.tool_name IS NOT NULL AND acb.tool_name <> 'Skill'
  GROUP BY acb.tool_name
),
tools_obj AS (SELECT json_group_object(name, cnt) AS tools FROM tools_pre),
skills_all AS (
  SELECT DISTINCT json_extract_string(acb.tool_input, '$.skill') AS name, 'model' AS source
  FROM assistant_content_blocks acb JOIN entries e ON e.entry_id = acb.entry_id
  WHERE e.file_path = ? AND acb.block_type = 'tool_use' AND acb.tool_name = 'Skill'
    AND json_extract_string(acb.tool_input, '$.skill') IS NOT NULL
  UNION ALL
  SELECT DISTINCT ais.skill_name, 'attachment'
  FROM attachment_invoked_skills ais JOIN entries e ON e.entry_id = ais.entry_id
  WHERE e.file_path = ?
),
skills_arr AS (
  SELECT json_group_array(json_object('name', name, 'source', source)) AS skills
  FROM (SELECT DISTINCT name, source FROM skills_all WHERE name IS NOT NULL AND name <> '')
),
errs AS (
  SELECT json_group_array(substring(coalesce(ucb.text, CAST(ucb.tool_result_content AS VARCHAR), ''), 1, 300)) AS errors
  FROM user_content_blocks ucb JOIN entries e ON e.entry_id = ucb.entry_id
  WHERE e.file_path = ? AND ucb.is_error = TRUE
)
SELECT
  CAST(epoch(ts.t1) - epoch(ts.t0) AS BIGINT) AS duration_seconds,
  toks.api_calls, toks.model, toks.input, toks.cache_read, toks.cache_write, toks.output,
  COALESCE(tools_obj.tools, '{}'::JSON) AS tools,
  COALESCE(skills_arr.skills, '[]'::JSON) AS skills,
  COALESCE(errs.errors, '[]'::JSON) AS errors
FROM ts, toks, tools_obj, skills_arr, errs
"#;

fn subagent_aggregate(conn: &Connection, file_path: &str) -> Result<Value, String> {
    let mut stmt = conn
        .prepare(SUBAGENT_AGG_SQL)
        .map_err(|e| format!("prepare: {e}"))?;
    let row: Result<Value, _> = stmt.query_row(
        duckdb::params![file_path, file_path, file_path, file_path, file_path, file_path],
        |r| {
            let mut obj = serde_json::Map::new();
            obj.insert(
                "duration_seconds".into(),
                opt_i64(r.get::<_, Option<i64>>(0)?),
            );
            obj.insert("api_calls".into(), Value::from(r.get::<_, i64>(1)?));
            obj.insert("model".into(), opt_string(r.get::<_, Option<String>>(2)?));
            let mut toks = serde_json::Map::new();
            toks.insert(
                "input".into(),
                Value::from(r.get::<_, Option<i64>>(3)?.unwrap_or(0)),
            );
            toks.insert(
                "cache_read".into(),
                Value::from(r.get::<_, Option<i64>>(4)?.unwrap_or(0)),
            );
            toks.insert(
                "cache_write".into(),
                Value::from(r.get::<_, Option<i64>>(5)?.unwrap_or(0)),
            );
            toks.insert(
                "output".into(),
                Value::from(r.get::<_, Option<i64>>(6)?.unwrap_or(0)),
            );
            obj.insert("tokens".into(), Value::Object(toks));
            obj.insert("tools".into(), parse_json_col(r.get::<_, String>(7)?));
            obj.insert("skills".into(), parse_json_col(r.get::<_, String>(8)?));
            obj.insert("errors".into(), parse_json_col(r.get::<_, String>(9)?));
            Ok(Value::Object(obj))
        },
    );
    row.map_err(|e| format!("subagent agg: {e}"))
}

// ── Conversation grouping ────────────────────────────────────────────────────

fn group_conversations(sessions: Vec<(Session, Option<String>)>) -> Vec<Conversation> {
    let mut by_slug: BTreeMap<String, Vec<Session>> = BTreeMap::new();
    let mut no_slug: Vec<Session> = Vec::new();
    for (s, slug) in sessions {
        match slug {
            Some(slug) if !slug.is_empty() => by_slug.entry(slug).or_default().push(s),
            _ => no_slug.push(s),
        }
    }
    let mut out: Vec<Conversation> = Vec::new();
    for (slug, mut list) in by_slug {
        list.sort_by(|a, b| a.start.cmp(&b.start));
        out.push(Conversation {
            slug: Some(slug),
            sessions: list,
        });
    }
    for s in no_slug {
        out.push(Conversation {
            slug: None,
            sessions: vec![s],
        });
    }
    out.sort_by(|a, b| {
        let an = a.sessions.first().and_then(|s| s.start.clone());
        let bn = b.sessions.first().and_then(|s| s.start.clone());
        an.cmp(&bn)
    });
    out
}

// ── Time stamping (no chrono dep just for one ISO string) ────────────────────

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let h = (rem / 3600) as u32;
    let mi = ((rem % 3600) / 60) as u32;
    let se = (rem % 60) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let mut y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    if mo <= 2 {
        y += 1;
    }
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{se:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_buckets_by_slug() {
        let s = |id: &str, start: &str| Session {
            session_id: id.into(),
            file: "f".into(),
            is_continuation: false,
            version: None,
            git_branch: None,
            start: Some(start.into()),
            end: None,
            turns: vec![],
        };
        let convs = group_conversations(vec![
            (s("a", "2026-05-01"), Some("alpha".into())),
            (s("b", "2026-05-02"), Some("alpha".into())),
            (s("c", "2026-05-03"), None),
        ]);
        assert_eq!(convs.len(), 2);
        let alpha = convs
            .iter()
            .find(|c| c.slug.as_deref() == Some("alpha"))
            .unwrap();
        assert_eq!(alpha.sessions.len(), 2);
        assert_eq!(alpha.sessions[0].session_id, "a");
        let none = convs.iter().find(|c| c.slug.is_none()).unwrap();
        assert_eq!(none.sessions.len(), 1);
        assert_eq!(none.sessions[0].session_id, "c");
    }

    #[test]
    fn now_iso_format() {
        let s = now_iso();
        assert_eq!(s.len(), 20); // YYYY-MM-DDTHH:MM:SSZ
        assert!(s.ends_with('Z'));
    }
}
