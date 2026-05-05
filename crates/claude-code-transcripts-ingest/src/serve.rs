//! HTTP server for the Claude Code transcript viewer.
//!
//! Reads from a DuckDB database built by `claude-code-transcripts-ingest ingest`.
//! Serves the embedded `web/index.html` and a JSON API backed by the DB.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use axum::{
    body::Bytes,
    extract::{Path as AxumPath, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};
use duckdb::Connection;
use include_dir::{include_dir, Dir};
use lru::LruCache;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::task::spawn_blocking;

use crate::cli::ServeArgs;
use crate::cost_decomp::{self, DecompResult};

// ── Static web bundle (built by Vite, embedded at compile time) ───────────────

static WEB_DIST: Dir<'_> = include_dir!("$OUT_DIR/web/dist");

// ── State ─────────────────────────────────────────────────────────────────────

type TranscriptKey = (String, Option<String>);

const TRANSCRIPT_CACHE_CAP: usize = 256;
const REFRESH_POLL_SECS: u64 = 5;

#[derive(Clone)]
struct AppState {
    db_path: String,
    db: Arc<Mutex<Connection>>,
    summary: Arc<RwLock<Arc<SessionSummary>>>,
    transcript_cache: Arc<Mutex<LruCache<TranscriptKey, Bytes>>>,
    decomp: Arc<RwLock<Option<Arc<DecompResult>>>>,
}

struct SessionSummary {
    rows: Vec<SessionRowCache>,
    tools: Vec<String>,
    earliest: Option<String>,
    latest: Option<String>,
    projects: Vec<ProjectRowCache>,
}

struct SessionRowCache {
    id: String,
    project: String,
    started_at: Option<String>,
    last_active: Option<String>,
    first_ms: Option<i64>,
    last_ms: Option<i64>,
    cost_usd: f64,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    total_tokens: i64,
    has_subagents: bool,
    tools: Vec<String>,
}

struct ProjectRowCache {
    key: String,
    display: String,
    session_count: i64,
}

fn open_db(db_path: &str) -> Result<Connection, String> {
    Connection::open(db_path).map_err(|e| format!("open {db_path}: {e}"))
}

fn parse_iso_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.timestamp_millis())
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|ndt| ndt.and_utc().timestamp_millis())
        })
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|ndt| ndt.and_utc().timestamp_millis())
        })
}

fn json_bytes_response(bytes: Bytes) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], bytes).into_response()
}

// ── display_name ──────────────────────────────────────────────────────────────

fn home_dir() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default()
}

fn home_key() -> String {
    home_dir()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn find_real_path(base: &Path, suffix: &str) -> Option<PathBuf> {
    if suffix.is_empty() {
        return Some(base.to_path_buf());
    }
    if !suffix.starts_with('-') {
        return None;
    }
    let parts: Vec<&str> = suffix[1..].split('-').collect();
    for n in 1..=parts.len() {
        let name = parts[..n].join("-");
        let candidate = base.join(&name);
        let remaining = if n < parts.len() {
            format!("-{}", parts[n..].join("-"))
        } else {
            String::new()
        };
        if candidate.exists() {
            if remaining.is_empty() {
                return Some(candidate);
            }
            if let Some(r) = find_real_path(&candidate, &remaining) {
                return Some(r);
            }
        }
    }
    None
}

fn display_name(key: &str) -> String {
    let hk = home_key();
    if key.starts_with(&hk) {
        let suffix = &key[hk.len()..];
        let home = PathBuf::from(home_dir());
        if let Some(real) = find_real_path(&home, suffix) {
            if let Ok(rel) = real.strip_prefix(&home) {
                return format!("~/{}", rel.display());
            }
        }
        return format!("~{}", suffix.replace('-', "/"));
    }
    format!("/{}", key.replace('-', "/").trim_start_matches('/'))
}

// ── Text helpers ──────────────────────────────────────────────────────────────

fn extract_tool_result_text(json_str: &str) -> String {
    match serde_json::from_str::<Value>(json_str) {
        Ok(Value::String(s)) => s,
        Ok(Value::Array(arr)) => arr
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text").and_then(|t| t.as_str()).map(str::to_owned)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn summarize_input(name: &str, input: &Value) -> String {
    let short = |p: &str| -> String {
        let parts: Vec<&str> = p.split('/').collect();
        if parts.len() >= 2 {
            parts[parts.len() - 2..].join("/")
        } else {
            p.to_owned()
        }
    };

    match name {
        "Read" => {
            if let Some(fp) = input.get("file_path").and_then(|v| v.as_str()) {
                let suffix = input
                    .get("limit")
                    .and_then(|v| v.as_i64())
                    .map(|n| format!(", {n} lines"))
                    .unwrap_or_default();
                return format!("Read({}{})", short(fp), suffix);
            }
        }
        "Write" => {
            if let Some(fp) = input.get("file_path").and_then(|v| v.as_str()) {
                let lines = input
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|c| c.chars().filter(|&c| c == '\n').count() + 1)
                    .unwrap_or(0);
                return format!("Write({}, {lines} lines)", short(fp));
            }
        }
        "Edit" | "MultiEdit" => {
            if let Some(fp) = input.get("file_path").and_then(|v| v.as_str()) {
                let lines = input
                    .get("old_string")
                    .and_then(|v| v.as_str())
                    .map(|s| s.chars().filter(|&c| c == '\n').count() + 1)
                    .unwrap_or(0);
                return format!("{name}({}, {lines} lines)", short(fp));
            }
        }
        _ => {}
    }

    let val = [
        "file_path",
        "pattern",
        "description",
        "command",
        "prompt",
        "query",
        "old_string",
        "skill",
        "subject",
        "path",
    ]
    .iter()
    .find_map(|k| {
        input
            .get(k)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    });

    if let Some(v) = val {
        let s: String = v.chars().take(55).collect();
        let ell = if v.len() > 55 { "…" } else { "" };
        return format!("{}({}{})", name, s.replace('\n', " "), ell);
    }

    name.to_owned()
}

// ── Error helper ──────────────────────────────────────────────────────────────

fn err500(msg: impl std::fmt::Display) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, msg.to_string()).into_response()
}

// ── Handlers ──────────────────────────────────────────────────────────────────

fn file_response(path: &str) -> Response {
    let file = match WEB_DIST.get_file(path) {
        Some(f) => f,
        None => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_owned()),
            (
                header::CACHE_CONTROL,
                "public, max-age=31536000, immutable".to_owned(),
            ),
        ],
        file.contents(),
    )
        .into_response()
}

async fn serve_index() -> Response {
    let file = match WEB_DIST.get_file("index.html") {
        Some(f) => f,
        None => return (StatusCode::INTERNAL_SERVER_ERROR, "index.html missing").into_response(),
    };
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        file.contents(),
    )
        .into_response()
}

async fn serve_asset(AxumPath(path): AxumPath<String>) -> Response {
    file_response(&format!("assets/{path}"))
}

async fn api_sessions_meta(State(state): State<AppState>) -> Response {
    let summary = state.summary.read().map(|g| g.clone());
    match summary {
        Ok(s) => Json(json!({
            "earliest": s.earliest,
            "latest":   s.latest,
            "tools":    s.tools,
        }))
        .into_response(),
        Err(e) => err500(e),
    }
}

async fn api_projects(State(state): State<AppState>) -> Response {
    let summary = state.summary.read().map(|g| g.clone());
    match summary {
        Ok(s) => {
            let out: Vec<Value> = s
                .projects
                .iter()
                .map(|p| {
                    json!({
                        "key":          p.key,
                        "display":      p.display,
                        "sessionCount": p.session_count,
                    })
                })
                .collect();
            Json(Value::Array(out)).into_response()
        }
        Err(e) => err500(e),
    }
}

// Precomputed session summary: computed once at startup, refreshed by the
// mtime-poll task when the DB file changes. All of `/api/sessions`,
// `/api/projects`, `/api/sessions/meta` serve from this — zero DB work per
// request after startup.
fn compute_summary(conn: &Connection) -> Result<SessionSummary, String> {
    let sql = "WITH sessions AS ( \
             SELECT \
               t1.session_id, \
               t1.file_path, \
               t1.first_timestamp, \
               t1.last_timestamp, \
               regexp_extract(t1.file_path, '.*/projects/([^/]+)/[^/]+\\.jsonl$', 1) AS project_key, \
               EXISTS(SELECT 1 FROM transcripts t2 WHERE t2.parent_session_id = t1.session_id) AS has_subagents \
             FROM transcripts t1 \
             WHERE NOT t1.is_subagent \
           ), \
           tok AS ( \
             SELECT e.file_path, \
                    SUM(d.cost_usd)                    AS cost_usd, \
                    SUM(d.input_tokens)                AS input_tokens, \
                    SUM(d.output_tokens)               AS output_tokens, \
                    SUM(d.cache_read_input_tokens)     AS cache_read_tokens, \
                    SUM(d.cache_creation_input_tokens) AS cache_creation_tokens \
             FROM entries e JOIN assistant_entries_deduped d ON d.entry_id = e.entry_id \
             WHERE d.message_id IS NOT NULL \
             GROUP BY e.file_path \
           ), \
           tl AS ( \
             SELECT e.file_path, LIST(DISTINCT acb.tool_name) AS tool_names \
             FROM entries e JOIN assistant_content_blocks acb \
               ON acb.entry_id = e.entry_id \
              AND acb.block_type = 'tool_use' \
              AND acb.tool_name IS NOT NULL \
             GROUP BY e.file_path \
           ) \
           SELECT s.session_id, s.project_key, \
                  CAST(s.first_timestamp AS VARCHAR), \
                  CAST(s.last_timestamp  AS VARCHAR), \
                  epoch_ms(s.first_timestamp), \
                  epoch_ms(s.last_timestamp), \
                  COALESCE(ROUND(tok.cost_usd, 6), 0.0), \
                  COALESCE(tok.input_tokens, 0), \
                  COALESCE(tok.output_tokens, 0), \
                  COALESCE(tok.cache_read_tokens, 0), \
                  COALESCE(tok.cache_creation_tokens, 0), \
                  s.has_subagents, \
                  COALESCE(to_json(tl.tool_names), '[]') \
           FROM sessions s \
           LEFT JOIN tok ON tok.file_path = s.file_path \
           LEFT JOIN tl  ON tl.file_path  = s.file_path";

    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows_iter = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, f64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, bool>(11)?,
                row.get::<_, String>(12)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut rows: Vec<SessionRowCache> = Vec::new();
    let mut tools_set: BTreeSet<String> = BTreeSet::new();
    let mut earliest_ms: Option<i64> = None;
    let mut latest_ms: Option<i64> = None;
    let mut earliest_str: Option<String> = None;
    let mut latest_str: Option<String> = None;
    // project_key -> (session_count, latest_last_ms)
    let mut proj_counts: HashMap<String, (i64, Option<i64>)> = HashMap::new();

    for row in rows_iter.filter_map(|r| r.ok()) {
        let (
            id,
            project,
            started_at,
            last_active,
            first_ms,
            last_ms,
            cost_usd,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            has_subagents,
            tools_json,
        ) = row;
        let id = id.unwrap_or_default();
        let project = project.unwrap_or_default();
        let tools: Vec<String> = serde_json::from_str(&tools_json).unwrap_or_default();
        for t in &tools {
            tools_set.insert(t.clone());
        }
        let total_tokens = input_tokens + output_tokens + cache_read_tokens + cache_creation_tokens;

        if let Some(fm) = first_ms {
            if earliest_ms.is_none_or(|e| fm < e) {
                earliest_ms = Some(fm);
                earliest_str = started_at.clone();
            }
        }
        if let Some(lm) = last_ms {
            if latest_ms.is_none_or(|l| lm > l) {
                latest_ms = Some(lm);
                latest_str = last_active.clone();
            }
        }

        if !project.is_empty() {
            let entry = proj_counts.entry(project.clone()).or_insert((0, None));
            entry.0 += 1;
            if let Some(lm) = last_ms {
                if entry.1.is_none_or(|e| lm > e) {
                    entry.1 = Some(lm);
                }
            }
        }

        rows.push(SessionRowCache {
            id,
            project,
            started_at,
            last_active,
            first_ms,
            last_ms,
            cost_usd,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            total_tokens,
            has_subagents,
            tools,
        });
    }

    // Order projects by latest activity DESC, matching the old SQL.
    let mut proj_vec: Vec<(String, i64, Option<i64>)> = proj_counts
        .into_iter()
        .map(|(k, (c, m))| (k, c, m))
        .collect();
    proj_vec.sort_by_key(|b| std::cmp::Reverse(b.2));
    let projects: Vec<ProjectRowCache> = proj_vec
        .into_iter()
        .map(|(k, c, _)| ProjectRowCache {
            display: display_name(&k),
            key: k,
            session_count: c,
        })
        .collect();

    Ok(SessionSummary {
        rows,
        tools: tools_set.into_iter().collect(),
        earliest: earliest_str,
        latest: latest_str,
        projects,
    })
}

#[derive(Deserialize, Default)]
struct SessionsQ {
    /// Comma-separated project keys. Empty/missing = all.
    project: Option<String>,
    /// ISO8601 / RFC3339 inclusive lower bound on session last_timestamp.
    #[serde(rename = "tStart")]
    t_start: Option<String>,
    /// Upper bound on session first_timestamp.
    #[serde(rename = "tEnd")]
    t_end: Option<String>,
    /// any | yes | no
    subagents: Option<String>,
    /// Comma-separated tool names (match if session used ANY).
    tools: Option<String>,
    /// cost | tokens | started | last
    sort: Option<String>,
    /// asc | desc
    order: Option<String>,
}

fn split_csv(s: &Option<String>) -> Vec<String> {
    match s {
        None => Vec::new(),
        Some(v) => v
            .split(',')
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect(),
    }
}

// Filters + sorts the precomputed session summary in memory. Zero DB work.
async fn api_sessions(State(state): State<AppState>, Query(q): Query<SessionsQ>) -> Response {
    let projects = split_csv(&q.project);
    let tools = split_csv(&q.tools);
    let t_start_ms = q
        .t_start
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(parse_iso_ms);
    let t_end_ms = q
        .t_end
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(parse_iso_ms);

    let subagent_filter: Option<bool> = match q.subagents.as_deref() {
        Some("yes") => Some(true),
        Some("no") => Some(false),
        _ => None,
    };

    let sort = q.sort.as_deref().unwrap_or("last").to_owned();
    let asc = q.order.as_deref() == Some("asc");

    let summary = match state.summary.read() {
        Ok(g) => g.clone(),
        Err(e) => return err500(e),
    };

    let project_set: HashSet<&str> = projects.iter().map(String::as_str).collect();
    let tool_set: HashSet<&str> = tools.iter().map(String::as_str).collect();

    let mut filtered: Vec<&SessionRowCache> = summary
        .rows
        .iter()
        .filter(|r| {
            if !project_set.is_empty() && !project_set.contains(r.project.as_str()) {
                return false;
            }
            if let Some(ts) = t_start_ms {
                if r.last_ms.is_none_or(|m| m < ts) {
                    return false;
                }
            }
            if let Some(te) = t_end_ms {
                if r.first_ms.is_none_or(|m| m > te) {
                    return false;
                }
            }
            if let Some(want) = subagent_filter {
                if r.has_subagents != want {
                    return false;
                }
            }
            if !tool_set.is_empty() && !r.tools.iter().any(|t| tool_set.contains(t.as_str())) {
                return false;
            }
            true
        })
        .collect();

    filtered.sort_by(|a, b| {
        let cmp = match sort.as_str() {
            "cost" => a
                .cost_usd
                .partial_cmp(&b.cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal),
            "tokens" => a.total_tokens.cmp(&b.total_tokens),
            "started" => a.first_ms.cmp(&b.first_ms),
            _ => a.last_ms.cmp(&b.last_ms),
        };
        if asc {
            cmp
        } else {
            cmp.reverse()
        }
    });

    let out: Vec<Value> = filtered
        .iter()
        .map(|r| {
            json!({
                "id":                  r.id,
                "project":             r.project,
                "startedAt":           r.started_at,
                "lastActive":          r.last_active,
                "costUsd":             r.cost_usd,
                "inputTokens":         r.input_tokens,
                "outputTokens":        r.output_tokens,
                "cacheReadTokens":     r.cache_read_tokens,
                "cacheCreationTokens": r.cache_creation_tokens,
                "totalTokens":         r.total_tokens,
                "hasSubagents":        r.has_subagents,
                "tools":               r.tools,
            })
        })
        .collect();

    Json(Value::Array(out)).into_response()
}

#[derive(Deserialize)]
struct TranscriptQ {
    #[allow(dead_code)]
    project: Option<String>,
    session: Option<String>,
}

// LRU-cached timeline build. Second visit to the same session returns cached
// serialized JSON bytes (no DB work, no re-serialization).
async fn api_transcript(State(state): State<AppState>, Query(q): Query<TranscriptQ>) -> Response {
    let session = q.session.unwrap_or_default();
    if session.is_empty() {
        return (StatusCode::BAD_REQUEST, "session required").into_response();
    }
    let key: TranscriptKey = (session.clone(), None);

    if let Some(bytes) = state
        .transcript_cache
        .lock()
        .ok()
        .and_then(|mut c| c.get(&key).cloned())
    {
        return json_bytes_response(bytes);
    }

    let db = state.db.clone();
    let cache = state.transcript_cache.clone();
    let result = spawn_blocking(move || -> Result<Bytes, String> {
        let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
        let fp = session_file_path(&conn, &session, false, None)?;
        let v = build_timeline(&conn, &fp, false)?;
        let bytes = Bytes::from(serde_json::to_vec(&v).map_err(|e| e.to_string())?);
        if let Ok(mut c) = cache.lock() {
            c.put(key, bytes.clone());
        }
        Ok(bytes)
    })
    .await;

    match result {
        Ok(Ok(bytes)) => json_bytes_response(bytes),
        Ok(Err(e)) => (StatusCode::NOT_FOUND, e).into_response(),
        Err(e) => err500(e),
    }
}

#[derive(Deserialize)]
struct SubagentQ {
    session: Option<String>,
    agent: Option<String>,
}

async fn api_subagent(State(state): State<AppState>, Query(q): Query<SubagentQ>) -> Response {
    let session = q.session.unwrap_or_default();
    let agent = q.agent.unwrap_or_default();
    if session.is_empty() || agent.is_empty() {
        return (StatusCode::BAD_REQUEST, "session and agent required").into_response();
    }
    let key: TranscriptKey = (session.clone(), Some(agent.clone()));

    if let Some(bytes) = state
        .transcript_cache
        .lock()
        .ok()
        .and_then(|mut c| c.get(&key).cloned())
    {
        return json_bytes_response(bytes);
    }

    let db = state.db.clone();
    let cache = state.transcript_cache.clone();
    let result = spawn_blocking(move || -> Result<Bytes, String> {
        let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
        let fp = session_file_path(&conn, &session, true, Some(&agent))?;
        let v = build_timeline(&conn, &fp, true)?;
        let bytes = Bytes::from(serde_json::to_vec(&v).map_err(|e| e.to_string())?);
        if let Ok(mut c) = cache.lock() {
            c.put(key, bytes.clone());
        }
        Ok(bytes)
    })
    .await;

    match result {
        Ok(Ok(bytes)) => json_bytes_response(bytes),
        Ok(Err(e)) => (StatusCode::NOT_FOUND, e).into_response(),
        Err(e) => err500(e),
    }
}

// ── DB helpers ────────────────────────────────────────────────────────────────

fn session_file_path(
    conn: &Connection,
    session_id: &str,
    is_subagent: bool,
    agent_id: Option<&str>,
) -> Result<String, String> {
    if is_subagent {
        let agent = agent_id.unwrap_or("");
        let mut stmt = conn
            .prepare(
                "SELECT file_path FROM transcripts \
             WHERE parent_session_id = ? AND agent_id = ? AND is_subagent LIMIT 1",
            )
            .map_err(|e| e.to_string())?;
        stmt.query_row([session_id, agent], |row| row.get::<_, String>(0))
            .map_err(|_| format!("subagent not found: session={session_id} agent={agent}"))
    } else {
        let mut stmt = conn
            .prepare(
                "SELECT file_path FROM transcripts \
             WHERE session_id = ? AND NOT is_subagent LIMIT 1",
            )
            .map_err(|e| e.to_string())?;
        stmt.query_row([session_id], |row| row.get::<_, String>(0))
            .map_err(|_| format!("session not found: {session_id}"))
    }
}

// ── Timeline builder ──────────────────────────────────────────────────────────

const INJECTED: &[&str] = &[
    "<local-command-caveat>",
    "<command-name>",
    "<command-message>",
    "<task-notification>",
    "<local-command-stdout>",
    "<system-reminder>",
];

fn build_timeline(conn: &Connection, file_path: &str, is_subagent: bool) -> Result<Value, String> {
    // ── entries ───────────────────────────────────────────────────────────────
    struct EntryRow {
        entry_id: i64,
        entry_type: String,
        timestamp: Option<String>,
        is_sidechain: bool,
        is_meta: bool,
    }
    let mut stmt = conn
        .prepare(
            "SELECT entry_id, type, CAST(timestamp AS VARCHAR), \
                COALESCE(is_sidechain, false), COALESCE(is_meta, false) \
         FROM entries WHERE file_path = ? ORDER BY entry_id",
        )
        .map_err(|e| e.to_string())?;
    let entry_rows: Vec<EntryRow> = stmt
        .query_map([file_path], |row| {
            Ok(EntryRow {
                entry_id: row.get(0)?,
                entry_type: row.get(1)?,
                timestamp: row.get(2)?,
                is_sidechain: row.get(3)?,
                is_meta: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    // ── user_entries metadata ─────────────────────────────────────────────────
    let mut user_compact: std::collections::HashSet<i64> = Default::default();
    let mut user_plain_text: HashMap<i64, String> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT ue.entry_id, COALESCE(ue.is_compact_summary, false), ue.message_content_text \
             FROM user_entries ue \
             JOIN entries e ON e.entry_id = ue.entry_id \
             WHERE e.file_path = ?",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([file_path], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows.filter_map(|r| r.ok()) {
            let (eid, compact, plain) = r;
            if compact {
                user_compact.insert(eid);
            }
            if let Some(t) = plain {
                user_plain_text.insert(eid, t);
            }
        }
    }

    // ── user text blocks (for block-content messages) ─────────────────────────
    let mut user_block_text: HashMap<i64, String> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT entry_id, text \
             FROM user_content_blocks \
             WHERE entry_id IN (SELECT entry_id FROM entries WHERE file_path = ?) \
               AND block_type = 'text' AND text IS NOT NULL \
             ORDER BY entry_id, position",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([file_path], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        for r in rows.filter_map(|r| r.ok()) {
            let (eid, text) = r;
            user_block_text.entry(eid).or_default().push_str(&text);
        }
    }

    // ── compact boundary data ─────────────────────────────────────────────────
    struct CompactData {
        subtype: String,
        trigger: Option<String>,
        pre_tokens: Option<i64>,
        post_tokens: Option<i64>,
        duration_ms: Option<i64>,
        pre_discovered_tools: Option<Value>,
    }
    let mut compact_data: HashMap<i64, CompactData> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT se.entry_id, se.subtype, se.compact_trigger, \
                        se.compact_pre_tokens, se.compact_post_tokens, se.compact_duration_ms, \
                        se.compact_pre_discovered_tools \
                 FROM system_entries se \
                 JOIN entries e ON e.entry_id = se.entry_id \
                 WHERE e.file_path = ? \
                   AND se.subtype IN ('compact_boundary', 'microcompact_boundary')",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([file_path], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows.filter_map(|r| r.ok()) {
            let (eid, subtype, trigger, pre_tokens, post_tokens, duration_ms, tools_json) = r;
            let pre_discovered_tools =
                tools_json.and_then(|s| serde_json::from_str::<Value>(&s).ok());
            compact_data.insert(
                eid,
                CompactData {
                    subtype,
                    trigger,
                    pre_tokens,
                    post_tokens,
                    duration_ms,
                    pre_discovered_tools,
                },
            );
        }
    }

    // ── subagent costs: agent_id → cost_usd (parent sessions only) ──────────
    let mut subagent_costs: HashMap<String, f64> = HashMap::new();
    if !is_subagent {
        let mut stmt = conn
            .prepare(
                "SELECT t2.agent_id, ROUND(COALESCE(SUM(d.cost_usd), 0.0), 6) \
                 FROM transcripts t_parent \
                 JOIN transcripts t2 ON t2.parent_session_id = t_parent.session_id \
                 JOIN entries e ON e.file_path = t2.file_path \
                 JOIN assistant_entries_deduped d ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL \
                 WHERE t_parent.file_path = ? AND NOT t_parent.is_subagent \
                   AND t2.is_subagent AND t2.agent_id IS NOT NULL \
                 GROUP BY t2.agent_id",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([file_path], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
            })
            .map_err(|e| e.to_string())?;
        for r in rows.filter_map(|r| r.ok()) {
            let (aid, cost) = r;
            subagent_costs.insert(aid, cost);
        }
    }

    // ── tool results: tool_use_id → (text, agent_id) ─────────────────────────
    let mut tool_results: HashMap<String, String> = HashMap::new();
    let mut tool_agent_ids: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT ucb.tool_use_id, ucb.tool_result_content, \
                        json_extract_string(ue.tool_use_result, '$.agentId') AS agent_id \
                 FROM user_content_blocks ucb \
                 JOIN user_entries ue ON ue.entry_id = ucb.entry_id \
                 WHERE ucb.entry_id IN (SELECT entry_id FROM entries WHERE file_path = ?) \
                   AND ucb.block_type = 'tool_result' AND ucb.tool_use_id IS NOT NULL",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([file_path], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows.filter_map(|r| r.ok()) {
            let (tu_id, content_json, agent_id) = r;
            let text = content_json
                .as_deref()
                .map(extract_tool_result_text)
                .unwrap_or_default();
            tool_results.insert(tu_id.clone(), text);
            if let Some(aid) = agent_id {
                if !aid.is_empty() {
                    tool_agent_ids.insert(tu_id, aid);
                }
            }
        }
    }

    // ── deduped assistant entry IDs ───────────────────────────────────────────
    let mut deduped_ids: std::collections::HashSet<i64> = Default::default();
    {
        let mut stmt = conn
            .prepare(
                "SELECT aed.entry_id \
             FROM assistant_entries_deduped aed \
             JOIN entries e ON e.entry_id = aed.entry_id \
             WHERE e.file_path = ?",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([file_path], |row| row.get::<_, i64>(0))
            .map_err(|e| e.to_string())?;
        for r in rows.filter_map(|r| r.ok()) {
            deduped_ids.insert(r);
        }
    }

    // ── assistant entry data ──────────────────────────────────────────────────
    struct AsstData {
        model: String,
        cost_usd: Option<f64>,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
    }
    let mut asst_data: HashMap<i64, AsstData> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT aed.entry_id, aed.model, aed.cost_usd, \
                    aed.input_tokens, aed.output_tokens, \
                    COALESCE(aed.cache_read_input_tokens, 0), \
                    COALESCE(aed.cache_creation_5m, 0) + COALESCE(aed.cache_creation_1h, 0) \
                      + CASE \
                          WHEN COALESCE(aed.cache_creation_5m, 0) + COALESCE(aed.cache_creation_1h, 0) > 0 \
                          THEN 0 \
                          ELSE COALESCE(aed.cache_creation_input_tokens, 0) \
                        END \
             FROM assistant_entries_deduped aed \
             JOIN entries e ON e.entry_id = aed.entry_id \
             WHERE e.file_path = ?",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([file_path], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows.filter_map(|r| r.ok()) {
            let (eid, model, cost, inp, out, cr, cw) = r;
            asst_data.insert(
                eid,
                AsstData {
                    model,
                    cost_usd: cost,
                    input: inp,
                    output: out,
                    cache_read: cr,
                    cache_write: cw,
                },
            );
        }
    }

    // ── assistant content blocks ───────────────────────────────────────────────
    // Each streaming JSONL entry for a message carries exactly one content block.
    // The deduped view picks one entry_id per message_id, but the blocks are spread
    // across all sibling entries. Join through message_id to collect all of them,
    // grouping under the deduped entry_id.
    struct Block {
        block_type: String,
        text: Option<String>,
        tu_id: Option<String>,
        tu_name: Option<String>,
        tu_input: Option<Value>,
    }
    let mut asst_blocks: HashMap<i64, Vec<Block>> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT aed.entry_id AS dedup_eid, acb.block_type, acb.text, \
                        acb.tool_use_id, acb.tool_name, acb.tool_input \
                 FROM assistant_entries_deduped aed \
                 JOIN entries e_dedup ON e_dedup.entry_id = aed.entry_id \
                   AND e_dedup.file_path = ? \
                 JOIN assistant_entries ae_dedup ON ae_dedup.entry_id = aed.entry_id \
                 JOIN entries e_all ON e_all.file_path = ? \
                 JOIN assistant_entries ae_all ON ae_all.entry_id = e_all.entry_id \
                   AND (   (ae_dedup.message_id IS NOT NULL \
                            AND ae_all.message_id = ae_dedup.message_id) \
                        OR (ae_dedup.message_id IS NULL \
                            AND ae_all.entry_id = aed.entry_id)) \
                 JOIN assistant_content_blocks acb ON acb.entry_id = ae_all.entry_id \
                 ORDER BY aed.entry_id, ae_all.entry_id, acb.position",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([file_path, file_path], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows.filter_map(|r| r.ok()) {
            let (dedup_eid, bt, text, tu_id, tu_name, tu_input_json) = r;
            let tu_input = tu_input_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok());
            asst_blocks.entry(dedup_eid).or_default().push(Block {
                block_type: bt,
                text,
                tu_id,
                tu_name,
                tu_input,
            });
        }
    }

    // ── assemble ──────────────────────────────────────────────────────────────
    let mut out: Vec<Value> = Vec::new();
    let mut api_num: i64 = 0;

    for e in &entry_rows {
        if (!is_subagent && e.is_sidechain) || e.is_meta {
            continue;
        }

        match e.entry_type.as_str() {
            "user" => {
                if user_compact.contains(&e.entry_id) {
                    continue;
                }
                let text = user_plain_text
                    .get(&e.entry_id)
                    .or_else(|| user_block_text.get(&e.entry_id))
                    .cloned()
                    .unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                if INJECTED.iter().any(|p| text.starts_with(p)) {
                    continue;
                }
                out.push(json!({
                    "kind":      "user",
                    "timestamp": e.timestamp,
                    "text":      text,
                }));
            }
            "assistant" => {
                if !deduped_ids.contains(&e.entry_id) {
                    continue;
                }
                let Some(ad) = asst_data.get(&e.entry_id) else {
                    continue;
                };
                api_num += 1;

                let blocks = asst_blocks
                    .get(&e.entry_id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                let has_thinking = blocks
                    .iter()
                    .any(|b| b.block_type == "thinking" || b.block_type == "redacted_thinking");
                let texts: Vec<String> = blocks
                    .iter()
                    .filter(|b| b.block_type == "text")
                    .filter_map(|b| b.text.clone())
                    .filter(|t| !t.is_empty())
                    .collect();
                let tool_uses: Vec<Value> = blocks
                    .iter()
                    .filter(|b| b.block_type == "tool_use")
                    .filter_map(|b| {
                        let id = b.tu_id.as_ref()?;
                        let name = b.tu_name.as_ref()?;
                        let input = b.tu_input.clone().unwrap_or(json!({}));
                        let summary = summarize_input(name, &input);
                        let result = tool_results.get(id).cloned().unwrap_or_default();
                        let agent_id = tool_agent_ids.get(id).cloned();
                        let subagent_cost = agent_id
                            .as_ref()
                            .and_then(|aid| subagent_costs.get(aid))
                            .copied();
                        Some(json!({
                            "id":                id,
                            "name":              name,
                            "summary":           summary,
                            "input":             input,
                            "result":            result,
                            "agent_id":          agent_id,
                            "subagent_cost_usd": subagent_cost,
                        }))
                    })
                    .collect();

                out.push(json!({
                    "kind":                        "assistant",
                    "entry_id":                    e.entry_id,
                    "num":                         api_num,
                    "timestamp":                   e.timestamp,
                    "model":                       ad.model,
                    "cost_usd":                    ad.cost_usd,
                    "input_tokens":                ad.input,
                    "output_tokens":               ad.output,
                    "cache_read_input_tokens":     ad.cache_read,
                    "cache_creation_input_tokens": ad.cache_write,
                    "has_thinking":                has_thinking,
                    "texts":                       texts,
                    "tool_uses":                   tool_uses,
                }));
            }
            "system" => {
                if let Some(cd) = compact_data.get(&e.entry_id) {
                    let reduction = match (cd.pre_tokens, cd.post_tokens) {
                        (Some(pre), Some(post)) if pre > 0 => {
                            Some(((pre - post) as f64 / pre as f64 * 100.0).round() as i64)
                        }
                        _ => None,
                    };
                    out.push(json!({
                        "kind":                  "compact",
                        "subtype":               cd.subtype,
                        "timestamp":             e.timestamp,
                        "trigger":               cd.trigger,
                        "pre_tokens":            cd.pre_tokens,
                        "post_tokens":           cd.post_tokens,
                        "duration_ms":           cd.duration_ms,
                        "reduction_pct":         reduction,
                        "pre_discovered_tools":  cd.pre_discovered_tools,
                    }));
                }
            }
            _ => {}
        }
    }

    Ok(json!({ "entries": out }))
}

// ── Dashboard endpoints ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct DashboardQ {
    from: Option<String>,
    to: Option<String>,
}

fn time_bounds(q: &DashboardQ) -> (String, String) {
    let from = q
        .from
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("1900-01-01")
        .to_owned();
    let to =
        q.to.as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("2100-01-01")
            .to_owned();
    (from, to)
}

async fn api_dashboard_summary(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT \
                   ROUND(COALESCE(SUM(d.cost_usd), 0.0), 4) AS cost_usd, \
                   COUNT(DISTINCT CASE WHEN NOT t.is_subagent THEN t.session_id END) AS session_count, \
                   COUNT(DISTINCT CASE WHEN t.is_subagent THEN t.session_id END) AS subagent_count, \
                   COUNT(d.entry_id) AS api_call_count \
                 FROM entries e \
                 JOIN transcripts t ON t.file_path = e.file_path \
                 JOIN assistant_entries_deduped d ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL \
                 WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP)",
            )
            .map_err(|e| e.to_string())?;
        let (cost_usd, session_count, subagent_count, api_call_count) = stmt
            .query_row([&from, &to], |row| {
                Ok((
                    row.get::<_, f64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let denom = session_count.max(1) as f64;
        let avg = (cost_usd / denom * 1_000_000.0).round() / 1_000_000.0;
        Ok(json!({
            "cost_usd":              cost_usd,
            "session_count":         session_count,
            "subagent_count":        subagent_count,
            "api_call_count":        api_call_count,
            "avg_cost_per_session":  avg,
        }))
    })
    .await;

    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_daily(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT \
                   CAST(e.timestamp AS DATE)::VARCHAR AS date, \
                   ROUND(SUM(CASE WHEN d.model ILIKE '%opus%' THEN d.cost_usd ELSE 0.0 END), 4) AS cost_opus, \
                   ROUND(SUM(CASE WHEN d.model NOT ILIKE '%opus%' AND d.model NOT ILIKE '%haiku%' THEN d.cost_usd ELSE 0.0 END), 4) AS cost_sonnet, \
                   ROUND(SUM(CASE WHEN d.model ILIKE '%haiku%' THEN d.cost_usd ELSE 0.0 END), 4) AS cost_haiku \
                 FROM entries e \
                 JOIN assistant_entries_deduped d ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL \
                 WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP) \
                 GROUP BY CAST(e.timestamp AS DATE) \
                 ORDER BY CAST(e.timestamp AS DATE) ASC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([&from, &to], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, f64>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows.filter_map(|r| r.ok()) {
            let (date, cost_opus, cost_sonnet, cost_haiku) = r;
            out.push(json!({
                "date":        date,
                "cost_opus":   cost_opus,
                "cost_sonnet": cost_sonnet,
                "cost_haiku":  cost_haiku,
            }));
        }
        Ok(Value::Array(out))
    })
    .await;

    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_models(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT \
                   d.model, \
                   COUNT(DISTINCT e.session_id) AS sessions, \
                   COUNT(d.entry_id) AS api_calls, \
                   ROUND(SUM(d.cost_usd), 4) AS cost_usd, \
                   ROUND(100.0 * SUM(d.cost_usd) / NULLIF(SUM(SUM(d.cost_usd)) OVER (), 0.0), 2) AS pct_spend, \
                   ROUND(SUM(d.cost_usd) / NULLIF(COUNT(d.entry_id), 0), 6) AS avg_cost_per_turn \
                 FROM entries e \
                 JOIN assistant_entries_deduped d ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL \
                 WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP) \
                 GROUP BY d.model \
                 ORDER BY cost_usd DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([&from, &to], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                    row.get::<_, Option<f64>>(4)?,
                    row.get::<_, Option<f64>>(5)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows.filter_map(|r| r.ok()) {
            let (model, sessions, api_calls, cost_usd, pct_spend, avg_cost_per_turn) = r;
            out.push(json!({
                "model":             model,
                "sessions":          sessions,
                "api_calls":         api_calls,
                "cost_usd":          cost_usd.unwrap_or(0.0),
                "pct_spend":         pct_spend.unwrap_or(0.0),
                "avg_cost_per_turn": avg_cost_per_turn.unwrap_or(0.0),
            }));
        }
        Ok(Value::Array(out))
    })
    .await;

    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_cache(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;

        // global cache stats
        let mut stmt = conn
            .prepare(
                "SELECT \
                   COALESCE(SUM(d.cache_read_input_tokens), 0) AS cache_read_tokens, \
                   COALESCE(SUM( \
                     COALESCE(d.cache_creation_5m, 0) + COALESCE(d.cache_creation_1h, 0) \
                     + CASE WHEN COALESCE(d.cache_creation_5m, 0) + COALESCE(d.cache_creation_1h, 0) > 0 \
                            THEN 0 ELSE COALESCE(d.cache_creation_input_tokens, 0) END \
                   ), 0) AS cache_create_tokens, \
                   COALESCE(SUM(d.input_tokens), 0) + COALESCE(SUM(d.cache_read_input_tokens), 0) \
                   + COALESCE(SUM( \
                     COALESCE(d.cache_creation_5m, 0) + COALESCE(d.cache_creation_1h, 0) \
                     + CASE WHEN COALESCE(d.cache_creation_5m, 0) + COALESCE(d.cache_creation_1h, 0) > 0 \
                            THEN 0 ELSE COALESCE(d.cache_creation_input_tokens, 0) END \
                   ), 0) \
                   + COALESCE(SUM(d.output_tokens), 0) AS total_tokens \
                 FROM entries e \
                 JOIN assistant_entries_deduped d ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL \
                 WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP)",
            )
            .map_err(|e| e.to_string())?;
        let (cache_read, cache_create, total) = stmt
            .query_row([&from, &to], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let denom = total.max(1) as f64;
        let hit_rate = cache_read as f64 / denom;
        let create_rate = cache_create as f64 / denom;

        // thrash turns
        let mut stmt2 = conn
            .prepare(
                "SELECT \
                   d.entry_id, \
                   t.session_id, \
                   regexp_extract(t.file_path, '.*/projects/([^/]+)/[^/]+\\.jsonl$', 1) AS project, \
                   ROUND(COALESCE(d.cost_usd, 0.0), 4) AS cost_usd, \
                   COALESCE(d.cache_creation_5m, 0) + COALESCE(d.cache_creation_1h, 0) \
                     + CASE WHEN COALESCE(d.cache_creation_5m, 0) + COALESCE(d.cache_creation_1h, 0) > 0 \
                            THEN 0 ELSE COALESCE(d.cache_creation_input_tokens, 0) END AS cc_tokens, \
                   COALESCE(d.output_tokens, 0) AS output_tokens \
                 FROM entries e \
                 JOIN transcripts t ON t.file_path = e.file_path \
                 JOIN assistant_entries_deduped d ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL \
                 WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP) \
                   AND COALESCE(d.output_tokens, 0) < 200 \
                   AND (COALESCE(d.cache_creation_5m, 0) + COALESCE(d.cache_creation_1h, 0) \
                        + CASE WHEN COALESCE(d.cache_creation_5m, 0) + COALESCE(d.cache_creation_1h, 0) > 0 \
                               THEN 0 ELSE COALESCE(d.cache_creation_input_tokens, 0) END) > 10000 \
                 ORDER BY cc_tokens DESC \
                 LIMIT 10",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt2
            .query_map([&from, &to], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut thrash = Vec::new();
        for r in rows.filter_map(|r| r.ok()) {
            let (entry_id, session_id, project, cost_usd, cc_tokens, output_tokens) = r;
            thrash.push(json!({
                "entry_id":      entry_id,
                "session_id":    session_id,
                "project":       project,
                "cost_usd":      cost_usd,
                "cc_tokens":     cc_tokens,
                "output_tokens": output_tokens,
            }));
        }

        Ok(json!({
            "hit_rate":            hit_rate,
            "create_rate":         create_rate,
            "cache_read_tokens":   cache_read,
            "cache_create_tokens": cache_create,
            "total_tokens":        total,
            "thrash_turns":        thrash,
        }))
    })
    .await;

    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_agents(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;

        // call counts
        let mut stmt1 = conn
            .prepare(
                "SELECT \
                   COUNT(*) FILTER (WHERE json_extract_string(acb.tool_input, '$.model') IS NOT NULL) AS explicit_calls, \
                   COUNT(*) FILTER (WHERE json_extract_string(acb.tool_input, '$.model') IS NULL) AS inherited_calls \
                 FROM assistant_content_blocks acb \
                 JOIN entries e ON e.entry_id = acb.entry_id \
                 WHERE acb.block_type = 'tool_use' AND acb.tool_name = 'Agent' \
                   AND CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP)",
            )
            .map_err(|e| e.to_string())?;
        let (explicit_calls, inherited_calls) = stmt1
            .query_row([&from, &to], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| e.to_string())?;

        // total subagent cost
        let mut stmt2 = conn
            .prepare(
                "SELECT ROUND(COALESCE(SUM(d.cost_usd), 0.0), 4) \
                 FROM transcripts t \
                 JOIN entries e ON e.file_path = t.file_path \
                 JOIN assistant_entries_deduped d ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL \
                 WHERE t.is_subagent \
                   AND CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP)",
            )
            .map_err(|e| e.to_string())?;
        let total_subagent_cost: f64 = stmt2
            .query_row([&from, &to], |row| row.get::<_, f64>(0))
            .map_err(|e| e.to_string())?;

        let total_agent_calls = explicit_calls + inherited_calls;
        let total_agent_calls_denom = total_agent_calls.max(1) as f64;
        let inherited_cost_usd =
            total_subagent_cost * inherited_calls as f64 / total_agent_calls_denom;
        let inherited_cost_usd = (inherited_cost_usd * 10_000.0).round() / 10_000.0;

        // subtypes
        let mut stmt3 = conn
            .prepare(
                "SELECT \
                   COALESCE(json_extract_string(acb.tool_input, '$.subagent_type'), 'general-purpose') AS subtype, \
                   COUNT(*) AS count \
                 FROM assistant_content_blocks acb \
                 JOIN entries e ON e.entry_id = acb.entry_id \
                 WHERE acb.block_type = 'tool_use' AND acb.tool_name = 'Agent' \
                   AND CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP) \
                 GROUP BY 1 \
                 ORDER BY count DESC \
                 LIMIT 15",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt3
            .query_map([&from, &to], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| e.to_string())?;
        let mut subtypes = Vec::new();
        for r in rows.filter_map(|r| r.ok()) {
            let (subtype, count) = r;
            let cost = total_subagent_cost * count as f64 / total_agent_calls_denom;
            let cost = (cost * 10_000.0).round() / 10_000.0;
            subtypes.push(json!({
                "subtype":  subtype,
                "count":    count,
                "cost_usd": cost,
            }));
        }

        // spawn model breakdown per subtype
        let mut stmt4 = conn
            .prepare(
                "SELECT \
                   COALESCE(json_extract_string(acb.tool_input, '$.subagent_type'), 'general-purpose') AS subtype, \
                   COUNT(*) AS spawns, \
                   COUNT(*) FILTER (WHERE json_extract_string(acb.tool_input, '$.model') IS NOT NULL) AS explicit, \
                   COUNT(*) FILTER (WHERE json_extract_string(acb.tool_input, '$.model') IS NULL)     AS inherited \
                 FROM assistant_content_blocks acb \
                 JOIN entries e ON e.entry_id = acb.entry_id \
                 WHERE acb.block_type = 'tool_use' AND acb.tool_name = 'Agent' \
                   AND CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP) \
                 GROUP BY COALESCE(json_extract_string(acb.tool_input, '$.subagent_type'), 'general-purpose') \
                 ORDER BY spawns DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows4 = stmt4
            .query_map([&from, &to], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut spawn_model_breakdown = Vec::new();
        for r in rows4.filter_map(|r| r.ok()) {
            let (subtype, spawns, explicit, inherited) = r;
            spawn_model_breakdown.push(json!({
                "subtype":   subtype,
                "spawns":    spawns,
                "explicit":  explicit,
                "inherited": inherited,
            }));
        }

        Ok(json!({
            "explicit_calls":        explicit_calls,
            "inherited_calls":       inherited_calls,
            "inherited_cost_usd":    inherited_cost_usd,
            "subtypes":              subtypes,
            "spawn_model_breakdown": spawn_model_breakdown,
        }))
    })
    .await;

    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_top_sessions(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT \
                   t.session_id, \
                   regexp_extract(t.file_path, '.*/projects/([^/]+)/[^/]+\\.jsonl$', 1) AS project, \
                   CAST(t.first_timestamp AS VARCHAR) AS started_at, \
                   ROUND(COALESCE(SUM(d.cost_usd), 0.0), 4) AS cost_usd, \
                   COUNT(DISTINCT d.entry_id) AS turn_count, \
                   COALESCE(( \
                     SELECT COUNT(*) FROM user_content_blocks ucb2 \
                     JOIN entries e2 ON e2.entry_id = ucb2.entry_id AND e2.file_path = t.file_path \
                     WHERE ucb2.is_error = true \
                   ), 0) AS error_count, \
                   COALESCE(( \
                     SELECT COUNT(*) FROM transcripts t2 \
                     WHERE t2.parent_session_id = t.session_id \
                   ), 0) AS subagent_count \
                 FROM transcripts t \
                 JOIN entries e ON e.file_path = t.file_path \
                 JOIN assistant_entries_deduped d ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL \
                 WHERE NOT t.is_subagent \
                   AND CAST(t.first_timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(t.first_timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP) \
                 GROUP BY t.session_id, t.file_path, t.first_timestamp \
                 ORDER BY cost_usd DESC \
                 LIMIT 15",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([&from, &to], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows.filter_map(|r| r.ok()) {
            let (session_id, project, started_at, cost_usd, turn_count, error_count, subagent_count) = r;
            out.push(json!({
                "session_id":     session_id,
                "project":        project,
                "started_at":     started_at,
                "cost_usd":       cost_usd,
                "turn_count":     turn_count,
                "error_count":    error_count,
                "subagent_count": subagent_count,
            }));
        }
        Ok(Value::Array(out))
    })
    .await;

    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_session_distribution(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT \
                   bucket, \
                   COUNT(*) AS session_count, \
                   ROUND(SUM(session_cost), 4) AS total_cost, \
                   ROUND(AVG(session_cost), 4) AS avg_cost, \
                   ROUND(MAX(session_cost), 4) AS max_cost \
                 FROM ( \
                   SELECT \
                     t.session_id, \
                     COALESCE(SUM(d.cost_usd), 0.0) AS session_cost, \
                     COUNT(DISTINCT d.entry_id) AS turn_count, \
                     CASE \
                       WHEN COUNT(DISTINCT d.entry_id) < 20 THEN '<20' \
                       WHEN COUNT(DISTINCT d.entry_id) < 100 THEN '20-100' \
                       WHEN COUNT(DISTINCT d.entry_id) < 500 THEN '100-500' \
                       WHEN COUNT(DISTINCT d.entry_id) < 2000 THEN '500-2k' \
                       ELSE '2k+' \
                     END AS bucket \
                   FROM transcripts t \
                   JOIN entries e ON e.file_path = t.file_path \
                   JOIN assistant_entries_deduped d ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL \
                   WHERE NOT t.is_subagent \
                     AND CAST(t.first_timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                     AND CAST(t.first_timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP) \
                   GROUP BY t.session_id \
                 ) sub \
                 GROUP BY bucket \
                 ORDER BY CASE bucket \
                   WHEN '<20' THEN 1 \
                   WHEN '20-100' THEN 2 \
                   WHEN '100-500' THEN 3 \
                   WHEN '500-2k' THEN 4 \
                   ELSE 5 \
                 END",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([&from, &to], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, f64>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows.filter_map(|r| r.ok()) {
            let (bucket, session_count, total_cost, avg_cost, max_cost) = r;
            out.push(json!({
                "bucket":        bucket,
                "session_count": session_count,
                "total_cost":    total_cost,
                "avg_cost":      avg_cost,
                "max_cost":      max_cost,
            }));
        }
        Ok(Value::Array(out))
    })
    .await;

    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_file_hotspots(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT \
                   json_extract_string(acb.tool_input, '$.file_path') AS file_path, \
                   COUNT(DISTINCT e.session_id) AS distinct_sessions, \
                   COUNT(*) AS total_reads \
                 FROM assistant_content_blocks acb \
                 JOIN entries e ON e.entry_id = acb.entry_id \
                 JOIN transcripts t ON t.file_path = e.file_path \
                 WHERE acb.block_type = 'tool_use' AND acb.tool_name = 'Read' \
                   AND NOT t.is_subagent \
                   AND CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP) \
                   AND json_extract_string(acb.tool_input, '$.file_path') IS NOT NULL \
                   AND json_extract_string(acb.tool_input, '$.file_path') != '' \
                 GROUP BY 1 \
                 ORDER BY 2 DESC \
                 LIMIT 30",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([&from, &to], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows.filter_map(|r| r.ok()) {
            let (file_path, distinct_sessions, total_reads) = r;
            out.push(json!({
                "file_path":         file_path,
                "distinct_sessions": distinct_sessions,
                "total_reads":       total_reads,
            }));
        }
        Ok(Value::Array(out))
    })
    .await;

    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_errors(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;

        // Query A: error types
        let mut stmt_a = conn
            .prepare(
                "SELECT \
                   CASE \
                     WHEN tool_result_content::TEXT ILIKE '%permission denied%' \
                       OR tool_result_content::TEXT ILIKE '%Operation not permitted%' THEN 'permission_denied' \
                     WHEN tool_result_content::TEXT ILIKE '%No such file%' \
                       OR tool_result_content::TEXT ILIKE '%not found%' \
                       OR tool_result_content::TEXT ILIKE '%does not exist%' THEN 'no_such_file' \
                     WHEN tool_result_content::TEXT ILIKE '%timeout%' \
                       OR tool_result_content::TEXT ILIKE '%timed out%' THEN 'timeout' \
                     WHEN tool_result_content::TEXT ILIKE '%tool_use_error%' \
                       OR tool_result_content::TEXT ILIKE '%ToolUseError%' THEN 'tool_use_error' \
                     ELSE 'other' \
                   END AS error_type, \
                   COUNT(*) AS count, \
                   COUNT(DISTINCT e.session_id) AS sessions_affected \
                 FROM user_content_blocks ucb \
                 JOIN entries e ON e.entry_id = ucb.entry_id \
                 JOIN transcripts t ON t.file_path = e.file_path \
                 WHERE ucb.is_error = true \
                   AND NOT t.is_subagent \
                   AND CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP) \
                 GROUP BY error_type \
                 ORDER BY count DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows_a = stmt_a
            .query_map([&from, &to], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut types = Vec::new();
        for r in rows_a.filter_map(|r| r.ok()) {
            let (error_type, count, sessions_affected) = r;
            types.push(json!({
                "error_type":        error_type,
                "count":             count,
                "sessions_affected": sessions_affected,
            }));
        }

        // Query B1: session costs
        let mut stmt_b1 = conn
            .prepare(
                "SELECT \
                   t.session_id, \
                   COALESCE(SUM(d.cost_usd), 0.0) AS session_cost, \
                   COUNT(DISTINCT d.entry_id) AS turn_count \
                 FROM transcripts t \
                 JOIN entries e ON e.file_path = t.file_path \
                 JOIN assistant_entries_deduped d ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL \
                 WHERE NOT t.is_subagent \
                   AND CAST(t.first_timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(t.first_timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP) \
                 GROUP BY t.session_id",
            )
            .map_err(|e| e.to_string())?;
        let rows_b1 = stmt_b1
            .query_map([&from, &to], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut session_cost: HashMap<String, (f64, i64)> = HashMap::new();
        for r in rows_b1.filter_map(|r| r.ok()) {
            let (sid, cost, turns) = r;
            if let Some(s) = sid {
                session_cost.insert(s, (cost, turns));
            }
        }

        // Query B2: session error counts
        let mut stmt_b2 = conn
            .prepare(
                "SELECT \
                   t.session_id, \
                   COUNT(*) AS error_count \
                 FROM transcripts t \
                 JOIN entries e ON e.file_path = t.file_path \
                 JOIN user_content_blocks ucb ON ucb.entry_id = e.entry_id \
                 WHERE ucb.is_error = true \
                   AND NOT t.is_subagent \
                   AND CAST(t.first_timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(t.first_timestamp AS TIMESTAMP) < CAST(? AS TIMESTAMP) \
                 GROUP BY t.session_id",
            )
            .map_err(|e| e.to_string())?;
        let rows_b2 = stmt_b2
            .query_map([&from, &to], |row| {
                Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| e.to_string())?;
        let mut session_errors: HashMap<String, i64> = HashMap::new();
        for r in rows_b2.filter_map(|r| r.ok()) {
            let (sid, ec) = r;
            if let Some(s) = sid {
                session_errors.insert(s, ec);
            }
        }

        // Bucket and aggregate
        fn bucket_for(err: i64) -> &'static str {
            if err == 0 {
                "0 errors"
            } else if err < 10 {
                "1-9"
            } else if err < 50 {
                "10-49"
            } else {
                "50+"
            }
        }

        // bucket -> (sessions, total_cost, total_turns, total_errors)
        let mut buckets: HashMap<&'static str, (i64, f64, i64, i64)> = HashMap::new();
        for (sid, (cost, turns)) in &session_cost {
            let err = *session_errors.get(sid).unwrap_or(&0);
            let b = bucket_for(err);
            let entry = buckets.entry(b).or_insert((0, 0.0, 0, 0));
            entry.0 += 1;
            entry.1 += cost;
            entry.2 += turns;
            entry.3 += err;
        }

        let order = ["0 errors", "1-9", "10-49", "50+"];
        let mut by_bucket = Vec::new();
        for label in &order {
            if let Some((sessions, total_cost, total_turns, total_errors)) = buckets.get(*label) {
                let turns_denom = (*total_turns).max(1) as f64;
                let avg_cost_per_turn =
                    ((total_cost / turns_denom) * 1_000_000.0).round() / 1_000_000.0;
                let errors_per_turn =
                    ((*total_errors as f64 / turns_denom) * 1_000_000.0).round() / 1_000_000.0;
                by_bucket.push(json!({
                    "bucket":            label,
                    "sessions":          sessions,
                    "avg_cost_per_turn": avg_cost_per_turn,
                    "errors_per_turn":   errors_per_turn,
                }));
            }
        }

        Ok(json!({
            "types":     types,
            "by_bucket": by_bucket,
        }))
    })
    .await;

    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_baseline(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;

        // trailing 4-week weekly spend + mean + median
        let mut stmt = conn
            .prepare(
                "WITH bounds AS (
               SELECT date_trunc('day', MAX(last_timestamp)) AS anchor FROM transcripts
             ),
             weekly AS (
               SELECT date_trunc('week', e.timestamp)::VARCHAR AS week_start,
                      ROUND(SUM(d.cost_usd), 2) AS cost_usd
               FROM entries e
               JOIN assistant_entries_deduped d
                 ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL
               WHERE e.timestamp >= (SELECT anchor - INTERVAL 28 DAY FROM bounds)
                 AND e.timestamp <  (SELECT anchor FROM bounds)
               GROUP BY 1
             ),
             stats AS (
               SELECT ROUND(AVG(cost_usd), 2)                       AS mean_usd,
                      ROUND(QUANTILE_CONT(cost_usd, 0.5), 2)         AS median_usd,
                      COUNT(*)                                        AS week_count,
                      (SELECT anchor::VARCHAR FROM bounds)            AS anchor
               FROM weekly
             )
             SELECT w.week_start, w.cost_usd, s.mean_usd, s.median_usd, s.week_count, s.anchor
             FROM weekly w, stats s
             ORDER BY w.week_start",
            )
            .map_err(|e| e.to_string())?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,         // week_start
                    row.get::<_, f64>(1)?,            // cost_usd
                    row.get::<_, f64>(2)?,            // mean_usd
                    row.get::<_, f64>(3)?,            // median_usd
                    row.get::<_, i64>(4)?,            // week_count
                    row.get::<_, Option<String>>(5)?, // anchor
                ))
            })
            .map_err(|e| e.to_string())?;

        let mut weeks = Vec::new();
        let mut mean_usd = 0.0_f64;
        let mut median_usd = 0.0_f64;
        let mut week_count = 0_i64;
        let mut anchor = None::<String>;

        for r in rows.filter_map(|r| r.ok()) {
            let (ws, cost, mean, median, count, anch) = r;
            weeks.push(json!({ "week_start": ws, "cost_usd": cost }));
            mean_usd = mean;
            median_usd = median;
            week_count = count;
            if anchor.is_none() {
                anchor = anch;
            }
        }

        // selected-range total
        let selected: f64 = conn
            .query_row(
                "SELECT ROUND(COALESCE(SUM(d.cost_usd), 0.0), 4)
                 FROM entries e
                 JOIN assistant_entries_deduped d
                   ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL
                 WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
                   AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)",
                [&from, &to],
                |row| row.get::<_, f64>(0),
            )
            .map_err(|e| e.to_string())?;

        let vs_mean = if mean_usd > 0.0 {
            selected / mean_usd
        } else {
            0.0
        };
        let vs_median = if median_usd > 0.0 {
            selected / median_usd
        } else {
            0.0
        };

        Ok(json!({
            "anchor":         anchor,
            "weeks":          weeks,
            "week_count":     week_count,
            "mean_usd":       mean_usd,
            "median_usd":     median_usd,
            "selected_usd":   selected,
            "vs_mean":        vs_mean,
            "vs_median":      vs_median,
        }))
    })
    .await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_token_streams(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;

        let sql = "
WITH latest_pricing AS (
  SELECT model, input_per_mtok, output_per_mtok,
         cache_creation_5m_per_mtok, cache_creation_1h_per_mtok, cache_read_per_mtok
  FROM (
    SELECT *, ROW_NUMBER() OVER (PARTITION BY model ORDER BY effective_date DESC) AS rn
    FROM model_pricing
  ) WHERE rn = 1
),
rows AS (
  SELECT
    e.is_sidechain,
    d.input_tokens,
    d.output_tokens,
    d.cache_read_input_tokens,
    COALESCE(d.cache_creation_5m, 0) AS cc5m,
    COALESCE(d.cache_creation_1h, 0) AS cc1h,
    CASE WHEN COALESCE(d.cache_creation_5m, 0) + COALESCE(d.cache_creation_1h, 0) = 0
         THEN COALESCE(d.cache_creation_input_tokens, 0)
         ELSE 0 END AS cc_legacy,
    p.input_per_mtok, p.output_per_mtok,
    p.cache_creation_5m_per_mtok, p.cache_creation_1h_per_mtok, p.cache_read_per_mtok,
    d.cost_usd
  FROM entries e
  JOIN assistant_entries_deduped d
    ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL
  LEFT JOIN latest_pricing p ON p.model = d.model
  WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
    AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
)
SELECT
  is_sidechain,
  ROUND(SUM(COALESCE(input_tokens, 0)             * COALESCE(input_per_mtok, 0))              / 1e6, 4) AS cost_input,
  ROUND(SUM(COALESCE(output_tokens, 0)            * COALESCE(output_per_mtok, 0))             / 1e6, 4) AS cost_output,
  ROUND(SUM(COALESCE(cache_read_input_tokens, 0)  * COALESCE(cache_read_per_mtok, 0))         / 1e6, 4) AS cost_cache_read,
  ROUND(SUM((cc5m + cc_legacy)                    * COALESCE(cache_creation_5m_per_mtok, 0))  / 1e6, 4) AS cost_cc5m,
  ROUND(SUM(cc1h                                  * COALESCE(cache_creation_1h_per_mtok, 0))  / 1e6, 4) AS cost_cc1h,
  ROUND(SUM(cost_usd), 4) AS cost_usd_actual
FROM rows
GROUP BY is_sidechain";

        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows = stmt.query_map([&from, &to], |row| {
            Ok((
                row.get::<_, Option<bool>>(0)?,   // is_sidechain
                row.get::<_, f64>(1)?,             // cost_input
                row.get::<_, f64>(2)?,             // cost_output
                row.get::<_, f64>(3)?,             // cost_cache_read
                row.get::<_, f64>(4)?,             // cost_cc5m
                row.get::<_, f64>(5)?,             // cost_cc1h
                row.get::<_, f64>(6)?,             // cost_usd_actual
            ))
        }).map_err(|e| e.to_string())?;

        let mut main_row = None;
        let mut side_row = None;
        let mut total_derived = 0.0_f64;
        let mut total_actual  = 0.0_f64;

        for r in rows.filter_map(|r| r.ok()) {
            let (is_sidechain, ci, co, cr, cc5m, cc1h, actual) = r;
            let total = ci + co + cr + cc5m + cc1h;
            total_derived += total;
            total_actual  += actual;
            let obj = json!({
                "input":       ci,
                "output":      co,
                "cache_read":  cr,
                "cc5m":        cc5m,
                "cc1h":        cc1h,
                "total":       (total * 10000.0).round() / 10000.0,
            });
            if is_sidechain.unwrap_or(false) {
                side_row = Some(obj);
            } else {
                main_row = Some(obj);
            }
        }

        let delta = total_derived - total_actual;
        Ok(json!({
            "streams": {
                "main":      main_row.unwrap_or(json!({"input":0,"output":0,"cache_read":0,"cc5m":0,"cc1h":0,"total":0})),
                "sidechain": side_row.unwrap_or(json!({"input":0,"output":0,"cache_read":0,"cc5m":0,"cc1h":0,"total":0})),
            },
            "reconciliation_delta": (delta * 10000.0).round() / 10000.0,
        }))
    }).await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

// ── Artifact leaderboard ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ArtifactQ {
    from: Option<String>,
    to: Option<String>,
    kind: String,
    tool: Option<String>,
    limit: Option<i64>,
}

async fn api_dashboard_artifacts(
    State(state): State<AppState>,
    Query(q): Query<ArtifactQ>,
) -> Response {
    let from = q.from.clone().unwrap_or_else(|| "1900-01-01".into());
    let to = q.to.clone().unwrap_or_else(|| "2100-01-01".into());
    let limit = q.limit.unwrap_or(30).clamp(1, 200);
    let kind = q.kind.clone();
    let tool = q.tool.clone();
    let db_path = state.db_path.clone();

    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let rows: Vec<Value> = match kind.as_str() {
            "write" => {
                let sql = format!(
                    "SELECT e.session_id,
                            regexp_extract(e.file_path, '.*/projects/([^/]+)/[^/]+\\.jsonl$', 1) AS project,
                            json_extract_string(acb.tool_input, '$.file_path') AS file_path,
                            LENGTH(json_extract_string(acb.tool_input, '$.content')) AS size_chars,
                            e.timestamp::VARCHAR AS ts
                     FROM assistant_content_blocks acb
                     JOIN entries e ON e.entry_id = acb.entry_id
                     WHERE acb.tool_name = 'Write'
                       AND CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
                       AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
                     ORDER BY size_chars DESC NULLS LAST
                     LIMIT {limit}"
                );
                let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
                stmt.query_map(
                    [&from, &to],
                    |row| Ok(json!({
                        "session_id": row.get::<_, Option<String>>(0)?,
                        "project":    row.get::<_, Option<String>>(1)?,
                        "file_path":  row.get::<_, Option<String>>(2)?,
                        "size_chars": row.get::<_, Option<i64>>(3)?,
                        "ts":         row.get::<_, Option<String>>(4)?,
                    }))
                ).map_err(|e| e.to_string())?
                .filter_map(|r| r.ok()).collect()
            }
            "agent" => {
                let sql = format!(
                    "SELECT e.session_id,
                            regexp_extract(e.file_path, '.*/projects/([^/]+)/[^/]+\\.jsonl$', 1) AS project,
                            COALESCE(json_extract_string(acb.tool_input, '$.subagent_type'), 'general-purpose') AS subagent_type,
                            json_extract_string(acb.tool_input, '$.description') AS description,
                            LENGTH(json_extract_string(acb.tool_input, '$.prompt')) AS size_chars,
                            e.timestamp::VARCHAR AS ts
                     FROM assistant_content_blocks acb
                     JOIN entries e ON e.entry_id = acb.entry_id
                     WHERE acb.tool_name = 'Agent'
                       AND CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
                       AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
                     ORDER BY size_chars DESC NULLS LAST
                     LIMIT {limit}"
                );
                let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
                stmt.query_map(
                    [&from, &to],
                    |row| Ok(json!({
                        "session_id":    row.get::<_, Option<String>>(0)?,
                        "project":       row.get::<_, Option<String>>(1)?,
                        "subagent_type": row.get::<_, Option<String>>(2)?,
                        "description":   row.get::<_, Option<String>>(3)?,
                        "size_chars":    row.get::<_, Option<i64>>(4)?,
                        "ts":            row.get::<_, Option<String>>(5)?,
                    }))
                ).map_err(|e| e.to_string())?
                .filter_map(|r| r.ok()).collect()
            }
            "tool_result" => {
                let has_tool = tool.as_deref().map(|t| !t.is_empty()).unwrap_or(false);
                let tool_filter = if has_tool { "AND acb.tool_name = ?" } else { "" };
                let sql = format!(
                    "SELECT e.session_id,
                            regexp_extract(e.file_path, '.*/projects/([^/]+)/[^/]+\\.jsonl$', 1) AS project,
                            acb.tool_name,
                            SUBSTR(json_extract_string(acb.tool_input, '$.file_path'), 1, 80) AS label_file,
                            SUBSTR(json_extract_string(acb.tool_input, '$.command'),   1, 80) AS label_cmd,
                            SUBSTR(json_extract_string(acb.tool_input, '$.url'),       1, 80) AS label_url,
                            SUBSTR(json_extract_string(acb.tool_input, '$.pattern'),   1, 80) AS label_pat,
                            LENGTH(CAST(ucb.tool_result_content AS VARCHAR)) AS size_chars,
                            e.timestamp::VARCHAR AS ts
                     FROM assistant_content_blocks acb
                     JOIN user_content_blocks ucb ON ucb.tool_use_id = acb.tool_use_id
                     JOIN entries e ON e.entry_id = ucb.entry_id
                     WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
                       AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
                       {tool_filter}
                     ORDER BY size_chars DESC NULLS LAST
                     LIMIT {limit}"
                );
                let mapper = |row: &duckdb::Row| Ok(json!({
                    "session_id": row.get::<_, Option<String>>(0)?,
                    "project":    row.get::<_, Option<String>>(1)?,
                    "tool_name":  row.get::<_, Option<String>>(2)?,
                    "label_file": row.get::<_, Option<String>>(3)?,
                    "label_cmd":  row.get::<_, Option<String>>(4)?,
                    "label_url":  row.get::<_, Option<String>>(5)?,
                    "label_pat":  row.get::<_, Option<String>>(6)?,
                    "size_chars": row.get::<_, Option<i64>>(7)?,
                    "ts":         row.get::<_, Option<String>>(8)?,
                }));
                let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
                if has_tool {
                    let t = tool.unwrap_or_default();
                    stmt.query_map([from.as_str(), to.as_str(), t.as_str()], mapper)
                } else {
                    stmt.query_map([from.as_str(), to.as_str()], mapper)
                }.map_err(|e| e.to_string())?
                .filter_map(|r| r.ok()).collect()
            }
            _ => return Err(format!("unknown kind: {kind}")),
        };

        Ok(json!({ "kind": kind, "rows": rows }))
    }).await;

    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_context_size(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;

        // Distribution
        let dist_sql = "
WITH per_turn AS (
  SELECT
    t.session_id,
    t.is_subagent,
    d.input_tokens
      + COALESCE(d.cache_read_input_tokens, 0)
      + COALESCE(d.cache_creation_5m, 0)
      + COALESCE(d.cache_creation_1h, 0)
      + CASE WHEN COALESCE(d.cache_creation_5m, 0) + COALESCE(d.cache_creation_1h, 0) = 0
             THEN COALESCE(d.cache_creation_input_tokens, 0)
             ELSE 0 END AS context_size
  FROM entries e
  JOIN transcripts t ON t.file_path = e.file_path
  JOIN assistant_entries_deduped d
    ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL
  WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
    AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
),
per_session AS (
  SELECT session_id, is_subagent, MAX(context_size) AS peak_ctx
  FROM per_turn
  GROUP BY session_id, is_subagent
)
SELECT
  CASE
    WHEN peak_ctx < 50000   THEN '<50k'
    WHEN peak_ctx < 100000  THEN '50-100k'
    WHEN peak_ctx < 200000  THEN '100-200k'
    WHEN peak_ctx < 500000  THEN '200-500k'
    ELSE '500k+'
  END AS bucket,
  COUNT(*) AS sessions,
  COUNT(*) FILTER (WHERE is_subagent) AS subagent_sessions
FROM per_session
GROUP BY bucket
ORDER BY
  CASE bucket
    WHEN '<50k' THEN 1 WHEN '50-100k' THEN 2 WHEN '100-200k' THEN 3
    WHEN '200-500k' THEN 4 ELSE 5 END";

        let mut stmt = conn.prepare(dist_sql).map_err(|e| e.to_string())?;
        let dist_rows: Vec<Value> = stmt
            .query_map([&from, &to], |row| {
                Ok(json!({
                    "bucket":           row.get::<_, String>(0)?,
                    "sessions":         row.get::<_, i64>(1)?,
                    "subagent_sessions":row.get::<_, i64>(2)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        // Big sessions
        let big_sql = "
WITH per_turn AS (
  SELECT
    t.session_id,
    t.is_subagent,
    regexp_extract(t.file_path, '.*/projects/([^/]+)/[^/]+\\.jsonl$', 1) AS project,
    d.input_tokens
      + COALESCE(d.cache_read_input_tokens, 0)
      + COALESCE(d.cache_creation_5m, 0)
      + COALESCE(d.cache_creation_1h, 0)
      + CASE WHEN COALESCE(d.cache_creation_5m, 0) + COALESCE(d.cache_creation_1h, 0) = 0
             THEN COALESCE(d.cache_creation_input_tokens, 0)
             ELSE 0 END AS context_size,
    d.cost_usd
  FROM entries e
  JOIN transcripts t ON t.file_path = e.file_path
  JOIN assistant_entries_deduped d
    ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL
  WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
    AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
),
per_session AS (
  SELECT session_id, is_subagent, project,
         MAX(context_size) AS peak_ctx,
         ROUND(SUM(cost_usd), 4) AS cost_usd,
         COUNT(*) AS turn_count
  FROM per_turn
  GROUP BY session_id, is_subagent, project
)
SELECT session_id, project, is_subagent, peak_ctx, cost_usd, turn_count
FROM per_session
WHERE peak_ctx >= 200000
ORDER BY cost_usd DESC
LIMIT 20";

        let mut stmt2 = conn.prepare(big_sql).map_err(|e| e.to_string())?;
        let big_rows: Vec<Value> = stmt2
            .query_map([&from, &to], |row| {
                Ok(json!({
                    "session_id":  row.get::<_, Option<String>>(0)?,
                    "project":     row.get::<_, Option<String>>(1)?,
                    "is_subagent": row.get::<_, Option<bool>>(2)?,
                    "peak_ctx":    row.get::<_, Option<i64>>(3)?,
                    "cost_usd":    row.get::<_, Option<f64>>(4)?,
                    "turn_count":  row.get::<_, Option<i64>>(5)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        Ok(json!({
            "distribution":  dist_rows,
            "big_sessions":  big_rows,
        }))
    })
    .await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_top_turns(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "WITH ranked AS (
               SELECT
                 d.entry_id,
                 t.session_id,
                 regexp_extract(t.file_path, '.*/projects/([^/]+)/[^/]+\\.jsonl$', 1) AS project,
                 t.is_subagent,
                 d.model,
                 ROUND(d.cost_usd, 4) AS cost_usd,
                 d.input_tokens,
                 d.output_tokens,
                 d.cache_read_input_tokens,
                 COALESCE(d.cache_creation_5m,0) + COALESCE(d.cache_creation_1h,0)
                   + CASE WHEN COALESCE(d.cache_creation_5m,0)+COALESCE(d.cache_creation_1h,0)=0
                          THEN COALESCE(d.cache_creation_input_tokens,0) ELSE 0 END AS cc_tokens,
                 d.tool_use_count,
                 e.timestamp::VARCHAR AS ts,
                 NTILE(100) OVER (ORDER BY d.cost_usd DESC) AS pct_bucket
               FROM entries e
               JOIN transcripts t ON t.file_path = e.file_path
               JOIN assistant_entries_deduped d
                 ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL
               WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
                 AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
             )
             SELECT entry_id, session_id, project, is_subagent,
                    model, cost_usd, input_tokens, output_tokens,
                    cache_read_input_tokens, cc_tokens, tool_use_count, ts
             FROM ranked
             WHERE pct_bucket = 1
             ORDER BY cost_usd DESC
             LIMIT 30",
            )
            .map_err(|e| e.to_string())?;

        let rows: Vec<Value> = stmt
            .query_map([&from, &to], |row| {
                Ok(json!({
                    "entry_id":          row.get::<_, Option<i64>>(0)?,
                    "session_id":        row.get::<_, Option<String>>(1)?,
                    "project":           row.get::<_, Option<String>>(2)?,
                    "is_subagent":       row.get::<_, Option<bool>>(3)?,
                    "model":             row.get::<_, Option<String>>(4)?,
                    "cost_usd":          row.get::<_, Option<f64>>(5)?,
                    "input_tokens":      row.get::<_, Option<i64>>(6)?,
                    "output_tokens":     row.get::<_, Option<i64>>(7)?,
                    "cache_read_tokens": row.get::<_, Option<i64>>(8)?,
                    "cc_tokens":         row.get::<_, Option<i64>>(9)?,
                    "tool_use_count":    row.get::<_, Option<i32>>(10)?,
                    "ts":                row.get::<_, Option<String>>(11)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        Ok(json!({ "rows": rows }))
    })
    .await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_two_regime(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "WITH per_session AS (
                   SELECT
                     t.session_id,
                     date_trunc('week', MIN(e.timestamp)) AS week,
                     SUM(d.cost_usd) AS cost_usd
                   FROM entries e
                   JOIN transcripts t ON t.file_path = e.file_path
                   JOIN assistant_entries_deduped d
                     ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL
                   WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
                     AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
                     AND NOT t.is_subagent
                   GROUP BY t.session_id
                 )
                 SELECT
                   week::VARCHAR AS week,
                   COUNT(*) AS session_count,
                   ROUND(AVG(cost_usd), 4) AS avg_cost,
                   ROUND(QUANTILE_CONT(cost_usd, 0.5), 4) AS median_cost,
                   ROUND(QUANTILE_CONT(cost_usd, 0.9), 4) AS p90_cost,
                   ROUND(SUM(cost_usd), 4) AS total_cost
                 FROM per_session
                 GROUP BY week
                 ORDER BY week",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<Value> = stmt
            .query_map([&from, &to], |row| {
                Ok(json!({
                    "week":          row.get::<_, Option<String>>(0)?,
                    "session_count": row.get::<_, i64>(1)?,
                    "avg_cost":      row.get::<_, f64>(2)?,
                    "median_cost":   row.get::<_, f64>(3)?,
                    "p90_cost":      row.get::<_, f64>(4)?,
                    "total_cost":    row.get::<_, f64>(5)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        Ok(Value::Array(rows))
    })
    .await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_first_turn_cc(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "WITH first_turns AS (
                   SELECT
                     t.session_id,
                     t.is_subagent,
                     FIRST_VALUE(
                       COALESCE(d.cache_creation_5m,0) + COALESCE(d.cache_creation_1h,0)
                       + CASE WHEN COALESCE(d.cache_creation_5m,0)+COALESCE(d.cache_creation_1h,0)=0
                              THEN COALESCE(d.cache_creation_input_tokens,0) ELSE 0 END
                     ) OVER (PARTITION BY t.session_id ORDER BY e.timestamp) AS first_cc,
                     ROW_NUMBER() OVER (PARTITION BY t.session_id ORDER BY e.timestamp) AS rn
                   FROM entries e
                   JOIN transcripts t ON t.file_path = e.file_path
                   JOIN assistant_entries_deduped d
                     ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL
                   WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
                     AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
                 )
                 SELECT
                   CASE
                     WHEN first_cc < 10000  THEN '<10k'
                     WHEN first_cc < 25000  THEN '10-25k'
                     WHEN first_cc < 50000  THEN '25-50k'
                     WHEN first_cc < 100000 THEN '50-100k'
                     ELSE '100k+'
                   END AS bucket,
                   COUNT(*) FILTER (WHERE NOT is_subagent) AS main_sessions,
                   COUNT(*) FILTER (WHERE is_subagent)     AS subagent_sessions,
                   ROUND(AVG(first_cc), 0)                 AS avg_cc
                 FROM first_turns
                 WHERE rn = 1
                 GROUP BY bucket
                 ORDER BY CASE bucket
                   WHEN '<10k' THEN 1 WHEN '10-25k' THEN 2 WHEN '25-50k' THEN 3
                   WHEN '50-100k' THEN 4 ELSE 5 END",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<Value> = stmt
            .query_map([&from, &to], |row| {
                Ok(json!({
                    "bucket":            row.get::<_, String>(0)?,
                    "main_sessions":     row.get::<_, i64>(1)?,
                    "subagent_sessions": row.get::<_, i64>(2)?,
                    "avg_cc":            row.get::<_, f64>(3)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        Ok(Value::Array(rows))
    })
    .await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_cache_invalidation(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;

        // Step 1: compute p90 threshold
        let threshold_p90: f64 = conn
            .query_row(
                "WITH base AS (
                   SELECT
                     (COALESCE(d.cache_creation_5m,0) + COALESCE(d.cache_creation_1h,0)
                      + CASE WHEN COALESCE(d.cache_creation_5m,0)+COALESCE(d.cache_creation_1h,0)=0
                             THEN COALESCE(d.cache_creation_input_tokens,0) ELSE 0 END) AS cc_total
                   FROM entries e
                   JOIN assistant_entries_deduped d
                     ON d.entry_id=e.entry_id AND d.message_id IS NOT NULL
                   WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
                     AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
                 )
                 SELECT QUANTILE_CONT(cc_total, 0.9) FROM base",
                [&from, &to],
                |row| row.get::<_, f64>(0),
            )
            .map_err(|e| e.to_string())?;

        // Step 2: gap × cc_type aggregation using the computed threshold
        let mut stmt = conn
            .prepare(
                "WITH seq AS (
                   SELECT
                     t.session_id,
                     d.cost_usd,
                     COALESCE(d.cache_creation_5m,0) AS cc5m,
                     COALESCE(d.cache_creation_1h,0) AS cc1h,
                     (COALESCE(d.cache_creation_5m,0) + COALESCE(d.cache_creation_1h,0)
                      + CASE WHEN COALESCE(d.cache_creation_5m,0)+COALESCE(d.cache_creation_1h,0)=0
                             THEN COALESCE(d.cache_creation_input_tokens,0) ELSE 0 END) AS cc_total,
                     e.timestamp,
                     LAG(e.timestamp) OVER (PARTITION BY t.session_id ORDER BY e.timestamp) AS prev_ts,
                     ROW_NUMBER() OVER (PARTITION BY t.session_id ORDER BY e.timestamp) AS rn
                   FROM entries e
                   JOIN transcripts t ON t.file_path = e.file_path
                   JOIN assistant_entries_deduped d
                     ON d.entry_id=e.entry_id AND d.message_id IS NOT NULL
                   WHERE NOT t.is_subagent
                     AND CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
                     AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
                 )
                 SELECT
                   CASE
                     WHEN prev_ts IS NULL THEN 'first-turn'
                     WHEN datediff('minute', prev_ts, timestamp) < 5   THEN '<5m'
                     WHEN datediff('minute', prev_ts, timestamp) < 55  THEN '5-55m'
                     WHEN datediff('minute', prev_ts, timestamp) < 65  THEN '55-65m'
                     ELSE '>65m'
                   END AS gap_bucket,
                   CASE
                     WHEN cc1h > cc5m THEN '1h-dominant'
                     WHEN cc5m > 0    THEN '5m-dominant'
                     ELSE 'legacy-cc'
                   END AS cc_type,
                   COUNT(*) AS events,
                   ROUND(SUM(cost_usd), 2) AS cost_usd
                 FROM seq
                 WHERE rn > 1 AND cc_total > ?
                 GROUP BY gap_bucket, cc_type
                 ORDER BY cost_usd DESC",
            )
            .map_err(|e| e.to_string())?;

        let events: Vec<Value> = stmt
            .query_map(
                duckdb::params![from.as_str(), to.as_str(), threshold_p90],
                |row| {
                    Ok(json!({
                        "gap_bucket": row.get::<_, String>(0)?,
                        "cc_type":    row.get::<_, String>(1)?,
                        "events":     row.get::<_, i64>(2)?,
                        "cost_usd":   row.get::<_, f64>(3)?,
                    }))
                },
            )
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        Ok(json!({
            "threshold_p90": threshold_p90,
            "events":        events,
        }))
    })
    .await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_compactions(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn.prepare(
            "WITH comp AS (
               SELECT s.session_id, e.timestamp AS comp_ts, s.summary, s.entry_id AS comp_entry_id
               FROM summary_entries s
               JOIN entries e ON e.uuid = s.leaf_uuid
               WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
                 AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
             ),
             next_turn AS (
               SELECT
                 c.session_id, c.comp_ts, c.summary,
                 ae.cost_usd,
                 ne.timestamp AS turn_ts,
                 regexp_extract(ne.file_path, '.*/projects/([^/]+)/[^/]+\\.jsonl$', 1) AS project,
                 datediff('minute', c.comp_ts, ne.timestamp) AS gap_min
               FROM comp c
               JOIN entries ne ON ne.session_id = c.session_id
                               AND ne.timestamp > c.comp_ts
               JOIN assistant_entries_deduped ae
                 ON ae.entry_id = ne.entry_id AND ae.message_id IS NOT NULL
               QUALIFY ROW_NUMBER() OVER (PARTITION BY c.session_id, c.comp_ts ORDER BY ne.timestamp) = 1
             )
             SELECT session_id, project, comp_ts::VARCHAR AS comp_ts, gap_min,
                    ROUND(cost_usd, 4) AS next_turn_cost,
                    SUBSTR(summary, 1, 120) AS summary_preview
             FROM next_turn
             ORDER BY next_turn_cost DESC
             LIMIT 50"
        ).map_err(|e| e.to_string())?;

        let rows: Vec<Value> = stmt.query_map([&from, &to], |row| {
            Ok(json!({
                "session_id":      row.get::<_, Option<String>>(0)?,
                "project":         row.get::<_, Option<String>>(1)?,
                "comp_ts":         row.get::<_, Option<String>>(2)?,
                "gap_min":         row.get::<_, Option<i64>>(3)?,
                "next_turn_cost":  row.get::<_, Option<f64>>(4)?,
                "summary_preview": row.get::<_, Option<String>>(5)?,
            }))
        }).map_err(|e| e.to_string())?
        .filter_map(|r| r.ok()).collect();

        Ok(json!({ "count": rows.len(), "events": rows }))
    }).await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_hour_of_day(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT EXTRACT('hour' FROM e.timestamp)::INT AS h,
                    ROUND(SUM(d.cost_usd), 2) AS cost_usd,
                    COUNT(DISTINCT t.session_id) AS session_count
             FROM entries e
             JOIN transcripts t ON t.file_path = e.file_path
             JOIN assistant_entries_deduped d
               ON d.entry_id = e.entry_id AND d.message_id IS NOT NULL
             WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
               AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
             GROUP BY 1 ORDER BY 1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([&from, &to], |row| {
                Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut by_hour = vec![(0.0_f64, 0_i64); 24];
        for r in rows.filter_map(|r| r.ok()) {
            let (h, cost, sessions) = r;
            if (0..24).contains(&h) {
                by_hour[h as usize] = (cost, sessions);
            }
        }
        let out: Vec<Value> = (0..24)
            .map(|h| {
                json!({
                    "hour": h, "cost_usd": by_hour[h].0, "session_count": by_hour[h].1,
                })
            })
            .collect();
        Ok(Value::Array(out))
    })
    .await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_hooks(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT shi.command,
                    COUNT(*) AS invocations,
                    ROUND(AVG(shi.duration_ms), 0) AS avg_duration_ms,
                    ROUND(SUM(shi.duration_ms) / 1000.0, 1) AS total_seconds
             FROM system_hook_infos shi
             JOIN entries e ON e.entry_id = shi.entry_id
             WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP)
               AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP)
             GROUP BY shi.command
             ORDER BY invocations DESC
             LIMIT 50",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<Value> = stmt
            .query_map([&from, &to], |row| {
                Ok(json!({
                    "command":         row.get::<_, Option<String>>(0)?,
                    "invocations":     row.get::<_, i64>(1)?,
                    "avg_duration_ms": row.get::<_, f64>(2)?,
                    "total_seconds":   row.get::<_, f64>(3)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        Ok(json!({ "rows": rows }))
    })
    .await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_skills(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT \
                   ais.skill_name, \
                   COUNT(*) AS invocations, \
                   COUNT(DISTINCT e.session_id) AS sessions \
                 FROM attachment_invoked_skills ais \
                 JOIN entries e ON e.entry_id = ais.entry_id \
                 WHERE CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP) \
                 GROUP BY ais.skill_name \
                 ORDER BY invocations DESC \
                 LIMIT 100",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<Value> = stmt
            .query_map([&from, &to], |row| {
                Ok(json!({
                    "skill_name":  row.get::<_, Option<String>>(0)?,
                    "invocations": row.get::<_, i64>(1)?,
                    "sessions":    row.get::<_, i64>(2)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        Ok(json!({ "rows": rows }))
    })
    .await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_bash(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;

        // Query 1: longest single Bash commands
        let mut stmt1 = conn
            .prepare(
                "SELECT \
                   e.session_id, \
                   regexp_extract(e.file_path, '.*/projects/([^/]+)/[^/]+\\.jsonl$', 1) AS project, \
                   LENGTH(json_extract_string(acb.tool_input, '$.command')) AS cmd_chars, \
                   SUBSTR(json_extract_string(acb.tool_input, '$.command'), 1, 200) AS cmd_preview, \
                   e.timestamp::VARCHAR AS ts \
                 FROM assistant_content_blocks acb \
                 JOIN entries e ON e.entry_id = acb.entry_id \
                 WHERE acb.tool_name = 'Bash' \
                   AND CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP) \
                 ORDER BY cmd_chars DESC NULLS LAST \
                 LIMIT 20",
            )
            .map_err(|e| e.to_string())?;
        let longest: Vec<Value> = stmt1
            .query_map([&from, &to], |row| {
                Ok(json!({
                    "session_id":  row.get::<_, Option<String>>(0)?,
                    "project":     row.get::<_, Option<String>>(1)?,
                    "cmd_chars":   row.get::<_, Option<i64>>(2)?,
                    "cmd_preview": row.get::<_, Option<String>>(3)?,
                    "ts":          row.get::<_, Option<String>>(4)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        // Query 2: most-repeated by first token
        let mut stmt2 = conn
            .prepare(
                "SELECT \
                   split_part(json_extract_string(acb.tool_input, '$.command'), ' ', 1) AS cmd_head, \
                   COUNT(*) AS calls, \
                   ROUND(AVG(LENGTH(CAST(ucb.tool_result_content AS VARCHAR))), 0) AS avg_result_chars \
                 FROM assistant_content_blocks acb \
                 LEFT JOIN user_content_blocks ucb ON ucb.tool_use_id = acb.tool_use_id \
                 JOIN entries e ON e.entry_id = acb.entry_id \
                 WHERE acb.tool_name = 'Bash' \
                   AND CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP) \
                 GROUP BY cmd_head \
                 ORDER BY calls DESC \
                 LIMIT 20",
            )
            .map_err(|e| e.to_string())?;
        let most_repeated: Vec<Value> = stmt2
            .query_map([&from, &to], |row| {
                Ok(json!({
                    "cmd_head":        row.get::<_, Option<String>>(0)?,
                    "calls":           row.get::<_, i64>(1)?,
                    "avg_result_chars":row.get::<_, f64>(2)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        Ok(json!({ "longest": longest, "most_repeated": most_repeated }))
    })
    .await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_read_sizes(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT \
                   json_extract_string(acb.tool_input, '$.file_path') AS file_path, \
                   LENGTH(CAST(ucb.tool_result_content AS VARCHAR)) AS result_chars, \
                   e.session_id, \
                   regexp_extract(e.file_path, '.*/projects/([^/]+)/[^/]+\\.jsonl$', 1) AS project, \
                   e.timestamp::VARCHAR AS ts \
                 FROM assistant_content_blocks acb \
                 JOIN user_content_blocks ucb ON ucb.tool_use_id = acb.tool_use_id \
                 JOIN entries e ON e.entry_id = ucb.entry_id \
                 WHERE acb.tool_name = 'Read' \
                   AND CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP) \
                 ORDER BY result_chars DESC \
                 LIMIT 50",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<Value> = stmt
            .query_map([&from, &to], |row| {
                Ok(json!({
                    "file_path":    row.get::<_, Option<String>>(0)?,
                    "result_chars": row.get::<_, i64>(1)?,
                    "session_id":   row.get::<_, Option<String>>(2)?,
                    "project":      row.get::<_, Option<String>>(3)?,
                    "ts":           row.get::<_, Option<String>>(4)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        Ok(json!({ "rows": rows }))
    })
    .await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_mcp_tools(
    State(state): State<AppState>,
    Query(q): Query<DashboardQ>,
) -> Response {
    let (from, to) = time_bounds(&q);
    let db_path = state.db_path.clone();
    let result = spawn_blocking(move || -> Result<Value, String> {
        let conn = open_db(&db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT \
                   acb.tool_name, \
                   regexp_extract(acb.tool_name, '^mcp__([^_]+)', 1) AS mcp_server, \
                   COUNT(*) AS calls, \
                   ROUND(AVG(LENGTH(CAST(ucb.tool_result_content AS VARCHAR))), 0) AS avg_result_chars, \
                   ROUND(MAX(LENGTH(CAST(ucb.tool_result_content AS VARCHAR))), 0) AS max_result_chars, \
                   ROUND(SUM(LENGTH(CAST(ucb.tool_result_content AS VARCHAR))) / 1e6, 2) AS total_mchars \
                 FROM assistant_content_blocks acb \
                 JOIN user_content_blocks ucb ON ucb.tool_use_id = acb.tool_use_id \
                 JOIN entries e ON e.entry_id = ucb.entry_id \
                 WHERE acb.tool_name LIKE 'mcp__%' \
                   AND CAST(e.timestamp AS TIMESTAMP) >= CAST(? AS TIMESTAMP) \
                   AND CAST(e.timestamp AS TIMESTAMP) <  CAST(? AS TIMESTAMP) \
                 GROUP BY acb.tool_name, mcp_server \
                 ORDER BY total_mchars DESC \
                 LIMIT 50",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<Value> = stmt
            .query_map([&from, &to], |row| {
                Ok(json!({
                    "tool_name":       row.get::<_, Option<String>>(0)?,
                    "mcp_server":      row.get::<_, Option<String>>(1)?,
                    "calls":           row.get::<_, i64>(2)?,
                    "avg_result_chars":row.get::<_, f64>(3)?,
                    "max_result_chars":row.get::<_, i64>(4)?,
                    "total_mchars":    row.get::<_, f64>(5)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        Ok(json!({ "rows": rows }))
    })
    .await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err500(e),
        Err(e) => err500(e),
    }
}

async fn api_dashboard_cost_decomposition(State(state): State<AppState>) -> Response {
    let cached = state.decomp.read().ok().and_then(|g| g.clone());
    match cached {
        Some(r) => Json(json!({
            "tree":               r.tree,
            "total_billed_usd":   r.total_billed_usd,
            "total_attributed_usd": r.total_attributed_usd,
            "days":               r.days,
            "computed_at":        r.computed_at_iso,
        }))
        .into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "cost decomposition not yet computed (recheck in a few seconds)",
        )
            .into_response(),
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run(args: ServeArgs) {
    let db_path = args.db.to_string_lossy().into_owned();
    let port = args.port;
    let decomp_days = args.decomp_days;

    // Open a single shared DuckDB connection. All session-list / transcript
    // handlers serialize access through Arc<Mutex<Connection>>; dashboard
    // endpoints still use open_db() for now (less hot path).
    let conn = Connection::open(&db_path).unwrap_or_else(|e| {
        eprintln!("open {db_path}: {e}");
        std::process::exit(1)
    });
    let db = Arc::new(Mutex::new(conn));

    // Precompute the session summary once so /api/sessions, /api/projects,
    // /api/sessions/meta are free.
    let initial = {
        let guard = db.lock().expect("db lock");
        match compute_summary(&guard) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("initial summary: {e}");
                std::process::exit(1)
            }
        }
    };
    println!(
        "summary: {} sessions, {} projects, {} distinct tools",
        initial.rows.len(),
        initial.projects.len(),
        initial.tools.len(),
    );

    let summary = Arc::new(RwLock::new(Arc::new(initial)));
    let transcript_cache = Arc::new(Mutex::new(LruCache::new(
        NonZeroUsize::new(TRANSCRIPT_CACHE_CAP).expect("cap > 0"),
    )));

    // Compute the cost-decomposition flamegraph synchronously at startup. This
    // takes seconds to a minute on a typical DB; it runs against a fresh
    // read-only connection (no contention with the shared one).
    let decomp: Arc<RwLock<Option<Arc<DecompResult>>>> = Arc::new(RwLock::new(None));
    {
        let path = db_path.clone();
        let started = std::time::Instant::now();
        match spawn_blocking(move || cost_decomp::compute(&path, decomp_days))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r)
        {
            Ok(r) => {
                println!(
                    "decomposition: ${:.2} attributed of ${:.2} billed over {}d ({:?})",
                    r.total_attributed_usd,
                    r.total_billed_usd,
                    r.days,
                    started.elapsed(),
                );
                if let Ok(mut g) = decomp.write() {
                    *g = Some(Arc::new(r));
                }
            }
            Err(e) => eprintln!("initial decomposition: {e}"),
        }
    }

    let state = AppState {
        db_path: db_path.clone(),
        db: db.clone(),
        summary: summary.clone(),
        transcript_cache: transcript_cache.clone(),
        decomp: decomp.clone(),
    };

    // Background task: poll DB file mtime. On change, reopen the shared
    // connection, rebuild summary, clear transcript cache.
    {
        let poll_path = db_path.clone();
        let poll_db = db.clone();
        let poll_summary = summary.clone();
        let poll_cache = transcript_cache.clone();
        tokio::spawn(async move {
            let mut last_mtime: Option<SystemTime> = std::fs::metadata(&poll_path)
                .ok()
                .and_then(|m| m.modified().ok());
            loop {
                tokio::time::sleep(Duration::from_secs(REFRESH_POLL_SECS)).await;
                let mtime = match std::fs::metadata(&poll_path).and_then(|m| m.modified()) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if last_mtime == Some(mtime) {
                    continue;
                }
                last_mtime = Some(mtime);

                let path = poll_path.clone();
                let db_inner = poll_db.clone();
                let rebuild = spawn_blocking(move || -> Result<SessionSummary, String> {
                    let new_conn =
                        Connection::open(&path).map_err(|e| format!("reopen {path}: {e}"))?;
                    let s = compute_summary(&new_conn)?;
                    let mut g = db_inner.lock().map_err(|e| format!("db lock: {e}"))?;
                    *g = new_conn;
                    Ok(s)
                })
                .await;

                match rebuild {
                    Ok(Ok(s)) => {
                        if let Ok(mut g) = poll_summary.write() {
                            *g = Arc::new(s);
                        }
                        if let Ok(mut c) = poll_cache.lock() {
                            c.clear();
                        }
                        eprintln!("db changed, summary refreshed, transcript cache cleared");
                    }
                    Ok(Err(e)) => eprintln!("refresh: {e}"),
                    Err(e) => eprintln!("refresh task: {e}"),
                }
            }
        });
    }

    // Independent refresh task for the cost-decomposition flamegraph. Polls the
    // same mtime; does its work on a fresh read-only connection so it doesn't
    // block other handlers. Runs after the summary rebuild so first-paint of
    // dashboard pages stays cheap.
    {
        let poll_path = db_path.clone();
        let poll_decomp = decomp.clone();
        tokio::spawn(async move {
            let mut last_mtime: Option<SystemTime> = std::fs::metadata(&poll_path)
                .ok()
                .and_then(|m| m.modified().ok());
            loop {
                tokio::time::sleep(Duration::from_secs(REFRESH_POLL_SECS)).await;
                let mtime = match std::fs::metadata(&poll_path).and_then(|m| m.modified()) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if last_mtime == Some(mtime) {
                    continue;
                }
                last_mtime = Some(mtime);

                let path = poll_path.clone();
                let result = spawn_blocking(move || cost_decomp::compute(&path, decomp_days))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r);
                match result {
                    Ok(r) => {
                        eprintln!(
                            "decomposition refreshed: ${:.2} attributed of ${:.2} billed",
                            r.total_attributed_usd, r.total_billed_usd
                        );
                        if let Ok(mut g) = poll_decomp.write() {
                            *g = Some(Arc::new(r));
                        }
                    }
                    Err(e) => eprintln!("decomposition refresh: {e}"),
                }
            }
        });
    }

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/assets/*path", get(serve_asset))
        .fallback(serve_index)
        .route("/api/projects", get(api_projects))
        .route("/api/sessions", get(api_sessions))
        .route("/api/sessions/meta", get(api_sessions_meta))
        .route("/api/transcript", get(api_transcript))
        .route("/api/subagent", get(api_subagent))
        .route("/api/dashboard/summary", get(api_dashboard_summary))
        .route("/api/dashboard/daily", get(api_dashboard_daily))
        .route("/api/dashboard/models", get(api_dashboard_models))
        .route("/api/dashboard/cache", get(api_dashboard_cache))
        .route("/api/dashboard/agents", get(api_dashboard_agents))
        .route(
            "/api/dashboard/top-sessions",
            get(api_dashboard_top_sessions),
        )
        .route(
            "/api/dashboard/session-distribution",
            get(api_dashboard_session_distribution),
        )
        .route(
            "/api/dashboard/file-hotspots",
            get(api_dashboard_file_hotspots),
        )
        .route("/api/dashboard/errors", get(api_dashboard_errors))
        .route("/api/dashboard/baseline", get(api_dashboard_baseline))
        .route(
            "/api/dashboard/token-streams",
            get(api_dashboard_token_streams),
        )
        .route("/api/dashboard/artifacts", get(api_dashboard_artifacts))
        .route(
            "/api/dashboard/context-size",
            get(api_dashboard_context_size),
        )
        .route("/api/dashboard/top-turns", get(api_dashboard_top_turns))
        .route("/api/dashboard/two-regime", get(api_dashboard_two_regime))
        .route(
            "/api/dashboard/first-turn-cc",
            get(api_dashboard_first_turn_cc),
        )
        .route(
            "/api/dashboard/cache-invalidation",
            get(api_dashboard_cache_invalidation),
        )
        .route("/api/dashboard/compactions", get(api_dashboard_compactions))
        .route("/api/dashboard/hour-of-day", get(api_dashboard_hour_of_day))
        .route("/api/dashboard/hooks", get(api_dashboard_hooks))
        .route("/api/dashboard/mcp-tools", get(api_dashboard_mcp_tools))
        .route("/api/dashboard/read-sizes", get(api_dashboard_read_sizes))
        .route("/api/dashboard/bash", get(api_dashboard_bash))
        .route("/api/dashboard/skills", get(api_dashboard_skills))
        .route(
            "/api/dashboard/cost-decomposition",
            get(api_dashboard_cost_decomposition),
        )
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
    println!("Claude Usage Visualizer → http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("bind {addr}: {e}");
            std::process::exit(1)
        });
    axum::serve(listener, app).await.unwrap_or_else(|e| {
        eprintln!("serve: {e}");
        std::process::exit(1)
    });
}
