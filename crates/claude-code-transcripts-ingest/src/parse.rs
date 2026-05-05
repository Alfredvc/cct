//! Parse one JSONL transcript file into a `ParsedFile` of typed row tuples.
//!
//! Each row is `Vec<serde_json::Value>` matching the column order of the
//! target table. `entry_id` placeholder lives at index 0 of every row; the
//! writer assigns globally-unique IDs from a sequence and patches them in.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::pricing::{compute_cost, PriceRow};
use claude_code_transcripts::types::{
    AssistantContentBlock, AttachmentData, CacheCreation, DocumentSource, Entry, ImageSource,
    UserContent, UserContentBlock,
};

/// Return type for per-entry row builders: an optional main-table row and
/// zero or more child-table rows, each keyed by table name.
type BuiltRows = (
    Option<(&'static str, Vec<Value>)>,
    Vec<(&'static str, Vec<Vec<Value>>)>,
);

// ─── Public types ────────────────────────────────────────────────────────

pub struct EntryRows {
    /// `entries` table row, entry_id placeholder at index 0.
    pub entry: Vec<Value>,
    /// (variant_table_name, row with entry_id placeholder at index 0).
    pub variant: Option<(&'static str, Vec<Value>)>,
    /// child rows per child-table (each row's entry_id placeholder at index 0).
    pub children: Vec<(&'static str, Vec<Vec<Value>>)>,
}

pub struct ParsedFile {
    pub transcript: TranscriptCols,
    pub entries: Vec<EntryRows>,
    pub failures: Vec<(usize, String)>,
    pub unknown_models: Vec<String>,
    /// Enum names that had unrecognised variants dropped during parsing.
    pub unknown_variants: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TranscriptCols {
    pub file_path: String,
    pub session_id: Option<String>,
    pub is_subagent: bool,
    pub agent_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub entry_count: u32,
    pub first_timestamp: Option<String>,
    pub last_timestamp: Option<String>,
    pub mtime: Option<String>,
}

// ─── Helpers ─────────────────────────────────────────────────────────────

fn s(v: Option<String>) -> Value {
    match v {
        Some(x) => Value::String(x),
        None => Value::Null,
    }
}

fn s_str(v: &str) -> Value {
    Value::String(v.to_string())
}

fn b(v: bool) -> Value {
    Value::Bool(v)
}

fn ob(v: Option<bool>) -> Value {
    match v {
        Some(x) => Value::Bool(x),
        None => Value::Null,
    }
}

fn u(v: u64) -> Value {
    Value::Number(serde_json::Number::from(v))
}

fn ou(v: Option<u64>) -> Value {
    match v {
        Some(x) => Value::Number(serde_json::Number::from(x)),
        None => Value::Null,
    }
}

fn ou32(v: Option<u32>) -> Value {
    match v {
        Some(x) => Value::Number(serde_json::Number::from(x as u64)),
        None => Value::Null,
    }
}

fn of(v: Option<f64>) -> Value {
    match v.and_then(serde_json::Number::from_f64) {
        Some(n) => Value::Number(n),
        None => Value::Null,
    }
}

fn json_str(v: &Value) -> Value {
    Value::String(serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()))
}

fn ojson(v: Option<&Value>) -> Value {
    match v {
        Some(x) => json_str(x),
        None => Value::Null,
    }
}

fn ojson_serializable<T: serde::Serialize>(v: Option<&T>) -> Value {
    match v {
        Some(x) => match serde_json::to_string(x) {
            Ok(s) => Value::String(s),
            Err(_) => Value::Null,
        },
        None => Value::Null,
    }
}

fn opt_opt_json(v: &Option<Option<Value>>) -> Value {
    match v {
        None => Value::Null,
        Some(None) => Value::String("null".to_string()),
        Some(Some(x)) => json_str(x),
    }
}

fn detect_subagent(path: &Path) -> (bool, Option<String>) {
    // Files under a "subagents" dir → subagent.
    let mut is_sub = false;
    let mut parent_id: Option<String> = None;
    for c in path.components() {
        if let std::path::Component::Normal(s) = c {
            if s == "subagents" {
                is_sub = true;
            }
        }
    }
    if is_sub {
        // parent session id = the .jsonl file in a sibling dir, if discoverable.
        // Best-effort: walk up two dirs and look for a directory name that is a session id.
        if let Some(grandparent) = path.parent().and_then(|p| p.parent()) {
            if let Some(name) = grandparent.file_name().and_then(|s| s.to_str()) {
                parent_id = Some(name.to_string());
            }
        }
    }
    (is_sub, parent_id)
}

fn extract_agent_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    // Pattern: agent-<id>
    stem.strip_prefix("agent-").map(|s| s.to_string())
}

fn iso_from_systime(t: std::time::SystemTime) -> Option<String> {
    let dt: DateTime<Utc> = t.into();
    Some(dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
}

// ─── Parsing entry-by-entry ──────────────────────────────────────────────

pub fn parse_file(path: &Path, pricing: &HashMap<String, PriceRow>) -> ParsedFile {
    let mut failures = Vec::new();
    let mut entries: Vec<EntryRows> = Vec::new();
    let mut first_ts: Option<String> = None;
    let mut last_ts: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut unknown_models: Vec<String> = Vec::new();
    let mut unknown_variants: Vec<String> = Vec::new();

    let mtime = fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(iso_from_systime);

    let file_path = path.to_string_lossy().to_string();
    let (is_subagent, parent_session_id) = detect_subagent(path);
    let agent_id_from_name = extract_agent_id_from_filename(path);

    let f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            failures.push((0, format!("open: {e}")));
            return ParsedFile {
                transcript: TranscriptCols {
                    file_path,
                    session_id: None,
                    is_subagent,
                    agent_id: agent_id_from_name,
                    parent_session_id,
                    entry_count: 0,
                    first_timestamp: None,
                    last_timestamp: None,
                    mtime,
                },
                entries,
                failures,
                unknown_models,
                unknown_variants,
            };
        }
    };

    for (idx, line) in BufReader::new(f).lines().enumerate() {
        let line_no = idx + 1;
        let raw_line = match line {
            Ok(l) => l,
            Err(e) => {
                failures.push((line_no, format!("io: {e}")));
                break;
            }
        };
        let cleaned: String = raw_line.chars().filter(|c| *c != '\0').collect();
        let trimmed = cleaned.trim();
        if trimmed.is_empty() {
            continue;
        }

        let raw_value: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                failures.push((line_no, format!("not json: {e}")));
                continue;
            }
        };

        let entry: Entry = match serde_json::from_value(raw_value.clone()) {
            Ok(e) => e,
            Err(e) => {
                failures.push((line_no, format!("typed parse: {e}")));
                continue;
            }
        };

        if matches!(entry, Entry::Unknown) {
            continue;
        }

        match build_rows(
            &entry,
            line_no as i64,
            &file_path,
            pricing,
            &mut session_id,
            &mut first_ts,
            &mut last_ts,
            &mut unknown_models,
            &mut unknown_variants,
        ) {
            Ok(rows) => entries.push(rows),
            Err(e) => failures.push((line_no, e)),
        }
    }

    let entry_count = entries.len() as u32;
    ParsedFile {
        transcript: TranscriptCols {
            file_path,
            session_id,
            is_subagent,
            agent_id: agent_id_from_name,
            parent_session_id,
            entry_count,
            first_timestamp: first_ts,
            last_timestamp: last_ts,
            mtime,
        },
        entries,
        failures,
        unknown_models,
        unknown_variants,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_rows(
    entry: &Entry,
    line_no: i64,
    file_path: &str,
    pricing: &HashMap<String, PriceRow>,
    session_id_out: &mut Option<String>,
    first_ts: &mut Option<String>,
    last_ts: &mut Option<String>,
    unknown_models: &mut Vec<String>,
    unknown_variants: &mut Vec<String>,
) -> Result<EntryRows, String> {
    // entries table column order:
    //   entry_id, file_path, line_no, type, subtype,
    //   uuid, parent_uuid, logical_parent_uuid, is_sidechain,
    //   session_id, timestamp, user_type, entrypoint, cwd, version,
    //   git_branch, slug, agent_id, team_name, agent_name, agent_color,
    //   prompt_id, is_meta, forked_from_uuid, forked_from_session_id
    let mut e_row: Vec<Value> = Vec::with_capacity(25);
    e_row.push(Value::Null); // entry_id placeholder
    e_row.push(s_str(file_path));
    e_row.push(Value::Number(serde_json::Number::from(line_no)));

    let (type_name, subtype) = entry_type_and_subtype(entry);
    e_row.push(s_str(type_name));
    e_row.push(s(subtype.clone()));

    if let Some(env) = envelope_of(entry) {
        e_row.push(s_str(&env.uuid));
        e_row.push(s(env.parent_uuid.clone()));
        e_row.push(s(env.logical_parent_uuid.clone()));
        e_row.push(b(env.is_sidechain));
        e_row.push(s_str(&env.session_id));
        e_row.push(s_str(&env.timestamp));
        e_row.push(s(env.user_type.clone()));
        e_row.push(s(env.entrypoint.clone()));
        e_row.push(s(env.cwd.clone()));
        e_row.push(s(env.version.clone()));
        e_row.push(s(env.git_branch.clone()));
        e_row.push(s(env.slug.clone()));
        e_row.push(s(env.agent_id.clone()));
        e_row.push(s(env.team_name.clone()));
        e_row.push(s(env.agent_name.clone()));
        e_row.push(s(env.agent_color.clone()));
        e_row.push(s(env.prompt_id.clone()));
        e_row.push(ob(env.is_meta));
        e_row.push(s(env.forked_from.as_ref().map(|f| f.message_uuid.clone())));
        e_row.push(s(env.forked_from.as_ref().map(|f| f.session_id.clone())));

        // capture session_id / timestamps for transcript header
        if session_id_out.is_none() {
            *session_id_out = Some(env.session_id.clone());
        }
        if first_ts.is_none() || first_ts.as_deref() > Some(&env.timestamp) {
            *first_ts = Some(env.timestamp.clone());
        }
        if last_ts.is_none() || last_ts.as_deref() < Some(&env.timestamp) {
            *last_ts = Some(env.timestamp.clone());
        }
    } else {
        for _ in 0..20 {
            e_row.push(Value::Null);
        }
        // Some metadata-only entries carry session_id directly.
        if let Some(sid) = metadata_session_id(entry) {
            e_row[9] = s_str(sid); // session_id slot
            if session_id_out.is_none() {
                *session_id_out = Some(sid.to_string());
            }
        }
    }

    // Variant + child rows
    let (variant, children) = build_variant(entry, pricing, unknown_models, unknown_variants)?;

    Ok(EntryRows {
        entry: e_row,
        variant,
        children,
    })
}

fn entry_type_and_subtype(e: &Entry) -> (&'static str, Option<String>) {
    match e {
        Entry::User(_) => ("user", None),
        Entry::Assistant(_) => ("assistant", None),
        Entry::System(s) => (
            "system",
            Some(
                serde_json::to_value(&s.subtype)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| "unknown".to_string()),
            ),
        ),
        Entry::Attachment(_) => ("attachment", None),
        Entry::Progress(_) => ("progress", None),
        Entry::PermissionMode(_) => ("permission-mode", None),
        Entry::LastPrompt(_) => ("last-prompt", None),
        Entry::AiTitle(_) => ("ai-title", None),
        Entry::CustomTitle(_) => ("custom-title", None),
        Entry::AgentName(_) => ("agent-name", None),
        Entry::AgentColor(_) => ("agent-color", None),
        Entry::AgentSetting(_) => ("agent-setting", None),
        Entry::Tag(_) => ("tag", None),
        Entry::Summary(_) => ("summary", None),
        Entry::TaskSummary(_) => ("task-summary", None),
        Entry::PrLink(_) => ("pr-link", None),
        Entry::Mode(_) => ("mode", None),
        Entry::WorktreeState(_) => ("worktree-state", None),
        Entry::ContentReplacement(_) => ("content-replacement", None),
        Entry::FileHistorySnapshot(_) => ("file-history-snapshot", None),
        Entry::AttributionSnapshot(_) => ("attribution-snapshot", None),
        Entry::QueueOperation(_) => ("queue-operation", None),
        Entry::ContextCollapseCommit(_) => ("marble-origami-commit", None),
        Entry::ContextCollapseSnapshot(_) => ("marble-origami-snapshot", None),
        Entry::SpeculationAccept(_) => ("speculation-accept", None),
        Entry::Unknown => unreachable!("Unknown entries are skipped before build_rows"),
    }
}

fn envelope_of(e: &Entry) -> Option<&claude_code_transcripts::types::Envelope> {
    match e {
        Entry::User(x) => Some(&x.envelope),
        Entry::Assistant(x) => Some(&x.envelope),
        Entry::System(x) => Some(&x.envelope),
        Entry::Attachment(x) => Some(&x.envelope),
        Entry::Progress(x) => Some(&x.envelope),
        _ => None,
    }
}

fn metadata_session_id(e: &Entry) -> Option<&str> {
    match e {
        Entry::PermissionMode(x) => Some(&x.session_id),
        Entry::LastPrompt(x) => Some(&x.session_id),
        Entry::AiTitle(x) => Some(&x.session_id),
        Entry::CustomTitle(x) => Some(&x.session_id),
        Entry::AgentName(x) => Some(&x.session_id),
        Entry::AgentColor(x) => Some(&x.session_id),
        Entry::AgentSetting(x) => Some(&x.session_id),
        Entry::Tag(x) => Some(&x.session_id),
        Entry::Summary(x) => Some(&x.session_id),
        Entry::TaskSummary(x) => Some(&x.session_id),
        Entry::PrLink(x) => Some(&x.session_id),
        Entry::Mode(x) => Some(&x.session_id),
        Entry::WorktreeState(x) => Some(&x.session_id),
        Entry::ContentReplacement(x) => Some(&x.session_id),
        Entry::QueueOperation(x) => Some(&x.session_id),
        Entry::ContextCollapseCommit(x) => Some(&x.session_id),
        Entry::ContextCollapseSnapshot(x) => Some(&x.session_id),
        _ => None,
    }
}

#[allow(clippy::type_complexity)]
fn build_variant(
    e: &Entry,
    pricing: &HashMap<String, PriceRow>,
    unknown_models: &mut Vec<String>,
    unknown_variants: &mut Vec<String>,
) -> Result<BuiltRows, String> {
    match e {
        Entry::User(u) => Ok(build_user(u, unknown_variants)),
        Entry::Assistant(a) => Ok(build_assistant(
            a,
            pricing,
            unknown_models,
            unknown_variants,
        )),
        Entry::System(s) => Ok(build_system(s)),
        Entry::Attachment(a) => Ok(build_attachment(a, unknown_variants)),
        Entry::Progress(p) => Ok(build_progress(p)),
        Entry::PermissionMode(x) => Ok((
            Some((
                "permission_mode_entries",
                vec![Value::Null, s_str(&x.permission_mode), s_str(&x.session_id)],
            )),
            vec![],
        )),
        Entry::LastPrompt(x) => {
            if x.last_prompt.is_none() && x.leaf_uuid.is_none() {
                // Possible Claude Code format change (e.g. new required discriminator).
                // Surfaces in the existing unknown_variants ingest summary.
                unknown_variants.push("last-prompt:no-fields".to_string());
            }
            Ok((
                Some((
                    "last_prompt_entries",
                    vec![
                        Value::Null,
                        x.leaf_uuid.as_deref().map_or(Value::Null, s_str),
                        x.last_prompt.as_deref().map_or(Value::Null, s_str),
                        s_str(&x.session_id),
                    ],
                )),
                vec![],
            ))
        }
        Entry::AiTitle(x) => Ok((
            Some((
                "ai_title_entries",
                vec![Value::Null, s_str(&x.ai_title), s_str(&x.session_id)],
            )),
            vec![],
        )),
        Entry::CustomTitle(x) => Ok((
            Some((
                "custom_title_entries",
                vec![Value::Null, s_str(&x.custom_title), s_str(&x.session_id)],
            )),
            vec![],
        )),
        Entry::AgentName(x) => Ok((
            Some((
                "agent_name_entries",
                vec![Value::Null, s_str(&x.agent_name), s_str(&x.session_id)],
            )),
            vec![],
        )),
        Entry::AgentColor(x) => Ok((
            Some((
                "agent_color_entries",
                vec![Value::Null, s_str(&x.agent_color), s_str(&x.session_id)],
            )),
            vec![],
        )),
        Entry::AgentSetting(x) => Ok((
            Some((
                "agent_setting_entries",
                vec![Value::Null, s_str(&x.agent_setting), s_str(&x.session_id)],
            )),
            vec![],
        )),
        Entry::Tag(x) => Ok((
            Some((
                "tag_entries",
                vec![Value::Null, s_str(&x.tag), s_str(&x.session_id)],
            )),
            vec![],
        )),
        Entry::Summary(x) => Ok((
            Some((
                "summary_entries",
                vec![
                    Value::Null,
                    s_str(&x.leaf_uuid),
                    s_str(&x.summary),
                    s_str(&x.session_id),
                ],
            )),
            vec![],
        )),
        Entry::TaskSummary(x) => Ok((
            Some((
                "task_summary_entries",
                vec![
                    Value::Null,
                    s_str(&x.summary),
                    s_str(&x.session_id),
                    s_str(&x.timestamp),
                ],
            )),
            vec![],
        )),
        Entry::PrLink(x) => Ok((
            Some((
                "pr_link_entries",
                vec![
                    Value::Null,
                    s_str(&x.session_id),
                    u(x.pr_number as u64),
                    s_str(&x.pr_url),
                    s_str(&x.pr_repository),
                    s_str(&x.timestamp),
                ],
            )),
            vec![],
        )),
        Entry::Mode(x) => Ok((
            Some((
                "mode_entries",
                vec![
                    Value::Null,
                    s_str(
                        serde_json::to_value(&x.mode)
                            .ok()
                            .and_then(|v| v.as_str().map(|s| s.to_string()))
                            .unwrap_or_default()
                            .as_str(),
                    ),
                    s_str(&x.session_id),
                ],
            )),
            vec![],
        )),
        Entry::WorktreeState(x) => Ok((
            Some((
                "worktree_state_entries",
                vec![
                    Value::Null,
                    s_str(&x.session_id),
                    ojson_serializable(x.worktree_session.as_ref()),
                ],
            )),
            vec![],
        )),
        Entry::ContentReplacement(x) => Ok((
            Some((
                "content_replacement_entries",
                vec![
                    Value::Null,
                    s_str(&x.session_id),
                    json_str(&json!(x.replacements)),
                    s(x.agent_id.clone()),
                ],
            )),
            vec![],
        )),
        Entry::FileHistorySnapshot(x) => Ok((
            Some((
                "file_history_snapshot_entries",
                vec![
                    Value::Null,
                    s_str(&x.message_id),
                    ojson_serializable(Some(&x.snapshot)),
                    b(x.is_snapshot_update),
                ],
            )),
            vec![],
        )),
        Entry::AttributionSnapshot(x) => Ok((
            Some((
                "attribution_snapshot_entries",
                vec![
                    Value::Null,
                    s_str(&x.message_id),
                    s_str(&x.surface),
                    json_str(&x.file_states),
                    ou32(x.prompt_count),
                    ou32(x.prompt_count_at_last_commit),
                    ou32(x.permission_prompt_count),
                    ou32(x.permission_prompt_count_at_last_commit),
                    ou32(x.escape_count),
                    ou32(x.escape_count_at_last_commit),
                ],
            )),
            vec![],
        )),
        Entry::QueueOperation(x) => Ok((
            Some((
                "queue_operation_entries",
                vec![
                    Value::Null,
                    s_str(&x.operation),
                    s_str(&x.timestamp),
                    s_str(&x.session_id),
                    s(x.content.clone()),
                ],
            )),
            vec![],
        )),
        Entry::ContextCollapseCommit(x) => Ok((
            Some((
                "marble_origami_commit_entries",
                vec![
                    Value::Null,
                    s_str(&x.session_id),
                    s_str(&x.collapse_id),
                    s_str(&x.summary_uuid),
                    s_str(&x.summary_content),
                    s_str(&x.summary),
                    s_str(&x.first_archived_uuid),
                    s_str(&x.last_archived_uuid),
                ],
            )),
            vec![],
        )),
        Entry::ContextCollapseSnapshot(x) => Ok((
            Some((
                "marble_origami_snapshot_entries",
                vec![
                    Value::Null,
                    s_str(&x.session_id),
                    json_str(&json!(x.staged)),
                    b(x.armed),
                    u(x.last_spawn_tokens),
                ],
            )),
            vec![],
        )),
        Entry::SpeculationAccept(x) => Ok((
            Some((
                "speculation_accept_entries",
                vec![Value::Null, s_str(&x.timestamp), u(x.time_saved_ms)],
            )),
            vec![],
        )),
        Entry::Unknown => unreachable!("Unknown entries are skipped before build_rows"),
    }
}

fn build_user(
    ue: &claude_code_transcripts::types::UserEntry,
    unknown_variants: &mut Vec<String>,
) -> BuiltRows {
    let role = serde_json::to_value(&ue.message.role)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();

    let (content_text, has_blocks, blocks): (Option<String>, bool, Vec<UserContentBlock>) =
        match &ue.message.content {
            UserContent::Text(t) => (Some(t.clone()), false, vec![]),
            UserContent::Blocks(bs) => (None, true, bs.clone()),
            UserContent::Other(_) => (None, false, vec![]),
        };

    let row = vec![
        Value::Null,                        // entry_id
        s_str(&role),                       // message_role
        s(content_text),                    // message_content_text
        b(has_blocks),                      // message_has_blocks
        ojson(ue.tool_use_result.as_ref()), // tool_use_result
        s(ue.source_tool_assistant_uuid.clone()),
        s(ue.source_tool_use_id.clone()),
        s(ue.permission_mode.clone()),
        ojson(ue.origin.as_ref()),
        ob(ue.is_compact_summary),
        ob(ue.is_visible_in_transcript_only),
        match &ue.image_paste_ids {
            Some(v) => json_str(&json!(v)),
            None => Value::Null,
        },
        s(ue.plan_content.clone()),
    ];

    let mut child_rows: Vec<Vec<Value>> = Vec::new();
    for (idx, block) in blocks.iter().enumerate() {
        let pos = idx as u64;
        let row = match block {
            UserContentBlock::Text { text } => vec![
                Value::Null, // entry_id
                u(pos),
                s_str("text"),
                s_str(text),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
            ],
            UserContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => vec![
                Value::Null,
                u(pos),
                s_str("tool_result"),
                Value::Null,
                s_str(tool_use_id),
                json_str(content),
                ob(*is_error),
                Value::Null,
                Value::Null,
                Value::Null,
            ],
            UserContentBlock::Image { source } => {
                let source_json = if matches!(source, ImageSource::Unknown) {
                    unknown_variants.push("ImageSource".to_string());
                    Value::Null
                } else {
                    ojson_serializable(Some(source))
                };
                vec![
                    Value::Null,
                    u(pos),
                    s_str("image"),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    source_json,
                    Value::Null,
                    Value::Null,
                ]
            }
            UserContentBlock::Document { source, title } => {
                let source_json = if matches!(source, DocumentSource::Unknown) {
                    unknown_variants.push("DocumentSource".to_string());
                    Value::Null
                } else {
                    ojson_serializable(Some(source))
                };
                vec![
                    Value::Null,
                    u(pos),
                    s_str("document"),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    source_json,
                    s(title.clone()),
                ]
            }
            UserContentBlock::Unknown => {
                unknown_variants.push("UserContentBlock".to_string());
                continue;
            }
        };
        let _ = (
            std::any::type_name::<ImageSource>(),
            std::any::type_name::<DocumentSource>(),
        ); // keep imports
        child_rows.push(row);
    }

    let children = if child_rows.is_empty() {
        vec![]
    } else {
        vec![("user_content_blocks", child_rows)]
    };
    (Some(("user_entries", row)), children)
}

fn build_assistant(
    ae: &claude_code_transcripts::types::AssistantEntry,
    pricing: &HashMap<String, PriceRow>,
    unknown_models: &mut Vec<String>,
    unknown_variants: &mut Vec<String>,
) -> BuiltRows {
    let m = &ae.message;
    let role = serde_json::to_value(&m.role)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();

    let usage = &m.usage;
    let (cache_5m, cache_1h) = match &usage.cache_creation {
        Some(c) => (c.ephemeral_5m_input_tokens, c.ephemeral_1h_input_tokens),
        None => (None, None),
    };
    let (web_search, web_fetch) = match &usage.server_tool_use {
        Some(stu) => (Some(stu.web_search_requests), Some(stu.web_fetch_requests)),
        None => (None, None),
    };

    let model_str = m.model.as_deref().unwrap_or("");

    let (cmr_type, cmr_tokens): (Option<String>, Option<u64>) =
        match m.diagnostics.as_ref().and_then(|outer| outer.as_ref()) {
            Some(diag) => match diag.cache_miss_reason.as_ref() {
                Some(cmr) => (Some(cmr.kind.clone()), cmr.cache_missed_input_tokens),
                None => (None, None),
            },
            None => (None, None),
        };

    let cost = compute_cost(
        pricing,
        model_str,
        usage.input_tokens,
        usage.output_tokens,
        cache_5m,
        cache_1h,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
    );
    if cost.is_none() && !model_str.is_empty() && !unknown_models.iter().any(|s| s == model_str) {
        unknown_models.push(model_str.to_string());
    }

    let tool_use_count = m
        .content
        .iter()
        .filter(|b| matches!(b, AssistantContentBlock::ToolUse { .. }))
        .count();

    let row = vec![
        Value::Null,                                 // entry_id
        s_str(&m.id),                                // message_id
        s_str(&role),                                // role
        s_str(model_str),                            // model
        opt_opt_json(&m.container),                  // container
        s(m.stop_reason.clone()),                    // stop_reason
        s(m.stop_sequence.clone()),                  // stop_sequence
        opt_opt_json(&m.stop_details),               // stop_details
        opt_opt_json(&m.context_management),         // context_management
        s(ae.request_id.clone()),                    // request_id
        ob(ae.is_api_error_message),                 // is_api_error_message
        ou32(ae.api_error_status.map(|v| v as u32)), // api_error_status
        s(ae.error.clone()),                         // error
        u(tool_use_count as u64),                    // tool_use_count
        of(cost),                                    // cost_usd
        u(usage.input_tokens),                       // input_tokens
        u(usage.output_tokens),                      // output_tokens
        ou(usage.cache_creation_input_tokens),       // cache_creation_input_tokens
        ou(usage.cache_read_input_tokens),           // cache_read_input_tokens
        ou(cache_5m),                                // cache_creation_5m
        ou(cache_1h),                                // cache_creation_1h
        ou(web_search),                              // web_search_requests
        ou(web_fetch),                               // web_fetch_requests
        opt_opt_json(&usage.service_tier),           // service_tier
        opt_opt_json(&usage.inference_geo),          // inference_geo
        opt_opt_json(&usage.iterations),             // iterations
        opt_opt_json(&usage.speed),                  // speed
        s(ae.attribution_agent.clone()),             // attribution_agent
        s(ae.attribution_plugin.clone()),            // attribution_plugin
        s(ae.attribution_skill.clone()),             // attribution_skill
        s(cmr_type),                                 // cache_miss_reason_type
        ou(cmr_tokens),                              // cache_missed_input_tokens
    ];

    let mut block_rows: Vec<Vec<Value>> = Vec::new();
    for (idx, block) in m.content.iter().enumerate() {
        let pos = idx as u64;
        let row = match block {
            AssistantContentBlock::Text { text } => vec![
                Value::Null,
                u(pos),
                s_str("text"),
                s_str(text),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
            ],
            AssistantContentBlock::Thinking {
                thinking,
                signature,
            } => vec![
                Value::Null,
                u(pos),
                s_str("thinking"),
                Value::Null,
                s_str(thinking),
                s_str(signature),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
            ],
            AssistantContentBlock::RedactedThinking { data } => vec![
                Value::Null,
                u(pos),
                s_str("redacted_thinking"),
                Value::Null,
                Value::Null,
                Value::Null,
                s_str(data),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
            ],
            AssistantContentBlock::ToolUse {
                id,
                name,
                input,
                caller,
            } => vec![
                Value::Null,
                u(pos),
                s_str("tool_use"),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                s_str(id),
                s_str(name),
                json_str(input),
                s(caller.as_ref().map(|c| c.caller_type.clone())),
            ],
            AssistantContentBlock::Unknown => {
                unknown_variants.push("AssistantContentBlock".to_string());
                continue;
            }
        };
        block_rows.push(row);
    }

    // Usage iterations: only when usage.iterations is a typed array.
    let mut iter_rows: Vec<Vec<Value>> = Vec::new();
    if let Some(Some(Value::Array(arr))) = usage.iterations.as_ref().map(|o| o.as_ref()) {
        for (idx, iter) in arr.iter().enumerate() {
            let pos = idx as u64;
            let it = iter.as_object();
            let get_u =
                |k: &str| -> Option<u64> { it.and_then(|o| o.get(k)).and_then(|v| v.as_u64()) };
            let cc = it.and_then(|o| o.get("cache_creation"));
            let cc5 = cc.and_then(|c| c.get("ephemeral_5m_input_tokens").and_then(|v| v.as_u64()));
            let cc1 = cc.and_then(|c| c.get("ephemeral_1h_input_tokens").and_then(|v| v.as_u64()));
            iter_rows.push(vec![
                Value::Null,
                u(pos),
                s(it.and_then(|o| o.get("type"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())),
                ou(get_u("input_tokens")),
                ou(get_u("output_tokens")),
                ou(get_u("cache_read_input_tokens")),
                ou(get_u("cache_creation_input_tokens")),
                ou(cc5),
                ou(cc1),
            ]);
        }
    }

    let mut children: Vec<(&'static str, Vec<Vec<Value>>)> = Vec::new();
    if !block_rows.is_empty() {
        children.push(("assistant_content_blocks", block_rows));
    }
    if !iter_rows.is_empty() {
        children.push(("assistant_usage_iterations", iter_rows));
    }
    let _ = CacheCreation {
        ephemeral_1h_input_tokens: None,
        ephemeral_5m_input_tokens: None,
    }; // keep import
    (Some(("assistant_entries", row)), children)
}

fn build_system(se: &claude_code_transcripts::types::SystemEntry) -> BuiltRows {
    let subtype_str = serde_json::to_value(&se.subtype)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "unknown".to_string());

    let cm = se.compact_metadata.as_ref();
    let row = vec![
        Value::Null, // entry_id
        s_str(&subtype_str),
        s(se.content.clone()),
        s(se.level.clone()),
        ob(se.is_meta),
        ojson(se.cause.as_ref()),
        ojson(se.error.as_ref()),
        of(se.retry_in_ms),
        ou32(se.retry_attempt),
        ou32(se.max_retries),
        ou32(se.hook_count),
        match &se.hook_errors {
            Some(v) => json_str(&json!(v)),
            None => Value::Null,
        },
        ob(se.prevented_continuation),
        s(se.stop_reason.clone()),
        ob(se.has_output),
        s(se.tool_use_id.clone()),
        of(se.duration_ms),
        ou32(se.message_count),
        s(se.url.clone()),
        s(se.upgrade_nudge.clone()),
        // compact_metadata flattened
        s(cm.map(|c| c.trigger.clone())),
        ou(cm.and_then(|c| c.pre_tokens)),
        ou(cm.and_then(|c| c.post_tokens)),
        ou(cm.and_then(|c| c.duration_ms)),
        s(cm.and_then(|c| c.preserved_segment.as_ref().map(|p| p.head_uuid.clone()))),
        s(cm.and_then(|c| c.preserved_segment.as_ref().map(|p| p.anchor_uuid.clone()))),
        s(cm.and_then(|c| c.preserved_segment.as_ref().map(|p| p.tail_uuid.clone()))),
        match cm.and_then(|c| c.pre_compact_discovered_tools.as_ref()) {
            Some(v) => json_str(&json!(v)),
            None => Value::Null,
        },
    ];

    let mut hook_rows: Vec<Vec<Value>> = Vec::new();
    if let Some(infos) = &se.hook_infos {
        for (idx, hi) in infos.iter().enumerate() {
            hook_rows.push(vec![
                Value::Null,
                u(idx as u64),
                s_str(&hi.command),
                ou(hi.duration_ms),
            ]);
        }
    }
    let children = if hook_rows.is_empty() {
        vec![]
    } else {
        vec![("system_hook_infos", hook_rows)]
    };
    (Some(("system_entries", row)), children)
}

fn build_attachment(
    ae: &claude_code_transcripts::types::AttachmentEntry,
    unknown_variants: &mut Vec<String>,
) -> BuiltRows {
    use AttachmentData::*;
    // Initialise wide row as all NULL then fill the relevant slots.
    let mut row: Vec<Value> = vec![Value::Null; 48];
    // index 0 = entry_id placeholder, index 1 = attachment_type
    let mut diag_rows: Vec<Vec<Value>> = Vec::new();
    let mut skill_rows: Vec<Vec<Value>> = Vec::new();

    let attach_type = match &ae.attachment {
        HookSuccess(_) => "hook_success",
        HookNonBlockingError(_) => "hook_non_blocking_error",
        HookBlockingError(_) => "hook_blocking_error",
        HookCancelled(_) => "hook_cancelled",
        HookAdditionalContext { .. } => "hook_additional_context",
        HookPermissionDecision { .. } => "hook_permission_decision",
        HookStoppedContinuation { .. } => "hook_stopped_continuation",
        HookSystemMessage { .. } => "hook_system_message",
        File { .. } => "file",
        EditedTextFile { .. } => "edited_text_file",
        Directory { .. } => "directory",
        CompactFileReference { .. } => "compact_file_reference",
        CommandPermissions { .. } => "command_permissions",
        PlanMode { .. } => "plan_mode",
        PlanModeExit { .. } => "plan_mode_exit",
        PlanFileReference { .. } => "plan_file_reference",
        AutoMode { .. } => "auto_mode",
        AutoModeExit => "auto_mode_exit",
        SkillListing { .. } => "skill_listing",
        DynamicSkill { .. } => "dynamic_skill",
        InvokedSkills { .. } => "invoked_skills",
        TaskReminder { .. } => "task_reminder",
        TodoReminder { .. } => "todo_reminder",
        Diagnostics { .. } => "diagnostics",
        DateChange { .. } => "date_change",
        DeferredToolsDelta { .. } => "deferred_tools_delta",
        AgentListingDelta { .. } => "agent_listing_delta",
        McpInstructionsDelta { .. } => "mcp_instructions_delta",
        UltrathinkEffort { .. } => "ultrathink_effort",
        QueuedCommand { .. } => "queued_command",
        NestedMemory { .. } => "nested_memory",
        Unknown => "unknown",
    };
    row[1] = s_str(attach_type);

    // Column layout (matches schema.rs order):
    //  2 hook_name, 3 tool_use_id, 4 hook_event, 5 hook_content, 6 hook_stdout,
    //  7 hook_stderr, 8 hook_exit_code, 9 hook_command, 10 hook_duration_ms,
    // 11 decision, 12 filename, 13 file_content_text, 14 file_content_metadata,
    // 15 display_path, 16 directory_path, 17 directory_content,
    // 18 command_allowed_tools, 19 plan_reminder_type, 20 plan_is_sub_agent,
    // 21 plan_file_path, 22 plan_exists, 23 skill_listing_content,
    // 24 skill_listing_is_initial, 25 skill_listing_count, 26 skill_dir,
    // 27 skill_names, 28 invoked_skills, 29 task_reminder_content,
    // 30 task_reminder_item_count, 31 diagnostics_files, 32 diagnostics_is_new,
    // 33 date_change_new_date, 34 deferred_added_names, 35 deferred_added_lines,
    // 36 deferred_removed_names, 37 mcp_added_names, 38 mcp_added_blocks,
    // 39 mcp_removed_names, 40 ultrathink_level, 41 queued_command_prompt,
    // 42 queued_command_mode, 43 nested_memory_path, 44 nested_memory_memory_type,
    // 45 nested_memory_content, 46 nested_memory_differs_from_disk,
    // 47 deferred_readded_names

    let fill_hook = |row: &mut Vec<Value>,
                     h: &claude_code_transcripts::types::HookResultAttachment| {
        row[2] = s(h.hook_name.clone());
        row[3] = s(h.tool_use_id.clone());
        row[4] = s(h.hook_event.clone());
        row[5] = s(h.content.clone());
        row[6] = s(h.stdout.clone());
        row[7] = s(h.stderr.clone());
        row[8] = match h.exit_code {
            Some(v) => Value::Number(serde_json::Number::from(v as i64)),
            None => Value::Null,
        };
        row[9] = s(h.command.clone());
        row[10] = ou(h.duration_ms);
    };

    match &ae.attachment {
        HookSuccess(h) | HookNonBlockingError(h) | HookBlockingError(h) | HookCancelled(h) => {
            fill_hook(&mut row, h);
        }
        HookAdditionalContext {
            content,
            hook_name,
            tool_use_id,
            hook_event,
        } => {
            row[2] = s(hook_name.clone());
            row[3] = s(tool_use_id.clone());
            row[4] = s(hook_event.clone());
            // content (list of strings) → store joined into hook_content for searchability
            row[5] = s_str(&content.join("\n"));
        }
        HookPermissionDecision {
            decision,
            hook_name,
            tool_use_id,
            hook_event,
        } => {
            row[2] = s(hook_name.clone());
            row[3] = s(tool_use_id.clone());
            row[4] = s(hook_event.clone());
            row[11] = s_str(decision);
        }
        HookStoppedContinuation {
            message,
            hook_name,
            tool_use_id,
            hook_event,
        }
        | HookSystemMessage {
            content: message,
            hook_name,
            tool_use_id,
            hook_event,
        } => {
            row[2] = s(hook_name.clone());
            row[3] = s(tool_use_id.clone());
            row[4] = s(hook_event.clone());
            row[5] = s_str(message);
        }
        File {
            filename,
            content,
            display_path,
        } => {
            row[12] = s_str(filename);
            row[13] = s(content.file.content.clone());
            row[14] = ojson_serializable(Some(content));
            row[15] = s(display_path.clone());
        }
        EditedTextFile { filename, snippet } => {
            row[12] = s_str(filename);
            row[13] = s_str(snippet);
        }
        Directory {
            path,
            content,
            display_path,
        } => {
            row[16] = s_str(path);
            row[17] = s_str(content);
            row[15] = s_str(display_path);
        }
        CompactFileReference {
            filename,
            display_path,
        } => {
            row[12] = s_str(filename);
            row[15] = s_str(display_path);
        }
        CommandPermissions { allowed_tools } => {
            row[18] = json_str(&json!(allowed_tools));
        }
        PlanMode {
            reminder_type,
            is_sub_agent,
            plan_file_path,
            plan_exists,
        } => {
            row[19] = s_str(reminder_type);
            row[20] = b(*is_sub_agent);
            row[21] = s(plan_file_path.clone());
            row[22] = b(*plan_exists);
        }
        PlanModeExit {
            plan_file_path,
            plan_exists,
        } => {
            row[21] = s(plan_file_path.clone());
            row[22] = b(*plan_exists);
        }
        SkillListing {
            content,
            is_initial,
            skill_count,
        } => {
            row[23] = s_str(content);
            row[24] = ob(*is_initial);
            row[25] = ou32(*skill_count);
        }
        DynamicSkill {
            skill_dir,
            skill_names,
            display_path,
        } => {
            row[26] = s_str(skill_dir);
            row[27] = json_str(&json!(skill_names));
            row[15] = s_str(display_path);
        }
        InvokedSkills { skills } => {
            row[28] = json_str(&json!(skills));
            for (idx, sk) in skills.iter().enumerate() {
                skill_rows.push(vec![
                    Value::Null,
                    u(idx as u64),
                    s_str(&sk.name),
                    json_str(&json!({ "path": sk.path, "content": sk.content })),
                ]);
            }
        }
        TaskReminder {
            content,
            item_count,
        }
        | TodoReminder {
            content,
            item_count,
        } => {
            row[29] = json_str(&json!(content));
            row[30] = u(*item_count as u64);
        }
        Diagnostics { files, is_new } => {
            row[31] = json_str(&json!(files));
            row[32] = b(*is_new);
            for (idx, df) in files.iter().enumerate() {
                diag_rows.push(vec![
                    Value::Null,
                    u(idx as u64),
                    s_str(&df.uri),
                    json_str(&json!(df.diagnostics)),
                ]);
            }
        }
        DateChange { new_date } => {
            row[33] = s_str(new_date);
        }
        DeferredToolsDelta {
            added_names,
            added_lines,
            removed_names,
            readded_names,
        } => {
            row[34] = json_str(&json!(added_names));
            row[35] = match added_lines {
                Some(v) => json_str(&json!(v)),
                None => Value::Null,
            };
            row[36] = match removed_names {
                Some(v) => json_str(&json!(v)),
                None => Value::Null,
            };
            row[47] = match readded_names {
                Some(v) => json_str(&json!(v)),
                None => Value::Null,
            };
        }
        AgentListingDelta { .. } => {
            // Classify-only: attachment_type column captures the variant.
            // Inner fields (addedTypes/addedLines/removedTypes/isInitial
            // /showConcurrencyNote) are not flattened into dedicated columns —
            // the agent listing payload is verbose and out of scope here.
        }
        AutoMode { .. } | AutoModeExit => {
            // Classify-only: attachment_type captures the variant.
        }
        PlanFileReference {
            plan_file_path,
            plan_content,
        } => {
            // Reuse the file-shaped columns (filename + file_content_text)
            // for plan path + content. Mirrors NestedMemory's path+content
            // pattern but at column indices that already carry path/content
            // semantics for File / EditedTextFile / CompactFileReference.
            row[12] = s_str(plan_file_path);
            row[13] = s_str(plan_content);
        }
        McpInstructionsDelta {
            added_names,
            added_blocks,
            removed_names,
        } => {
            row[37] = json_str(&json!(added_names));
            row[38] = json_str(&json!(added_blocks));
            row[39] = match removed_names {
                Some(v) => json_str(&json!(v)),
                None => Value::Null,
            };
        }
        UltrathinkEffort { level } => {
            row[40] = s_str(level);
        }
        QueuedCommand {
            prompt,
            command_mode,
            image_paste_ids: _,
        } => {
            row[41] = match prompt {
                Value::String(t) => s_str(t),
                Value::Array(blocks) => {
                    let parts: Vec<String> = blocks
                        .iter()
                        .filter_map(|b| {
                            b.get("type").and_then(Value::as_str).and_then(|ty| {
                                if ty == "text" {
                                    b.get("text").and_then(Value::as_str).map(str::to_string)
                                } else {
                                    Some(format!("[{ty}]"))
                                }
                            })
                        })
                        .collect();
                    s_str(&parts.join("\n"))
                }
                Value::Null => Value::Null,
                other => json_str(other),
            };
            row[42] = s(command_mode.clone());
        }
        NestedMemory {
            path,
            content,
            display_path,
        } => {
            row[15] = s_str(display_path);
            row[43] = s_str(path);
            row[44] = s_str(&content.memory_type);
            row[45] = s_str(&content.content);
            row[46] = match content.content_differs_from_disk {
                Some(v) => b(v),
                None => Value::Null,
            };
        }
        Unknown => {
            unknown_variants.push("AttachmentData".to_string());
        }
    }

    let mut children: Vec<(&'static str, Vec<Vec<Value>>)> = Vec::new();
    if !diag_rows.is_empty() {
        children.push(("attachment_diagnostics_files", diag_rows));
    }
    if !skill_rows.is_empty() {
        children.push(("attachment_invoked_skills", skill_rows));
    }
    (Some(("attachment_entries", row)), children)
}

fn build_progress(pe: &claude_code_transcripts::types::ProgressEntry) -> BuiltRows {
    let d = &pe.data;
    let row = vec![
        Value::Null,                      // entry_id
        s(pe.parent_tool_use_id.clone()), // parent_tool_use_id
        s(pe.tool_use_id.clone()),        // tool_use_id
        s_str(&d.data_type),              // data_type
        s(d.hook_event.clone()),          // hook_event
        s(d.hook_name.clone()),           // hook_name
        s(d.command.clone()),             // command
        s(d.agent_id.clone()),            // agent_id
        s(d.prompt.clone()),              // prompt
        ojson(d.message.as_ref()),        // message
        s(d.query.clone()),               // query
        ou32(d.result_count),             // result_count
        of(d.elapsed_time_seconds),       // elapsed_time_seconds
        s(d.full_output.clone()),         // full_output
        s(d.output.clone()),              // output
        ou(d.timeout_ms),                 // timeout_ms
        ou(d.total_lines),                // total_lines
        ou(d.total_bytes),                // total_bytes
        s(d.task_id.clone()),             // task_id
        s(d.server_name.clone()),         // server_name
        s(d.status.clone()),              // status
        s(d.tool_name.clone()),           // tool_name
        of(d.elapsed_time_ms),            // elapsed_time_ms
        s(d.task_description.clone()),    // task_description
        s(d.task_type.clone()),           // task_type
    ];
    (Some(("progress_entries", row)), vec![])
}

// Suppress unused warning when path types change.
#[allow(dead_code)]
fn _silence_unused_path() -> PathBuf {
    PathBuf::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes content to a uniquely-named temp file and deletes it on drop.
    ///
    /// Backed by `tempfile::NamedTempFile`, which uses a thread-local random
    /// suffix + atomic counter so paths cannot collide across threads in the
    /// same process. The previous implementation derived the filename from
    /// `process::id() + subsec_nanos()`, which collided when `cargo test` ran
    /// multiple parse tests in parallel within the same PID — causing the
    /// `unknown_*` tests to intermittently see each other's input lines.
    struct TempJsonl(tempfile::NamedTempFile);

    impl TempJsonl {
        fn new(content: &str) -> Self {
            let mut f = tempfile::Builder::new()
                .prefix("cct-test-")
                .suffix(".jsonl")
                .tempfile()
                .expect("create temp jsonl");
            std::io::Write::write_all(f.as_file_mut(), content.as_bytes())
                .expect("write temp jsonl");
            TempJsonl(f)
        }

        fn path(&self) -> &Path {
            self.0.path()
        }
    }

    // Minimal envelope fields shared by all test lines.
    const ENVELOPE: &str = r#""parentUuid":null,"isSidechain":false,"sessionId":"test-session","timestamp":"2024-01-01T00:00:00.000000Z""#;

    #[test]
    fn unknown_attachment_type_is_dropped_not_failed() {
        let line = format!(
            r#"{{"type":"attachment","uuid":"a1",{ENVELOPE},"attachment":{{"type":"future_attachment_shape","payload":"foo"}}}}"#
        );
        let tmp = TempJsonl::new(&line);
        let parsed = parse_file(tmp.path(), &HashMap::new());

        assert!(
            parsed.failures.is_empty(),
            "unexpected failures: {:?}",
            parsed.failures
        );
        assert_eq!(parsed.entries.len(), 1, "entry should still be ingested");
        assert!(
            parsed
                .unknown_variants
                .contains(&"AttachmentData".to_string()),
            "expected AttachmentData in unknown_variants, got {:?}",
            parsed.unknown_variants
        );
    }

    #[test]
    fn unknown_assistant_content_block_is_dropped_not_failed() {
        let line = format!(
            r#"{{"type":"assistant","uuid":"b1",{ENVELOPE},"message":{{"id":"msg1","type":"message","role":"assistant","model":"claude-3-opus-20240229","content":[{{"type":"future_modality","data":"ignored"}}],"stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
        );
        let tmp = TempJsonl::new(&line);
        let parsed = parse_file(tmp.path(), &HashMap::new());

        assert!(
            parsed.failures.is_empty(),
            "unexpected failures: {:?}",
            parsed.failures
        );
        assert_eq!(parsed.entries.len(), 1);
        assert!(
            parsed
                .unknown_variants
                .contains(&"AssistantContentBlock".to_string()),
            "got {:?}",
            parsed.unknown_variants
        );
    }

    #[test]
    fn unknown_user_content_block_is_dropped_not_failed() {
        let line = format!(
            r#"{{"type":"user","uuid":"c1",{ENVELOPE},"message":{{"role":"user","content":[{{"type":"video","url":"https://example.com"}}]}}}}"#
        );
        let tmp = TempJsonl::new(&line);
        let parsed = parse_file(tmp.path(), &HashMap::new());

        assert!(
            parsed.failures.is_empty(),
            "unexpected failures: {:?}",
            parsed.failures
        );
        assert_eq!(parsed.entries.len(), 1);
        assert!(
            parsed
                .unknown_variants
                .contains(&"UserContentBlock".to_string()),
            "got {:?}",
            parsed.unknown_variants
        );
    }

    #[test]
    fn unknown_image_source_type_is_dropped_not_failed() {
        let line = format!(
            r#"{{"type":"user","uuid":"d1",{ENVELOPE},"message":{{"role":"user","content":[{{"type":"image","source":{{"type":"s3_bucket","key":"test"}}}}]}}}}"#
        );
        let tmp = TempJsonl::new(&line);
        let parsed = parse_file(tmp.path(), &HashMap::new());

        assert!(
            parsed.failures.is_empty(),
            "unexpected failures: {:?}",
            parsed.failures
        );
        assert_eq!(parsed.entries.len(), 1);
        assert!(
            parsed.unknown_variants.contains(&"ImageSource".to_string()),
            "got {:?}",
            parsed.unknown_variants
        );
    }

    #[test]
    fn unknown_document_source_type_is_dropped_not_failed() {
        let line = format!(
            r#"{{"type":"user","uuid":"e1",{ENVELOPE},"message":{{"role":"user","content":[{{"type":"document","source":{{"type":"pdf_url","url":"https://example.com/doc.pdf"}}}}]}}}}"#
        );
        let tmp = TempJsonl::new(&line);
        let parsed = parse_file(tmp.path(), &HashMap::new());

        assert!(
            parsed.failures.is_empty(),
            "unexpected failures: {:?}",
            parsed.failures
        );
        assert_eq!(parsed.entries.len(), 1);
        assert!(
            parsed
                .unknown_variants
                .contains(&"DocumentSource".to_string()),
            "got {:?}",
            parsed.unknown_variants
        );
    }

    /// Regression: the exact variant that caused the original bug report.
    /// Ensures nested_memory parses into the typed variant and emits an
    /// attachment_entries row (not dropped as Unknown).
    #[test]
    fn nested_memory_attachment_regression() {
        let line = format!(
            r#"{{"type":"attachment","uuid":"f1",{ENVELOPE},"attachment":{{"type":"nested_memory","path":"/p/CLAUDE.md","content":{{"path":"/p/CLAUDE.md","type":"Project","content":"body","contentDiffersFromDisk":false}},"displayPath":"CLAUDE.md"}}}}"#
        );
        let tmp = TempJsonl::new(&line);
        let parsed = parse_file(tmp.path(), &HashMap::new());

        assert!(
            parsed.failures.is_empty(),
            "nested_memory should not produce a parse failure: {:?}",
            parsed.failures
        );
        assert!(
            !parsed
                .unknown_variants
                .contains(&"AttachmentData".to_string()),
            "nested_memory must not be reported as unknown: {:?}",
            parsed.unknown_variants
        );
        assert_eq!(parsed.entries.len(), 1);
        let e = &parsed.entries[0];
        let (table, row) = e
            .variant
            .as_ref()
            .expect("attachment variant row should be present");
        assert_eq!(*table, "attachment_entries");
        // attachment_type column
        assert_eq!(row[1], serde_json::json!("nested_memory"));
        // display_path (col 15), nested_memory_path (43), memory_type (44),
        // content (45), differs_from_disk (46)
        assert_eq!(row[15], serde_json::json!("CLAUDE.md"));
        assert_eq!(row[43], serde_json::json!("/p/CLAUDE.md"));
        assert_eq!(row[44], serde_json::json!("Project"));
        assert_eq!(row[45], serde_json::json!("body"));
        assert_eq!(row[46], serde_json::json!(false));
    }

    /// Three attachment shapes that previously fell through to Unknown:
    /// hook_stopped_continuation, hook_system_message, todo_reminder.
    #[test]
    fn newly_supported_attachment_variants_round_trip() {
        let lines = [
            r#"{"type":"attachment","uuid":"g1","parentUuid":null,"isSidechain":false,"sessionId":"s","timestamp":"2024-01-01T00:00:00.000000Z","attachment":{"type":"hook_stopped_continuation","message":"Awaiting owner reply (turn ended).","hookName":"PostToolUse:x","toolUseID":"tu1","hookEvent":"PostToolUse"}}"#,
            r#"{"type":"attachment","uuid":"g2","parentUuid":null,"isSidechain":false,"sessionId":"s","timestamp":"2024-01-01T00:00:00.000000Z","attachment":{"type":"hook_system_message","content":"Describe your goal.","hookName":"PreToolUse:x","toolUseID":"tu2","hookEvent":"PreToolUse"}}"#,
            r#"{"type":"attachment","uuid":"g3","parentUuid":null,"isSidechain":false,"sessionId":"s","timestamp":"2024-01-01T00:00:00.000000Z","attachment":{"type":"todo_reminder","content":[],"itemCount":0}}"#,
        ];
        let tmp = TempJsonl::new(&lines.join("\n"));
        let parsed = parse_file(tmp.path(), &HashMap::new());

        assert!(parsed.failures.is_empty(), "{:?}", parsed.failures);
        assert!(
            !parsed
                .unknown_variants
                .contains(&"AttachmentData".to_string()),
            "none of these should be Unknown: {:?}",
            parsed.unknown_variants
        );
        assert_eq!(parsed.entries.len(), 3);

        let row_for = |idx: usize| -> &Vec<Value> {
            parsed.entries[idx]
                .variant
                .as_ref()
                .map(|(_, r)| r)
                .expect("variant row")
        };

        let r0 = row_for(0);
        assert_eq!(r0[1], serde_json::json!("hook_stopped_continuation"));
        assert_eq!(r0[2], serde_json::json!("PostToolUse:x"));
        assert_eq!(r0[3], serde_json::json!("tu1"));
        assert_eq!(r0[4], serde_json::json!("PostToolUse"));
        assert_eq!(
            r0[5],
            serde_json::json!("Awaiting owner reply (turn ended).")
        );

        let r1 = row_for(1);
        assert_eq!(r1[1], serde_json::json!("hook_system_message"));
        assert_eq!(r1[2], serde_json::json!("PreToolUse:x"));
        assert_eq!(r1[3], serde_json::json!("tu2"));
        assert_eq!(r1[4], serde_json::json!("PreToolUse"));
        assert_eq!(r1[5], serde_json::json!("Describe your goal."));

        let r2 = row_for(2);
        assert_eq!(r2[1], serde_json::json!("todo_reminder"));
        assert_eq!(r2[29], serde_json::json!("[]"));
        assert_eq!(r2[30], serde_json::json!(0));
    }

    /// Assistant entry carrying attributionAgent, attributionPlugin,
    /// attributionSkill, and message.diagnostics.cache_miss_reason ingests
    /// cleanly with no parse failures and no unknown variants. Regression
    /// test for the 2026-05-05 transcript shape change.
    #[test]
    fn assistant_attribution_and_diagnostics_parse_clean() {
        let line = format!(
            r#"{{"type":"assistant","uuid":"x1",{ENVELOPE},"attributionAgent":"plugin1:agent1","attributionPlugin":"plugin1","attributionSkill":"plugin1:skill1","message":{{"id":"msg1","type":"message","role":"assistant","model":"claude-opus-4-7","content":[],"stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":10,"output_tokens":5}},"diagnostics":{{"cache_miss_reason":{{"type":"tools_changed","cache_missed_input_tokens":41735}}}}}}}}"#
        );
        let tmp = TempJsonl::new(&line);
        let parsed = parse_file(tmp.path(), &HashMap::new());

        assert!(
            parsed.failures.is_empty(),
            "unexpected failures: {:?}",
            parsed.failures
        );
        assert_eq!(parsed.entries.len(), 1, "entry should be ingested");
        assert!(
            parsed.unknown_variants.is_empty(),
            "no unknown variants expected, got {:?}",
            parsed.unknown_variants
        );
    }
}
