use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Top-level Entry — one per JSONL line
// ---------------------------------------------------------------------------

// AssistantEntry / SystemEntry are wide by nature (one flat struct per
// transcript shape). Boxing them would shrink the enum but break the public
// API of this published crate, and entries are parsed and consumed one line at
// a time — the enum is never held in a large collection — so the size costs
// nothing in practice.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Entry {
    // ── Message-bearing entries ──────────────────────────────────────────
    #[serde(rename = "user")]
    User(UserEntry),

    #[serde(rename = "assistant")]
    Assistant(AssistantEntry),

    #[serde(rename = "system")]
    System(SystemEntry),

    #[serde(rename = "attachment")]
    Attachment(AttachmentEntry),

    #[serde(rename = "progress")]
    Progress(ProgressEntry),

    // ── Metadata-only entries (no envelope) ─────────────────────────────
    #[serde(rename = "permission-mode")]
    PermissionMode(PermissionModeEntry),

    #[serde(rename = "last-prompt")]
    LastPrompt(LastPromptEntry),

    #[serde(rename = "ai-title")]
    AiTitle(AiTitleEntry),

    #[serde(rename = "custom-title")]
    CustomTitle(CustomTitleEntry),

    #[serde(rename = "agent-name")]
    AgentName(AgentNameEntry),

    #[serde(rename = "agent-color")]
    AgentColor(AgentColorEntry),

    #[serde(rename = "agent-setting")]
    AgentSetting(AgentSettingEntry),

    #[serde(rename = "tag")]
    Tag(TagEntry),

    #[serde(rename = "summary")]
    Summary(SummaryEntry),

    #[serde(rename = "task-summary")]
    TaskSummary(TaskSummaryEntry),

    #[serde(rename = "pr-link")]
    PrLink(PrLinkEntry),

    #[serde(rename = "mode")]
    Mode(ModeEntry),

    #[serde(rename = "worktree-state")]
    WorktreeState(WorktreeStateEntry),

    #[serde(rename = "content-replacement")]
    ContentReplacement(ContentReplacementEntry),

    #[serde(rename = "file-history-snapshot")]
    FileHistorySnapshot(FileHistorySnapshotEntry),

    #[serde(rename = "attribution-snapshot")]
    AttributionSnapshot(AttributionSnapshotEntry),

    #[serde(rename = "queue-operation")]
    QueueOperation(QueueOperationEntry),

    #[serde(rename = "marble-origami-commit")]
    ContextCollapseCommit(ContextCollapseCommitEntry),

    #[serde(rename = "marble-origami-snapshot")]
    ContextCollapseSnapshot(ContextCollapseSnapshotEntry),

    #[serde(rename = "speculation-accept")]
    SpeculationAccept(SpeculationAcceptEntry),

    #[serde(rename = "atis-latch")]
    AtisLatch(AtisLatchEntry),

    #[serde(rename = "bridge-session")]
    BridgeSession(BridgeSessionEntry),

    #[serde(rename = "file-history-delta")]
    FileHistoryDelta(FileHistoryDeltaEntry),

    #[serde(rename = "frame-link")]
    FrameLink(FrameLinkEntry),

    #[serde(rename = "fork-context-ref")]
    ForkContextRef(ForkContextRefEntry),

    #[serde(rename = "artifact-autoreact-ledger")]
    ArtifactAutoreactLedger(ArtifactAutoreactLedgerEntry),

    #[serde(rename = "artifact-comment-monitor")]
    ArtifactCommentMonitor(ArtifactCommentMonitorEntry),

    /// Catch-all for entry types not yet recognised by the ingest binary.
    /// Allows forward-compatible parsing: new Claude Code entry types in
    /// the JSONL will be silently skipped rather than aborting ingest.
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// Shared envelope — present on all message-bearing entries
//
// parentUuid serialises WITHOUT skip_serializing_if so that explicit JSON
// nulls (first message in a session) round-trip correctly as null rather
// than being dropped.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub uuid: String,

    /// null = first message in session; UUID = linked to previous entry.
    pub parent_uuid: Option<String>,

    /// Preserves logical chain across compact boundaries (parentUuid is
    /// nulled at those points).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_parent_uuid: Option<String>,

    pub is_sidechain: bool,
    pub session_id: String,
    pub timestamp: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_type: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,

    /// Human-readable session slug, e.g. "drifting-tinkering-parnas".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,

    /// 7-char hex id for sidechain / subagent sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_color: Option<String>,

    /// Correlates with OTel prompt.id for user-prompt messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,

    /// True when this entry should be hidden in the UI (meta / invisible).
    #[serde(rename = "isMeta", skip_serializing_if = "Option::is_none")]
    pub is_meta: Option<bool>,

    /// Set when this session was forked from another session.
    #[serde(rename = "forkedFrom", skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<ForkedFrom>,

    /// Snake-case session id emitted alongside `sessionId` by newer clients.
    /// Usually identical to `session_id`, but the two do diverge (observed on
    /// resumed sessions, where `session_id` keeps the originating id), so it
    /// is kept as its own field rather than aliased onto `sessionId`.
    #[serde(rename = "session_id", skip_serializing_if = "Option::is_none")]
    pub session_id_snake: Option<String>,

    /// Session flavour when not a plain foreground session — e.g. "bg" for
    /// background sessions. Absent on ordinary interactive sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkedFrom {
    pub message_uuid: String,
    pub session_id: String,
}

// ---------------------------------------------------------------------------
// User entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserEntry {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub message: UserMessage,

    /// Structured result of the tool call this message delivers (populated
    /// by Claude Code, not the API).
    #[serde(rename = "toolUseResult", skip_serializing_if = "Option::is_none")]
    pub tool_use_result: Option<Value>,

    /// UUID of the assistant message that requested this tool result.
    #[serde(
        rename = "sourceToolAssistantUUID",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_tool_assistant_uuid: Option<String>,

    /// ID of the tool use block that triggered this user message.
    #[serde(rename = "sourceToolUseID", skip_serializing_if = "Option::is_none")]
    pub source_tool_use_id: Option<String>,

    #[serde(rename = "permissionMode", skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<Value>,

    #[serde(rename = "isCompactSummary", skip_serializing_if = "Option::is_none")]
    pub is_compact_summary: Option<bool>,

    #[serde(
        rename = "isVisibleInTranscriptOnly",
        skip_serializing_if = "Option::is_none"
    )]
    pub is_visible_in_transcript_only: Option<bool>,

    #[serde(rename = "imagePasteIds", skip_serializing_if = "Option::is_none")]
    pub image_paste_ids: Option<Vec<u64>>,

    #[serde(rename = "planContent", skip_serializing_if = "Option::is_none")]
    pub plan_content: Option<String>,

    /// How the prompt reached the session: "typed" | "system" | "queued" |
    /// "suggestion_accepted". Present on real user-prompt entries only.
    #[serde(rename = "promptSource", skip_serializing_if = "Option::is_none")]
    pub prompt_source: Option<String>,

    /// Newline-terminated JSON blob of classifier metadata attached to the
    /// prompt (e.g. `{"meta":{"gitStatus":{…}}}`).
    #[serde(
        rename = "classifierMetaLines",
        skip_serializing_if = "Option::is_none"
    )]
    pub classifier_meta_lines: Option<String>,

    /// API message id of the assistant turn that this entry interrupted.
    #[serde(
        rename = "interruptedMessageId",
        skip_serializing_if = "Option::is_none"
    )]
    pub interrupted_message_id: Option<String>,

    /// Why a tool call was denied: "user-rejected" | "automode-blocked" |
    /// "automode-unavailable".
    #[serde(rename = "toolDenialKind", skip_serializing_if = "Option::is_none")]
    pub tool_denial_kind: Option<String>,

    /// Queue placement for a prompt submitted while the turn was running,
    /// e.g. "later".
    #[serde(rename = "queuePriority", skip_serializing_if = "Option::is_none")]
    pub queue_priority: Option<String>,

    /// True when this entry accompanies the turn rather than driving it.
    #[serde(rename = "turnCompanion", skip_serializing_if = "Option::is_none")]
    pub turn_companion: Option<bool>,

    /// MCP `_meta` envelope forwarded with an MCP tool result. Shape is
    /// defined by the MCP spec / the server, so it is kept opaque.
    #[serde(rename = "mcpMeta", skip_serializing_if = "Option::is_none")]
    pub mcp_meta: Option<Value>,

    /// Harness-authored instructions injected as user feedback (e.g. when the
    /// user asks to clarify an AskUserQuestion prompt).
    #[serde(rename = "userFeedback", skip_serializing_if = "Option::is_none")]
    pub user_feedback: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    pub role: UserRole,
    pub content: UserContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    User,
    #[serde(other)]
    Unknown,
}

/// User content is either a plain string or an array of typed blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<UserContentBlock>),
    /// Catch-all for content shapes not yet recognised (e.g. future object forms).
    Other(Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContentBlock {
    Text {
        text: String,
    },

    ToolResult {
        tool_use_id: String,
        /// String for plain text, or array of content blocks for rich results.
        /// Using Value here because serde cannot nest untagged enums inside
        /// the fields of an internally-tagged enum variant.
        content: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },

    Image {
        source: ImageSource,
    },

    Document {
        source: DocumentSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },

    /// Catch-all for block types not yet recognised by the ingest binary.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 {
        media_type: String,
        data: String,
    },
    Url {
        url: String,
    },
    /// Catch-all for source types not yet recognised.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DocumentSource {
    Base64 {
        media_type: String,
        data: String,
    },
    Text {
        data: String,
    },
    Url {
        url: String,
    },
    /// Catch-all for source types not yet recognised.
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// Assistant entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantEntry {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub message: AssistantMessage,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,

    #[serde(rename = "isApiErrorMessage", skip_serializing_if = "Option::is_none")]
    pub is_api_error_message: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// HTTP status returned by the API when the turn errored (e.g. 401, 429,
    /// 400, 403). Populated alongside `error` / `is_api_error_message` on
    /// failed turns; absent on successful turns.
    #[serde(rename = "apiErrorStatus", skip_serializing_if = "Option::is_none")]
    pub api_error_status: Option<u16>,

    /// Subagent / plugin slug that produced this turn. Format `<plugin>:<agent>`
    /// when emitted by a plugin-namespaced agent, or bare `<agent>` for
    /// built-in agents. Absent on top-level turns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution_agent: Option<String>,

    /// Plugin namespace owning the subagent for this turn. Canonical when it
    /// disagrees with the `<plugin>:` prefix of `attribution_agent`. Absent
    /// when `attribution_agent` is bare or absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution_plugin: Option<String>,

    /// Skill slug invoked on this turn. Format `<plugin>:<skill>` for
    /// plugin-namespaced skills, or bare `<skill>` for built-in skills.
    /// Absent when no skill is active for this turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution_skill: Option<String>,

    /// MCP server that served the tool call attributed to this turn, e.g.
    /// "plugin:context7:context7". Paired with `attribution_mcp_tool`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution_mcp_server: Option<String>,

    /// MCP tool name (server-local, un-prefixed) invoked on this turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution_mcp_tool: Option<String>,

    /// Reasoning effort the turn ran at: "medium" | "high" | "xhigh" (and the
    /// other tiers the client offers). Absent on older transcripts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,

    /// Rate-limit / quota snapshot attached when the API rejected or throttled
    /// the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota_limits: Option<QuotaLimits>,

    /// True when the response stream was cut off mid-flight (user interrupt or
    /// transport failure) rather than completing.
    #[serde(rename = "isAbortedMidStream", skip_serializing_if = "Option::is_none")]
    pub is_aborted_mid_stream: Option<bool>,

    /// Raw error body from the API for a failed turn (status line + JSON).
    /// Companion to the short `error` slug.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
}

/// Quota / rate-limit state reported by the API on a throttled or rejected
/// turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaLimits {
    /// e.g. "rejected" | "allowed".
    pub status: String,

    /// Unix seconds at which the limit window resets.
    pub resets_at: u64,

    pub unified_rate_limit_fallback_available: bool,

    /// Window the limit applies to, e.g. "five_hour".
    pub rate_limit_type: String,

    /// Overage state, e.g. "rejected".
    pub overage_status: String,

    /// Why overage was unavailable, e.g. "org_level_disabled".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overage_disabled_reason: Option<String>,

    pub is_using_overage: bool,

    /// Upgrade routes offered to the user, e.g. ["upgrade_plan"].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upgrade_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub id: String,
    /// Always "message".
    #[serde(rename = "type")]
    pub msg_type: String,
    pub role: AssistantRole,
    #[serde(default)]
    pub model: Option<String>,

    /// null when no container; Some(None) = present as JSON null.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_nullable"
    )]
    pub container: Option<Option<Value>>,

    pub content: Vec<AssistantContentBlock>,

    /// The API always includes this field; null means the stream is still
    /// ongoing or the field was not set.
    pub stop_reason: Option<String>,

    /// null when stop_reason != "stop_sequence"
    pub stop_sequence: Option<String>,

    /// null in most responses; some API versions emit structured details.
    /// outer None = field absent, Some(None) = field present as JSON null.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_nullable"
    )]
    pub stop_details: Option<Option<Value>>,

    pub usage: AssistantUsage,

    /// null in most responses; Some(None) = present as JSON null.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_nullable"
    )]
    pub context_management: Option<Option<Value>>,

    /// API-emitted cache-miss diagnostic. `null` on most turns; an object
    /// when the API reports why prompt caching did not hit. Outer
    /// `None` = field absent, `Some(None)` = JSON null, `Some(Some(d))`
    /// = populated.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_nullable"
    )]
    pub diagnostics: Option<Option<AssistantDiagnostics>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssistantRole {
    Assistant,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantContentBlock {
    Text {
        text: String,
    },

    /// Extended thinking block. `thinking` is always an empty string in
    /// persisted transcripts (Claude Code redacts it for storage); the
    /// cryptographic `signature` is retained.
    Thinking {
        thinking: String,
        signature: String,
    },

    RedactedThinking {
        data: String,
    },

    ToolUse {
        id: String,
        name: String,
        input: Value,
        /// Present in some versions to identify call origin.
        #[serde(skip_serializing_if = "Option::is_none")]
        caller: Option<ToolUseCaller>,
    },

    /// Catch-all for content block types not yet recognised by the ingest binary.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUseCaller {
    #[serde(rename = "type")]
    pub caller_type: String,
}

// The Anthropic API returns usage fields in snake_case — no rename_all here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_tool_use: Option<ServerToolUse>,

    /// null = explicitly set to null by API; absent = field not present.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_nullable"
    )]
    pub service_tier: Option<Option<Value>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<CacheCreation>,

    /// null = explicitly set to null by API; absent = field not present.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_nullable"
    )]
    pub inference_geo: Option<Option<Value>>,

    /// null = explicitly set to null by API; absent = field not present.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_nullable"
    )]
    pub iterations: Option<Option<Value>>,

    /// null = explicitly set to null by API; absent = field not present.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_nullable"
    )]
    pub speed: Option<Option<Value>>,

    /// Breakdown of `output_tokens`; currently carries the thinking-token
    /// share of the completion. null = present as JSON null; absent = field
    /// not present.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_nullable"
    )]
    pub output_tokens_details: Option<Option<OutputTokensDetails>>,
}

// Snake-case to match the API payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputTokensDetails {
    /// Extended-thinking tokens included in `output_tokens`.
    pub thinking_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerToolUse {
    #[serde(default)]
    pub web_search_requests: u64,
    #[serde(default)]
    pub web_fetch_requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheCreation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ephemeral_1h_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ephemeral_5m_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageIteration {
    pub input_tokens: u64,
    pub output_tokens: u64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<CacheCreation>,

    /// Iteration type; typically "message".
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub iter_type: Option<String>,
}

// ---------------------------------------------------------------------------
// System entry
//
// All subtype-specific fields are optional so a single flat struct covers
// every subtype while preserving exact field order semantics.  Type safety
// on the discriminant is still enforced via SystemSubtype.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemEntry {
    #[serde(flatten)]
    pub envelope: Envelope,

    pub subtype: SystemSubtype,

    /// Human-readable message text (most subtypes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// Severity level: "info" | "warning" | "error" | "suggestion".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,

    /// True when the entry should be hidden from the main conversation view.
    #[serde(rename = "isMeta", skip_serializing_if = "Option::is_none")]
    pub is_meta: Option<bool>,

    // ── api_error ────────────────────────────────────────────────────────
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,

    #[serde(rename = "retryInMs", skip_serializing_if = "Option::is_none")]
    pub retry_in_ms: Option<f64>,

    #[serde(rename = "retryAttempt", skip_serializing_if = "Option::is_none")]
    pub retry_attempt: Option<u32>,

    #[serde(rename = "maxRetries", skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,

    // ── stop_hook_summary ────────────────────────────────────────────────
    #[serde(rename = "hookCount", skip_serializing_if = "Option::is_none")]
    pub hook_count: Option<u32>,

    #[serde(rename = "hookInfos", skip_serializing_if = "Option::is_none")]
    pub hook_infos: Option<Vec<HookInfo>>,

    #[serde(rename = "hookErrors", skip_serializing_if = "Option::is_none")]
    pub hook_errors: Option<Vec<Value>>,

    #[serde(
        rename = "preventedContinuation",
        skip_serializing_if = "Option::is_none"
    )]
    pub prevented_continuation: Option<bool>,

    #[serde(rename = "stopReason", skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,

    #[serde(rename = "hasOutput", skip_serializing_if = "Option::is_none")]
    pub has_output: Option<bool>,

    #[serde(rename = "toolUseID", skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,

    // ── turn_duration ────────────────────────────────────────────────────
    #[serde(rename = "durationMs", skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,

    #[serde(rename = "messageCount", skip_serializing_if = "Option::is_none")]
    pub message_count: Option<u32>,

    // ── bridge_status ────────────────────────────────────────────────────
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    #[serde(rename = "upgradeNudge", skip_serializing_if = "Option::is_none")]
    pub upgrade_nudge: Option<String>,

    // ── compact_boundary ────────────────────────────────────────────────
    #[serde(rename = "compactMetadata", skip_serializing_if = "Option::is_none")]
    pub compact_metadata: Option<CompactMetadata>,

    /// Context strings hooks contributed on this event. Empty in every
    /// observed `stop_hook_summary`; element shape is hook-defined.
    #[serde(
        rename = "hookAdditionalContext",
        skip_serializing_if = "Option::is_none"
    )]
    pub hook_additional_context: Option<Vec<Value>>,

    /// Background agents still running when the turn ended.
    #[serde(
        rename = "pendingBackgroundAgentCount",
        skip_serializing_if = "Option::is_none"
    )]
    pub pending_background_agent_count: Option<u32>,

    /// Set on `scheduled_task_fire` entries to say which scheduler fired the
    /// turn, e.g. "loop".
    #[serde(rename = "cronKind", skip_serializing_if = "Option::is_none")]
    pub cron_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemSubtype {
    ApiError,
    AwaySummary,
    BridgeStatus,
    CompactBoundary,
    Informational,
    LocalCommand,
    ScheduledTaskFire,
    StopHookSummary,
    TurnDuration,
    MicrocompactBoundary,
    PermissionRetry,
    AgentsKilled,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookInfo {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,

    /// Prompt text the hook injected (goal-condition hooks and friends).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreservedSegment {
    pub head_uuid: String,
    pub anchor_uuid: String,
    pub tail_uuid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactMetadata {
    pub trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preserved_segment: Option<PreservedSegment>,
    #[serde(
        rename = "preCompactDiscoveredTools",
        skip_serializing_if = "Option::is_none"
    )]
    pub pre_compact_discovered_tools: Option<Vec<String>>,

    /// Tokens dropped by every compaction in this session so far, including
    /// this one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cumulative_dropped_tokens: Option<u64>,

    /// Explicit uuid list of the messages carried across the boundary — the
    /// enumerated form of `preserved_segment`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preserved_messages: Option<PreservedMessages>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreservedMessages {
    /// Uuid of the anchor message the preserved window is centred on.
    pub anchor_uuid: String,

    /// Uuids kept in the post-compaction context.
    pub uuids: Vec<String>,

    /// Uuids considered for preservation, including ones ultimately dropped.
    pub all_uuids: Vec<String>,
}

// ---------------------------------------------------------------------------
// Attachment entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentEntry {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub attachment: AttachmentData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttachmentData {
    // ── Hook results ─────────────────────────────────────────────────────
    HookSuccess(HookResultAttachment),
    HookNonBlockingError(HookResultAttachment),
    HookBlockingError(HookResultAttachment),
    HookCancelled(HookResultAttachment),

    HookAdditionalContext {
        content: Vec<String>,
        #[serde(rename = "hookName", skip_serializing_if = "Option::is_none")]
        hook_name: Option<String>,
        #[serde(rename = "toolUseID", skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        #[serde(rename = "hookEvent", skip_serializing_if = "Option::is_none")]
        hook_event: Option<String>,
    },

    HookPermissionDecision {
        decision: String,
        #[serde(rename = "hookName", skip_serializing_if = "Option::is_none")]
        hook_name: Option<String>,
        #[serde(rename = "toolUseID", skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        #[serde(rename = "hookEvent", skip_serializing_if = "Option::is_none")]
        hook_event: Option<String>,
    },

    /// Emitted when a hook ended the assistant's turn (e.g. harness
    /// `await_user_message`). Sibling of HookAdditionalContext but with a
    /// single `message` field.
    HookStoppedContinuation {
        message: String,
        #[serde(rename = "hookName", skip_serializing_if = "Option::is_none")]
        hook_name: Option<String>,
        #[serde(rename = "toolUseID", skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        #[serde(rename = "hookEvent", skip_serializing_if = "Option::is_none")]
        hook_event: Option<String>,
    },

    /// Single-string sibling of HookAdditionalContext.
    HookSystemMessage {
        content: String,
        #[serde(rename = "hookName", skip_serializing_if = "Option::is_none")]
        hook_name: Option<String>,
        #[serde(rename = "toolUseID", skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        #[serde(rename = "hookEvent", skip_serializing_if = "Option::is_none")]
        hook_event: Option<String>,
    },

    // ── File / filesystem ────────────────────────────────────────────────
    File {
        filename: String,
        content: FileAttachmentContent,
        #[serde(rename = "displayPath", skip_serializing_if = "Option::is_none")]
        display_path: Option<String>,
    },

    EditedTextFile {
        filename: String,
        /// Line-numbered file content snippet.
        snippet: String,
    },

    Directory {
        path: String,
        content: String,
        #[serde(rename = "displayPath")]
        display_path: String,
    },

    CompactFileReference {
        filename: String,
        #[serde(rename = "displayPath")]
        display_path: String,
    },

    // ── Permissions ──────────────────────────────────────────────────────
    CommandPermissions {
        #[serde(rename = "allowedTools")]
        allowed_tools: Vec<String>,
    },

    // ── Plan mode ────────────────────────────────────────────────────────
    PlanMode {
        #[serde(rename = "reminderType")]
        reminder_type: String,
        #[serde(rename = "isSubAgent")]
        is_sub_agent: bool,
        #[serde(rename = "planFilePath", skip_serializing_if = "Option::is_none")]
        plan_file_path: Option<String>,
        #[serde(rename = "planExists")]
        plan_exists: bool,
    },

    PlanModeExit {
        #[serde(rename = "planFilePath", skip_serializing_if = "Option::is_none")]
        plan_file_path: Option<String>,
        #[serde(rename = "planExists")]
        plan_exists: bool,
    },

    // ── Auto mode ────────────────────────────────────────────────────────
    AutoMode {
        /// Reminder verbosity on older clients; newer ones describe the mode
        /// through the flags below instead.
        #[serde(rename = "reminderType", skip_serializing_if = "Option::is_none")]
        reminder_type: Option<String>,
        /// True while the client is still walking the user through the auto
        /// mode consent flow.
        #[serde(
            rename = "autoModeConsentFlow",
            skip_serializing_if = "Option::is_none"
        )]
        auto_mode_consent_flow: Option<bool>,
        /// Auto mode is steering work through Bash rather than the dedicated
        /// file tools.
        #[serde(rename = "bashFirst", skip_serializing_if = "Option::is_none")]
        bash_first: Option<bool>,
        /// Auto mode only steers; it does not grant extra permissions.
        #[serde(rename = "steerOnly", skip_serializing_if = "Option::is_none")]
        steer_only: Option<bool>,
        /// Permission prompts are bypassed for this run.
        #[serde(skip_serializing_if = "Option::is_none")]
        bypass: Option<bool>,
    },

    AutoModeExit {
        #[serde(rename = "bashFirst", skip_serializing_if = "Option::is_none")]
        bash_first: Option<bool>,
        #[serde(rename = "steerOnly", skip_serializing_if = "Option::is_none")]
        steer_only: Option<bool>,
    },

    // ── Plan file reference ─────────────────────────────────────────────
    /// Snapshot of a plan markdown file pinned to the conversation. Carries
    /// the absolute path and the full file content at pin time.
    PlanFileReference {
        #[serde(rename = "planFilePath")]
        plan_file_path: String,
        #[serde(rename = "planContent")]
        plan_content: String,
    },

    // ── Skills ───────────────────────────────────────────────────────────
    SkillListing {
        content: String,
        /// True on the very first skill listing injection for a session.
        #[serde(rename = "isInitial", skip_serializing_if = "Option::is_none")]
        is_initial: Option<bool>,
        /// Total number of skills listed.
        #[serde(rename = "skillCount", skip_serializing_if = "Option::is_none")]
        skill_count: Option<u32>,
        /// Slugs of the listed skills, parallel to the rendered `content`.
        #[serde(skip_serializing_if = "Option::is_none")]
        names: Option<Vec<String>>,
    },

    DynamicSkill {
        #[serde(rename = "skillDir")]
        skill_dir: String,
        #[serde(rename = "skillNames")]
        skill_names: Vec<String>,
        #[serde(rename = "displayPath")]
        display_path: String,
    },

    InvokedSkills {
        skills: Vec<InvokedSkill>,
    },

    // ── Tasks ────────────────────────────────────────────────────────────
    TaskReminder {
        content: Vec<Value>,
        #[serde(rename = "itemCount")]
        item_count: u32,
    },

    /// Older alias for TaskReminder; identical shape, only the discriminator
    /// differs. Observed payloads have always been empty.
    TodoReminder {
        content: Vec<Value>,
        #[serde(rename = "itemCount")]
        item_count: u32,
    },

    // ── Diagnostics / IDE ────────────────────────────────────────────────
    Diagnostics {
        files: Vec<DiagnosticsFile>,
        #[serde(rename = "isNew")]
        is_new: bool,
    },

    // ── Dates / context ──────────────────────────────────────────────────
    DateChange {
        #[serde(rename = "newDate")]
        new_date: String,
    },

    // ── Tool / MCP updates ───────────────────────────────────────────────
    DeferredToolsDelta {
        #[serde(rename = "addedNames")]
        added_names: Vec<String>,
        /// Legacy/alias field that mirrors addedNames; both are present in
        /// some versions.
        #[serde(rename = "addedLines", skip_serializing_if = "Option::is_none")]
        added_lines: Option<Vec<String>>,
        #[serde(rename = "removedNames", skip_serializing_if = "Option::is_none")]
        removed_names: Option<Vec<String>>,
        /// Tools that were previously removed and have been re-added on this
        /// turn. Disjoint from `addedNames` (which lists newly-added tools).
        #[serde(rename = "readdedNames", skip_serializing_if = "Option::is_none")]
        readded_names: Option<Vec<String>>,
        /// MCP servers whose tool lists had not arrived yet when the delta was
        /// emitted. Empty in every observed payload.
        #[serde(rename = "pendingMcpServers", skip_serializing_if = "Option::is_none")]
        pending_mcp_servers: Option<Vec<Value>>,
    },

    McpInstructionsDelta {
        #[serde(rename = "addedNames")]
        added_names: Vec<String>,
        #[serde(rename = "addedBlocks")]
        added_blocks: Vec<String>,
        #[serde(rename = "removedNames", skip_serializing_if = "Option::is_none")]
        removed_names: Option<Vec<String>>,
    },

    /// Diff of available agent types announced to the assistant. Sibling of
    /// `DeferredToolsDelta` / `McpInstructionsDelta` but for the agent
    /// listing. `isInitial` is true on the first injection per session;
    /// `showConcurrencyNote` toggles a UI hint about parallel agent dispatch.
    AgentListingDelta {
        #[serde(rename = "addedTypes")]
        added_types: Vec<String>,
        #[serde(rename = "addedLines")]
        added_lines: Vec<String>,
        #[serde(rename = "removedTypes")]
        removed_types: Vec<String>,
        #[serde(rename = "isInitial", skip_serializing_if = "Option::is_none")]
        is_initial: Option<bool>,
        #[serde(
            rename = "showConcurrencyNote",
            skip_serializing_if = "Option::is_none"
        )]
        show_concurrency_note: Option<bool>,
    },

    // ── Thinking effort ──────────────────────────────────────────────────
    UltrathinkEffort {
        level: String,
    },

    // ── Queued commands ──────────────────────────────────────────────────
    QueuedCommand {
        /// String for plain prompts, or array of content blocks (text/image)
        /// for prompts that include attached images. Using Value because serde
        /// cannot nest untagged enums inside an internally-tagged variant.
        prompt: Value,
        #[serde(rename = "commandMode", skip_serializing_if = "Option::is_none")]
        command_mode: Option<String>,
        /// IDs of images pasted into the queued prompt. Mirrors the field of
        /// the same name on `UserEntry`.
        #[serde(rename = "imagePasteIds", skip_serializing_if = "Option::is_none")]
        image_paste_ids: Option<Vec<u64>>,
        /// When the command was queued.
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
        /// Where the queued prompt came from, e.g. `{"kind":"human"}`.
        #[serde(skip_serializing_if = "Option::is_none")]
        origin: Option<Value>,
        /// Uuid of the entry this queued command was lifted from. Snake-case
        /// in the payload, unlike its camelCase siblings.
        #[serde(skip_serializing_if = "Option::is_none")]
        source_uuid: Option<String>,
        /// True when the queued command should stay hidden in the UI.
        #[serde(rename = "isMeta", skip_serializing_if = "Option::is_none")]
        is_meta: Option<bool>,
    },

    // ── Nested memory (CLAUDE.md imports) ────────────────────────────────
    NestedMemory {
        path: String,
        content: NestedMemoryContent,
        #[serde(rename = "displayPath")]
        display_path: String,
    },

    // ── Context budget ───────────────────────────────────────────────────
    /// Remaining-token reminder injected into the conversation, e.g.
    /// `<total_tokens>15000000 tokens left</total_tokens>`.
    TotalTokensReminder {
        text: String,
    },

    // ── Tool output truncation ───────────────────────────────────────────
    /// Banner appended when a Read returned only part of a file.
    ReadTruncationNotice {
        banner: String,
        #[serde(rename = "toolUseID")]
        tool_use_id: String,
    },

    // ── Goal / stop conditions ───────────────────────────────────────────
    /// Verdict of the stop-condition checker for a `/loop`-style goal.
    GoalStatus {
        /// Whether the stop condition is considered met.
        met: bool,
        /// The user-supplied stopping condition being evaluated.
        condition: String,
        /// True when the checker ran as the sentinel pass.
        #[serde(skip_serializing_if = "Option::is_none")]
        sentinel: Option<bool>,
        /// Checker's rationale for the verdict.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Loop iterations executed so far.
        #[serde(skip_serializing_if = "Option::is_none")]
        iterations: Option<u32>,
        /// Wall-clock spent in the loop.
        #[serde(rename = "durationMs", skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// Tokens spent in the loop.
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens: Option<u64>,
    },

    // ── Background tasks ─────────────────────────────────────────────────
    /// Status update for a background task (subagent, workflow, cloud run).
    TaskStatus {
        #[serde(rename = "taskId")]
        task_id: String,
        /// e.g. "local_agent".
        #[serde(rename = "taskType")]
        task_type: String,
        description: String,
        /// e.g. "running" | "completed".
        status: String,
        /// One-line summary of what changed since the last update; null when
        /// there is nothing new to report.
        #[serde(
            rename = "deltaSummary",
            default,
            skip_serializing_if = "Option::is_none",
            with = "opt_nullable"
        )]
        delta_summary: Option<Option<String>>,
        #[serde(rename = "outputFilePath")]
        output_file_path: String,
    },

    /// Catch-all for attachment types not yet recognised by the ingest binary.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NestedMemoryContent {
    pub path: String,
    /// CLAUDE.md scope ("Project", "User", "Local", etc).
    #[serde(rename = "type")]
    pub memory_type: String,
    pub content: String,
    #[serde(
        rename = "contentDiffersFromDisk",
        skip_serializing_if = "Option::is_none"
    )]
    pub content_differs_from_disk: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookResultAttachment {
    #[serde(rename = "hookName", skip_serializing_if = "Option::is_none")]
    pub hook_name: Option<String>,
    #[serde(rename = "toolUseID", skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(rename = "hookEvent", skip_serializing_if = "Option::is_none")]
    pub hook_event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(rename = "blockingError", skip_serializing_if = "Option::is_none")]
    pub blocking_error: Option<Value>,
}

/// Wrapper for a file content attachment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAttachmentContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub file: FileData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileData {
    pub file_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(rename = "numLines", skip_serializing_if = "Option::is_none")]
    pub num_lines: Option<u64>,
    #[serde(rename = "startLine", skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u64>,
    #[serde(rename = "totalLines", skip_serializing_if = "Option::is_none")]
    pub total_lines: Option<u64>,

    /// Present instead of `content` when the attached file is a Jupyter
    /// notebook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cells: Option<Vec<NotebookCell>>,
}

/// One cell of an attached Jupyter notebook. Field casing is mixed in the
/// payload (`cellType` but `cell_id` / `execution_count`), so each key is
/// renamed explicitly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookCell {
    /// "code" | "markdown".
    #[serde(rename = "cellType")]
    pub cell_type: String,

    pub cell_id: String,

    /// Cell language, e.g. "python".
    pub language: String,

    pub source: String,

    /// Execution counter; absent on markdown and never-run cells.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_count: Option<i64>,

    /// Rendered outputs. Shape follows the nbformat spec (`output_type` plus
    /// type-dependent keys), so entries are kept opaque.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokedSkill {
    pub name: String,
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsFile {
    pub uri: String,
    pub diagnostics: Vec<Diagnostic>,
}

/// Cache-miss diagnostic emitted by the API on `AssistantMessage`. Indicates
/// why prompt caching did not hit on this turn and (when known) how many
/// input tokens missed the cache.
///
/// `kind` is kept as `String` (not an enum with `#[serde(other)]`) so the
/// raw value flows through to the ingest column without being collapsed
/// into a generic `Unknown` bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMissReason {
    #[serde(rename = "type")]
    pub kind: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_missed_input_tokens: Option<u64>,
}

/// Diagnostics container on `AssistantMessage`. Currently always shaped as
/// `{ "cache_miss_reason": … }` but boxed as a struct so future sibling
/// keys can be added without breaking deserialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantDiagnostics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_miss_reason: Option<CacheMissReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub message: String,
    pub severity: String,
    pub range: DiagnosticRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticRange {
    pub start: DiagnosticPosition,
    pub end: DiagnosticPosition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticPosition {
    pub line: u32,
    pub character: u32,
}

// ---------------------------------------------------------------------------
// Progress entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEntry {
    #[serde(flatten)]
    pub envelope: Envelope,

    pub data: ProgressData,

    #[serde(rename = "parentToolUseID", skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<String>,

    #[serde(rename = "toolUseID", skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressData {
    #[serde(rename = "type")]
    pub data_type: String,
    #[serde(rename = "hookEvent", skip_serializing_if = "Option::is_none")]
    pub hook_event: Option<String>,
    #[serde(rename = "hookName", skip_serializing_if = "Option::is_none")]
    pub hook_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    // agent_progress fields
    #[serde(rename = "agentId", skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,
    // query_update / search progress fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(rename = "resultCount", skip_serializing_if = "Option::is_none")]
    pub result_count: Option<u32>,
    // bash/command progress fields
    #[serde(rename = "elapsedTimeSeconds", skip_serializing_if = "Option::is_none")]
    pub elapsed_time_seconds: Option<f64>,
    #[serde(rename = "fullOutput", skip_serializing_if = "Option::is_none")]
    pub full_output: Option<String>,
    #[serde(rename = "output", skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(rename = "timeoutMs", skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(rename = "totalLines", skip_serializing_if = "Option::is_none")]
    pub total_lines: Option<u64>,
    #[serde(rename = "totalBytes", skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(rename = "taskId", skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    // mcp tool progress fields
    #[serde(rename = "serverName", skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    #[serde(rename = "status", skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(rename = "toolName", skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(rename = "elapsedTimeMs", skip_serializing_if = "Option::is_none")]
    pub elapsed_time_ms: Option<f64>,
    // agent task progress fields
    #[serde(rename = "taskDescription", skip_serializing_if = "Option::is_none")]
    pub task_description: Option<String>,
    #[serde(rename = "taskType", skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Metadata-only entries
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionModeEntry {
    pub permission_mode: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastPromptEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leaf_uuid: Option<String>,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTitleEntry {
    pub ai_title: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomTitleEntry {
    pub custom_title: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentNameEntry {
    pub agent_name: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentColorEntry {
    pub agent_color: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettingEntry {
    pub agent_setting: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagEntry {
    pub tag: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryEntry {
    pub leaf_uuid: String,
    pub summary: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSummaryEntry {
    pub summary: String,
    pub session_id: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrLinkEntry {
    pub session_id: String,
    pub pr_number: u32,
    pub pr_url: String,
    pub pr_repository: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeEntry {
    pub mode: SessionMode,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMode {
    Coordinator,
    Normal,
    #[serde(other)]
    Unknown,
}

// worktreeSession is nullable (null = exited, object = active)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeStateEntry {
    pub session_id: String,
    /// null when the worktree session was exited; Some when active.
    pub worktree_session: Option<PersistedWorktreeSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedWorktreeSession {
    pub original_cwd: String,
    pub worktree_path: String,
    pub worktree_name: String,
    pub session_id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_branch: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_branch: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_head_commit: Option<String>,

    #[serde(rename = "tmuxSessionName", skip_serializing_if = "Option::is_none")]
    pub tmux_session_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_based: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentReplacementEntry {
    pub session_id: String,
    pub replacements: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistorySnapshotEntry {
    pub message_id: String,
    pub snapshot: FileHistorySnapshot,
    pub is_snapshot_update: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistorySnapshot {
    pub message_id: String,
    pub tracked_file_backups: Value,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionSnapshotEntry {
    pub message_id: String,
    pub surface: String,
    pub file_states: Value,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_count: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_count_at_last_commit: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_prompt_count: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_prompt_count_at_last_commit: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub escape_count: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub escape_count_at_last_commit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueOperationEntry {
    pub operation: String,
    pub timestamp: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

// ---------------------------------------------------------------------------
// Context-collapse entries (internal, obfuscated type names)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCollapseCommitEntry {
    pub session_id: String,
    pub collapse_id: String,
    pub summary_uuid: String,
    pub summary_content: String,
    pub summary: String,
    pub first_archived_uuid: String,
    pub last_archived_uuid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCollapseSnapshotEntry {
    pub session_id: String,
    pub staged: Vec<StagedSpan>,
    pub armed: bool,
    pub last_spawn_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StagedSpan {
    pub start_uuid: String,
    pub end_uuid: String,
    pub summary: String,
    pub risk: f64,
    pub staged_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeculationAcceptEntry {
    pub timestamp: String,
    pub time_saved_ms: u64,
}

// ---------------------------------------------------------------------------
// Session-scoped state entries
// ---------------------------------------------------------------------------

/// Latched ATIS (the short status string the client replays on resume).
/// Empty in every observed payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AtisLatchEntry {
    pub atis: String,
    pub session_id: String,
}

/// Link between a local session and the cloud bridge session backing it
/// (`/bridge`, Claude Code on the web).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeSessionEntry {
    pub session_id: String,

    /// Server-side session id, e.g. "cse_012oGoLjZDXydUaR4QRuqhk5".
    pub bridge_session_id: String,

    /// Highest bridge event sequence number acknowledged so far.
    pub last_sequence_num: u64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_account_uuid: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_organization_uuid: Option<String>,
}

/// Incremental file-history record: one tracked file backed up at one message.
/// Companion to the full `file-history-snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistoryDeltaEntry {
    /// Entry uuid this delta belongs to.
    pub message_id: String,

    /// Uuid of the snapshot this delta extends.
    pub snapshot_message_id: String,

    /// Path as tracked, relative to the backup's `real_parent_dir`.
    pub tracking_path: String,

    pub backup: FileHistoryBackup,

    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistoryBackup {
    /// Name of the backup blob, e.g. "16fd0df0a36513cf@v1"; null when the file
    /// did not exist at backup time.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_nullable"
    )]
    pub backup_file_name: Option<Option<String>>,

    pub version: u32,

    pub backup_time: String,

    /// Absolute directory `tracking_path` is relative to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub real_parent_dir: Option<String>,
}

/// Artifact / frame the session published, as surfaced in the client UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameLinkEntry {
    pub session_id: String,
    pub timestamp: String,

    /// Local file that was published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Hosted URL of the published artifact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_url: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Number of artifacts published in the session so far.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_count: Option<u32>,
}

/// Written at the head of a forked subagent transcript: where the fork's
/// inherited context came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkContextRefEntry {
    pub agent_id: String,
    pub parent_session_id: String,

    /// Last entry of the parent session included in the fork.
    pub parent_last_uuid: String,

    /// Number of parent entries carried into the fork.
    pub context_length: u64,
}

/// Per-session ledger of artifacts the session auto-reacts to (republishes
/// and comment threads it is watching).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactAutoreactLedgerEntry {
    /// Ledger format version.
    pub v: u32,
    pub session_id: String,
    pub account_uuid: String,

    /// Keyed by artifact uuid.
    pub artifacts: HashMap<String, ArtifactAutoreactState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactAutoreactState {
    /// Unix milliseconds of the last save.
    pub saved_at: u64,

    /// Highest comment stamp already reacted to; null until one is seen.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_nullable"
    )]
    pub stamp_high_water: Option<Option<Value>>,

    pub ever_baselined: bool,

    pub ever_had_threads: bool,

    /// Unix milliseconds of turns that touched the artifact.
    pub turn_timestamps: Vec<Value>,

    /// Comment threads tracked for this artifact.
    pub threads: Vec<Value>,

    /// True when auto-reaction was interrupted by the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupted: Option<bool>,
}

/// Per-session state of the artifact comment watcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactCommentMonitorEntry {
    /// State format version.
    pub v: u32,
    pub session_id: String,

    /// Keyed by artifact uuid.
    pub artifacts: HashMap<String, ArtifactCommentMonitorState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactCommentMonitorState {
    /// e.g. "armed".
    pub state: String,

    /// Unix milliseconds the state was written.
    pub written_at_ms: u64,

    pub title: String,
}

// ---------------------------------------------------------------------------
// Serde helper: distinguish JSON null from absent field
//
// Used with:
//   #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_nullable")]
//   pub field: Option<Option<T>>,
//
// Semantics:
//   None           → field absent  (skip_serializing_if prevents serialization)
//   Some(None)     → field present as JSON null
//   Some(Some(v))  → field present with value v
// ---------------------------------------------------------------------------
mod opt_nullable {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S, T>(val: &Option<Option<T>>, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize,
    {
        match val {
            None => unreachable!("skip_serializing_if = \"Option::is_none\" should prevent this"),
            Some(inner) => inner.serialize(ser),
        }
    }

    pub fn deserialize<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Ok(Some(Option::<T>::deserialize(de)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_data_unknown_variant() {
        let json = r#"{"type":"future_attachment_shape","some_field":42}"#;
        let v: AttachmentData = serde_json::from_str(json).unwrap();
        assert!(matches!(v, AttachmentData::Unknown));
    }

    #[test]
    fn attachment_data_nested_memory_variant() {
        let json = r#"{"type":"nested_memory","path":"/p/CLAUDE.md","content":{"path":"/p/CLAUDE.md","type":"Project","content":"hi","contentDiffersFromDisk":false},"displayPath":"CLAUDE.md"}"#;
        let v: AttachmentData = serde_json::from_str(json).unwrap();
        match v {
            AttachmentData::NestedMemory {
                path,
                content,
                display_path,
            } => {
                assert_eq!(path, "/p/CLAUDE.md");
                assert_eq!(content.memory_type, "Project");
                assert_eq!(content.content, "hi");
                assert_eq!(content.content_differs_from_disk, Some(false));
                assert_eq!(display_path, "CLAUDE.md");
            }
            other => panic!("expected NestedMemory, got {other:?}"),
        }
    }

    #[test]
    fn assistant_content_block_unknown_variant() {
        let json = r#"{"type":"future_modality","data":"foo"}"#;
        let v: AssistantContentBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(v, AssistantContentBlock::Unknown));
    }

    #[test]
    fn user_content_block_unknown_variant() {
        let json = r#"{"type":"video","url":"https://example.com"}"#;
        let v: UserContentBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(v, UserContentBlock::Unknown));
    }

    #[test]
    fn image_source_unknown_variant() {
        let json = r#"{"type":"s3_bucket","key":"foo"}"#;
        let v: ImageSource = serde_json::from_str(json).unwrap();
        assert!(matches!(v, ImageSource::Unknown));
    }

    #[test]
    fn document_source_unknown_variant() {
        let json = r#"{"type":"pdf","data":"base64data"}"#;
        let v: DocumentSource = serde_json::from_str(json).unwrap();
        assert!(matches!(v, DocumentSource::Unknown));
    }

    // Verify known variants still parse correctly after adding Unknown.
    #[test]
    fn attachment_data_known_variant_unaffected() {
        let json = r#"{"type":"date_change","newDate":"2024-01-01"}"#;
        let v: AttachmentData = serde_json::from_str(json).unwrap();
        assert!(matches!(v, AttachmentData::DateChange { .. }));
    }

    #[test]
    fn assistant_content_block_known_variant_unaffected() {
        let json = r#"{"type":"text","text":"hello"}"#;
        let v: AssistantContentBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(v, AssistantContentBlock::Text { .. }));
    }

    // ── New robustness tests (RED phase) ─────────────────────────────────

    /// UserContent is untagged; a JSON object (neither string nor array)
    /// must not fail — should fall through to an Other/Value catch-all.
    #[test]
    fn user_content_unknown_shape_does_not_fail() {
        let json = r#"{"type":"future_format","data":42}"#;
        let v: UserContent = serde_json::from_str(json).unwrap();
        assert!(matches!(v, UserContent::Other(_)));
    }

    /// ServerToolUse with a missing field (e.g. if Anthropic removes one)
    /// must deserialize successfully with a default of 0.
    #[test]
    fn server_tool_use_missing_field_uses_default() {
        let json = r#"{"web_search_requests":3}"#;
        let v: ServerToolUse = serde_json::from_str(json).unwrap();
        assert_eq!(v.web_search_requests, 3);
        assert_eq!(v.web_fetch_requests, 0);
    }

    /// A new / unrecognised UserRole value must parse as Unknown.
    #[test]
    fn user_role_unknown_value_does_not_fail() {
        let json = r#""operator""#;
        let v: UserRole = serde_json::from_str(json).unwrap();
        assert!(matches!(v, UserRole::Unknown));
    }

    /// A new / unrecognised AssistantRole value must parse as Unknown.
    #[test]
    fn assistant_role_unknown_value_does_not_fail() {
        let json = r#""system_agent""#;
        let v: AssistantRole = serde_json::from_str(json).unwrap();
        assert!(matches!(v, AssistantRole::Unknown));
    }

    /// A new / unrecognised SessionMode value must parse as Unknown.
    #[test]
    fn session_mode_unknown_value_does_not_fail() {
        let json = r#""background""#;
        let v: SessionMode = serde_json::from_str(json).unwrap();
        assert!(matches!(v, SessionMode::Unknown));
    }

    /// AssistantMessage without a "model" field (e.g. API error responses)
    /// must not fail deserialization.
    #[test]
    fn assistant_message_missing_model_uses_default() {
        let json = r#"{
            "id": "msg_err1",
            "type": "message",
            "role": "assistant",
            "content": [],
            "stop_reason": "error",
            "stop_sequence": null,
            "usage": {"input_tokens": 0, "output_tokens": 0}
        }"#;
        let v: AssistantMessage = serde_json::from_str(json).unwrap();
        assert!(v.model.is_none());
    }

    /// AssistantDiagnostics round-trips with cache_miss_reason and an
    /// optional cache_missed_input_tokens count.
    #[test]
    fn assistant_diagnostics_round_trip_with_tokens() {
        let json =
            r#"{"cache_miss_reason":{"type":"tools_changed","cache_missed_input_tokens":41735}}"#;
        let v: AssistantDiagnostics = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&v).unwrap();
        assert_eq!(back, json);
    }

    /// cache_missed_input_tokens is optional — absent on `unavailable` /
    /// `previous_message_not_found` variants seen in real transcripts.
    #[test]
    fn assistant_diagnostics_round_trip_without_tokens() {
        let json = r#"{"cache_miss_reason":{"type":"unavailable"}}"#;
        let v: AssistantDiagnostics = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&v).unwrap();
        assert_eq!(back, json);
    }

    /// Unknown cache_miss_reason.type values must pass through as-is, not error.
    /// (We intentionally keep `kind` as String, not an enum, to preserve the
    /// raw string for the ingest column.)
    #[test]
    fn assistant_diagnostics_unknown_kind_passes_through() {
        let json = r#"{"cache_miss_reason":{"type":"future_reason_not_yet_seen"}}"#;
        let v: AssistantDiagnostics = serde_json::from_str(json).unwrap();
        let cmr = v.cache_miss_reason.as_ref().expect("cache_miss_reason set");
        assert_eq!(cmr.kind, "future_reason_not_yet_seen");
    }

    /// AssistantMessage with `diagnostics` populated round-trips both
    /// keys (cache_miss_reason + cache_missed_input_tokens) intact.
    #[test]
    fn assistant_message_with_diagnostics_round_trip() {
        let json = r#"{"id":"msg_dx","type":"message","role":"assistant","model":"claude-opus-4-7","content":[],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1},"diagnostics":{"cache_miss_reason":{"type":"system_changed","cache_missed_input_tokens":33656}}}"#;
        let v: AssistantMessage = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&v).unwrap();
        assert_eq!(back, json);
    }

    /// `diagnostics: null` (field present, value JSON null) must round-trip
    /// as null — not be dropped. Most assistant turns in the wild have this shape.
    #[test]
    fn assistant_message_with_null_diagnostics_round_trips_as_null() {
        let json = r#"{"id":"msg_dn","type":"message","role":"assistant","model":"claude-opus-4-7","content":[],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1},"diagnostics":null}"#;
        let v: AssistantMessage = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&v).unwrap();
        assert_eq!(back, json);
    }

    /// `diagnostics` absent must deserialize cleanly and not re-emit the key.
    /// All older transcripts (pre-2026-05-05 shape change) lack the field
    /// entirely, so this is the largest population in the wild.
    #[test]
    fn assistant_message_without_diagnostics_omits_field() {
        let json = r#"{"id":"msg_da","type":"message","role":"assistant","model":"claude-opus-4-7","content":[],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1}}"#;
        let v: AssistantMessage = serde_json::from_str(json).unwrap();
        assert!(
            v.diagnostics.is_none(),
            "outer Option should be None when key absent"
        );
        let back = serde_json::to_string(&v).unwrap();
        assert_eq!(back, json, "absent field must not re-emit as null");
    }

    #[test]
    fn assistant_entry_round_trip_with_attribution_and_diagnostics() {
        let json = r#"{"uuid":"a1","parentUuid":null,"isSidechain":true,"sessionId":"s1","timestamp":"2026-05-05T00:00:00.000Z","type":"assistant","attributionAgent":"plugin1:agent1","attributionPlugin":"plugin1","message":{"id":"msg1","type":"message","role":"assistant","model":"claude-opus-4-7","content":[],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1},"diagnostics":{"cache_miss_reason":{"type":"messages_changed","cache_missed_input_tokens":204}}}}"#;
        let v: Entry = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&v).unwrap();

        let original: serde_json::Value = serde_json::from_str(json).unwrap();
        let roundtripped: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(roundtripped, original);
    }

    #[test]
    fn assistant_entry_round_trip_with_attribution_skill() {
        // Note: `attributionSkill` value is opaque (user-specific plugin slug).
        // Test fixture uses neutral placeholders — values are not part of the schema contract.
        let json = r#"{"uuid":"a2","parentUuid":null,"isSidechain":true,"sessionId":"s1","timestamp":"2026-05-05T00:00:00.000Z","type":"assistant","attributionAgent":"plugin1:agent1","attributionPlugin":"plugin1","attributionSkill":"plugin1:skill1","message":{"id":"msg1","type":"message","role":"assistant","model":"claude-opus-4-7","content":[],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1}}}"#;
        let v: Entry = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&v).unwrap();

        let original: serde_json::Value = serde_json::from_str(json).unwrap();
        let roundtripped: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(roundtripped, original);
    }

    /// `apiErrorStatus` on assistant turn round-trips as a u16. Field is
    /// populated alongside `error` / `isApiErrorMessage` on failed turns.
    #[test]
    fn assistant_entry_round_trip_with_api_error_status() {
        let json = r#"{"uuid":"a4","parentUuid":null,"isSidechain":false,"sessionId":"s1","timestamp":"2026-05-05T00:00:00.000Z","type":"assistant","isApiErrorMessage":true,"error":"rate limit","apiErrorStatus":429,"message":{"id":"msg1","type":"message","role":"assistant","model":"claude-opus-4-7","content":[],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1}}}"#;
        let v: Entry = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&v).unwrap();

        let original: serde_json::Value = serde_json::from_str(json).unwrap();
        let roundtripped: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(roundtripped, original);
    }

    /// `plan_file_reference` attachment round-trips with planFilePath
    /// and planContent.
    #[test]
    fn attachment_plan_file_reference_round_trips() {
        let json = r##"{"uuid":"a8","parentUuid":null,"isSidechain":false,"sessionId":"s1","timestamp":"2026-05-05T00:00:00.000Z","type":"attachment","attachment":{"type":"plan_file_reference","planFilePath":"/tmp/plan.md","planContent":"# Plan body"}}"##;
        let v: Entry = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&v).unwrap();
        let original: serde_json::Value = serde_json::from_str(json).unwrap();
        let roundtripped: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(roundtripped, original);
    }

    /// `deferred_tools_delta` with `readdedNames` round-trips. Also covers
    /// the omit-when-absent case implicitly via prior tests.
    #[test]
    fn attachment_deferred_tools_delta_with_readded_names_round_trips() {
        let json = r#"{"uuid":"a7","parentUuid":null,"isSidechain":false,"sessionId":"s1","timestamp":"2026-05-05T00:00:00.000Z","type":"attachment","attachment":{"type":"deferred_tools_delta","addedNames":["A"],"addedLines":["- A: foo"],"removedNames":["B"],"readdedNames":["C"]}}"#;
        let v: Entry = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&v).unwrap();
        let original: serde_json::Value = serde_json::from_str(json).unwrap();
        let roundtripped: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(roundtripped, original);
    }

    /// `auto_mode` and `auto_mode_exit` attachment variants round-trip.
    /// Sibling shape of `plan_mode` / `plan_mode_exit`.
    #[test]
    fn attachment_auto_mode_round_trips() {
        let json = r#"{"uuid":"a5","parentUuid":null,"isSidechain":false,"sessionId":"s1","timestamp":"2026-05-05T00:00:00.000Z","type":"attachment","attachment":{"type":"auto_mode","reminderType":"full"}}"#;
        let v: Entry = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&v).unwrap();
        let original: serde_json::Value = serde_json::from_str(json).unwrap();
        let roundtripped: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(roundtripped, original);
    }

    #[test]
    fn attachment_auto_mode_exit_round_trips() {
        let json = r#"{"uuid":"a6","parentUuid":null,"isSidechain":false,"sessionId":"s1","timestamp":"2026-05-05T00:00:00.000Z","type":"attachment","attachment":{"type":"auto_mode_exit"}}"#;
        let v: Entry = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&v).unwrap();
        let original: serde_json::Value = serde_json::from_str(json).unwrap();
        let roundtripped: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(roundtripped, original);
    }

    /// `agent_listing_delta` attachment round-trips with all five fields.
    /// Regression test for the 2026-05-05 attachment shape change — prior
    /// code dropped this variant into `Unknown`, losing addedTypes/addedLines
    /// /removedTypes/isInitial/showConcurrencyNote.
    #[test]
    fn attachment_agent_listing_delta_round_trips() {
        let json = r#"{"uuid":"a3","parentUuid":null,"isSidechain":false,"sessionId":"s1","timestamp":"2026-05-05T00:00:00.000Z","type":"attachment","attachment":{"type":"agent_listing_delta","addedTypes":["Explore","plugin1:agent1"],"addedLines":["- Explore: Fast read-only search","- plugin1:agent1: example"],"removedTypes":[],"isInitial":true,"showConcurrencyNote":true}}"#;
        let v: Entry = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&v).unwrap();

        let original: serde_json::Value = serde_json::from_str(json).unwrap();
        let roundtripped: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(roundtripped, original);
    }
}

#[cfg(test)]
mod format_2026_08_tests {
    use super::*;

    /// Parse, re-serialise, and assert the JSON is byte-for-byte equivalent.
    fn rt(json: &str) {
        let v: Entry = serde_json::from_str(json).expect("parse");
        let back = serde_json::to_string(&v).expect("serialize");
        let original: serde_json::Value = serde_json::from_str(json).unwrap();
        let roundtripped: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(roundtripped, original);
    }

    const ENV: &str = r#""uuid":"u1","parentUuid":null,"isSidechain":false,"sessionId":"s1","timestamp":"2026-08-20T15:12:33.674Z""#;

    // ── Envelope additions ───────────────────────────────────────────────

    #[test]
    fn envelope_session_kind_and_snake_session_id_round_trip() {
        rt(&format!(
            r#"{{{ENV},"session_id":"s0","sessionKind":"bg","type":"user","message":{{"role":"user","content":"hi"}}}}"#
        ));
    }

    /// The snake-case `session_id` is a distinct field, not an alias: it can
    /// hold a different id from `sessionId` on resumed sessions.
    #[test]
    fn snake_session_id_does_not_collide_with_camel() {
        let json = format!(
            r#"{{{ENV},"session_id":"s0","type":"user","message":{{"role":"user","content":"hi"}}}}"#
        );
        match serde_json::from_str::<Entry>(&json).unwrap() {
            Entry::User(u) => {
                assert_eq!(u.envelope.session_id, "s1");
                assert_eq!(u.envelope.session_id_snake.as_deref(), Some("s0"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    // ── Assistant additions ──────────────────────────────────────────────

    #[test]
    fn assistant_effort_mcp_attribution_and_thinking_tokens_round_trip() {
        rt(&format!(
            r#"{{{ENV},"type":"assistant","effort":"xhigh","attributionMcpServer":"plugin:context7:context7","attributionMcpTool":"resolve-library-id","message":{{"id":"m1","type":"message","role":"assistant","model":"claude-opus-5","content":[],"stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":1,"output_tokens":9,"output_tokens_details":{{"thinking_tokens":4}}}}}}}}"#
        ));
    }

    /// `output_tokens_details: null` is common in the wild and must survive as
    /// null rather than being dropped.
    #[test]
    fn assistant_null_output_tokens_details_round_trips_as_null() {
        rt(&format!(
            r#"{{{ENV},"type":"assistant","message":{{"id":"m1","type":"message","role":"assistant","model":"claude-opus-5","content":[],"stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":1,"output_tokens":1,"output_tokens_details":null}}}}}}"#
        ));
    }

    #[test]
    fn assistant_quota_limits_and_abort_fields_round_trip() {
        rt(&format!(
            r#"{{{ENV},"type":"assistant","isAbortedMidStream":true,"errorDetails":"429 {{\"type\":\"error\"}}","quotaLimits":{{"status":"rejected","resetsAt":1787244600,"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"five_hour","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false,"upgradePaths":["upgrade_plan"]}},"message":{{"id":"m1","type":"message","role":"assistant","model":"claude-opus-5","content":[],"stop_reason":null,"stop_sequence":null,"usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#
        ));
    }

    // ── User additions ───────────────────────────────────────────────────

    #[test]
    fn user_prompt_metadata_fields_round_trip() {
        rt(&format!(
            r#"{{{ENV},"type":"user","promptSource":"typed","queuePriority":"later","turnCompanion":true,"toolDenialKind":"user-rejected","interruptedMessageId":"msg_1","classifierMetaLines":"{{\"meta\":{{}}}}\n","userFeedback":"clarify","mcpMeta":{{"_meta":{{"io.modelcontextprotocol/serverInfo":{{"name":"Context7"}}}}}},"message":{{"role":"user","content":"hi"}}}}"#
        ));
    }

    // ── System additions ─────────────────────────────────────────────────

    #[test]
    fn system_compact_preserved_messages_round_trips() {
        rt(&format!(
            r#"{{{ENV},"type":"system","subtype":"compact_boundary","compactMetadata":{{"trigger":"auto","preTokens":10,"postTokens":2,"cumulativeDroppedTokens":8,"durationMs":5,"preservedSegment":{{"headUuid":"h","anchorUuid":"a","tailUuid":"t"}},"preservedMessages":{{"anchorUuid":"a","uuids":["h","t"],"allUuids":["h","x","t"]}}}}}}"#
        ));
    }

    #[test]
    fn system_stop_hook_summary_extras_round_trip() {
        rt(&format!(
            r#"{{{ENV},"type":"system","subtype":"stop_hook_summary","hookCount":1,"hookInfos":[{{"command":"sh hook.sh","durationMs":12,"promptText":"keep going"}}],"hookAdditionalContext":[],"pendingBackgroundAgentCount":6}}"#
        ));
    }

    #[test]
    fn system_cron_kind_round_trips() {
        rt(&format!(
            r#"{{{ENV},"type":"system","subtype":"scheduled_task_fire","cronKind":"loop"}}"#
        ));
    }

    // ── Attachment variants ──────────────────────────────────────────────

    #[test]
    fn attachment_total_tokens_reminder_round_trips() {
        rt(&format!(
            r#"{{{ENV},"type":"attachment","attachment":{{"type":"total_tokens_reminder","text":"<total_tokens>15000000 tokens left</total_tokens>"}}}}"#
        ));
    }

    #[test]
    fn attachment_read_truncation_notice_round_trips() {
        rt(&format!(
            r#"{{{ENV},"type":"attachment","attachment":{{"type":"read_truncation_notice","banner":"[Truncated: PARTIAL view]","toolUseID":"toolu_1"}}}}"#
        ));
    }

    #[test]
    fn attachment_goal_status_round_trips() {
        rt(&format!(
            r#"{{{ENV},"type":"attachment","attachment":{{"type":"goal_status","met":false,"condition":"keep going","sentinel":true,"reason":"still running","iterations":2,"durationMs":9390812,"tokens":806003}}}}"#
        ));
    }

    /// `deltaSummary` is always present and may be null — it must round-trip as
    /// null rather than being dropped.
    #[test]
    fn attachment_task_status_round_trips_with_null_delta_summary() {
        rt(&format!(
            r#"{{{ENV},"type":"attachment","attachment":{{"type":"task_status","taskId":"a1","taskType":"local_agent","description":"Fix contracts","status":"running","deltaSummary":null,"outputFilePath":"/tmp/a1.output"}}}}"#
        ));
    }

    /// Newer clients describe auto mode through flags and omit `reminderType`
    /// entirely; older ones sent only `reminderType`.
    #[test]
    fn attachment_auto_mode_flag_shape_round_trips() {
        rt(&format!(
            r#"{{{ENV},"type":"attachment","attachment":{{"type":"auto_mode","autoModeConsentFlow":false,"bashFirst":true,"steerOnly":true,"bypass":false}}}}"#
        ));
    }

    #[test]
    fn attachment_auto_mode_exit_with_flags_round_trips() {
        rt(&format!(
            r#"{{{ENV},"type":"attachment","attachment":{{"type":"auto_mode_exit","bashFirst":true,"steerOnly":true}}}}"#
        ));
    }

    #[test]
    fn attachment_queued_command_extras_round_trip() {
        rt(&format!(
            r#"{{{ENV},"type":"attachment","attachment":{{"type":"queued_command","prompt":"go","commandMode":"prompt","timestamp":"2026-08-20T15:32:26.583Z","origin":{{"kind":"human"}},"source_uuid":"u0","isMeta":true}}}}"#
        ));
    }

    #[test]
    fn attachment_skill_listing_names_round_trips() {
        rt(&format!(
            r#"{{{ENV},"type":"attachment","attachment":{{"type":"skill_listing","content":"- a: x","skillCount":1,"isInitial":true,"names":["a"]}}}}"#
        ));
    }

    #[test]
    fn attachment_deferred_tools_delta_pending_mcp_servers_round_trips() {
        rt(&format!(
            r#"{{{ENV},"type":"attachment","attachment":{{"type":"deferred_tools_delta","addedNames":["A"],"addedLines":["A"],"removedNames":[],"readdedNames":[],"pendingMcpServers":[]}}}}"#
        ));
    }

    #[test]
    fn attachment_notebook_file_cells_round_trip() {
        rt(&format!(
            r#"{{{ENV},"type":"attachment","attachment":{{"type":"file","filename":"/n.ipynb","content":{{"type":"text","file":{{"filePath":"/n.ipynb","cells":[{{"cellType":"code","cell_id":"c1","language":"python","source":"x=1","execution_count":1,"outputs":[{{"output_type":"error","text":"boom"}}]}},{{"cellType":"markdown","cell_id":"c2","language":"markdown","source":"hi"}}]}}}},"displayPath":"n.ipynb"}}}}"#
        ));
    }

    // ── New top-level entry types ────────────────────────────────────────

    #[test]
    fn atis_latch_round_trips() {
        rt(r#"{"type":"atis-latch","atis":"","sessionId":"s1"}"#);
    }

    #[test]
    fn bridge_session_round_trips() {
        rt(
            r#"{"type":"bridge-session","sessionId":"s1","bridgeSessionId":"cse_1","lastSequenceNum":0,"ownerAccountUuid":"a1","ownerOrganizationUuid":"o1"}"#,
        );
    }

    #[test]
    fn bridge_session_without_owner_uuids_round_trips() {
        rt(
            r#"{"type":"bridge-session","sessionId":"s1","bridgeSessionId":"cse_1","lastSequenceNum":3}"#,
        );
    }

    /// `backupFileName` is null when the tracked file did not exist yet.
    #[test]
    fn file_history_delta_round_trips_with_null_backup_name() {
        rt(
            r#"{"type":"file-history-delta","messageId":"m1","snapshotMessageId":"m0","trackingPath":"a.py","backup":{"backupFileName":null,"version":1,"backupTime":"2026-08-23T16:51:45.058Z","realParentDir":"/src"},"timestamp":"2026-08-23T16:51:45.058Z"}"#,
        );
    }

    #[test]
    fn frame_link_round_trips() {
        rt(
            r#"{"type":"frame-link","sessionId":"s1","timestamp":"2026-08-23T11:03:16.000Z","path":"/tmp/a.html","frameUrl":"https://claude.ai/code/artifact/x","title":"T","artifactCount":1}"#,
        );
    }

    /// Heartbeat rows carry only the session id and timestamp.
    #[test]
    fn frame_link_minimal_round_trips() {
        rt(r#"{"type":"frame-link","sessionId":"s1","timestamp":"2026-08-23T11:12:03.314Z"}"#);
    }

    #[test]
    fn fork_context_ref_round_trips() {
        rt(
            r#"{"type":"fork-context-ref","agentId":"abt4","parentSessionId":"s0","parentLastUuid":"u0","contextLength":500}"#,
        );
    }

    #[test]
    fn artifact_autoreact_ledger_round_trips() {
        rt(
            r#"{"type":"artifact-autoreact-ledger","v":1,"sessionId":"s1","accountUuid":"a1","artifacts":{"art1":{"savedAt":1787486401550,"stampHighWater":null,"everBaselined":true,"everHadThreads":false,"turnTimestamps":[],"threads":[],"interrupted":true}}}"#,
        );
    }

    #[test]
    fn artifact_comment_monitor_round_trips() {
        rt(
            r#"{"type":"artifact-comment-monitor","v":1,"sessionId":"s1","artifacts":{"art1":{"state":"armed","writtenAtMs":1787483938784,"title":"T"}}}"#,
        );
    }

    // ── Absent (not null) nullable fields ────────────────────────────────
    //
    // Every `opt_nullable` field must also carry `skip_serializing_if`, or an
    // absent key deserialises to the outer `None` that `serialize` treats as
    // unreachable. These three cover the fields added in this batch.

    #[test]
    fn attachment_task_status_round_trips_without_delta_summary() {
        rt(&format!(
            r#"{{{ENV},"type":"attachment","attachment":{{"type":"task_status","taskId":"a1","taskType":"local_agent","description":"Fix contracts","status":"running","outputFilePath":"/tmp/a1.output"}}}}"#
        ));
    }

    #[test]
    fn file_history_delta_round_trips_without_backup_name() {
        rt(
            r#"{"type":"file-history-delta","messageId":"m1","snapshotMessageId":"m0","trackingPath":"a.py","backup":{"version":1,"backupTime":"2026-08-23T16:51:45.058Z"},"timestamp":"2026-08-23T16:51:45.058Z"}"#,
        );
    }

    #[test]
    fn artifact_autoreact_ledger_round_trips_without_stamp_high_water() {
        rt(
            r#"{"type":"artifact-autoreact-ledger","v":1,"sessionId":"s1","accountUuid":"a1","artifacts":{"art1":{"savedAt":1787486401550,"everBaselined":true,"everHadThreads":false,"turnTimestamps":[],"threads":[],"interrupted":true}}}"#,
        );
    }
}

#[cfg(test)]
mod last_prompt_tests {
    use super::*;

    fn parse(line: &str) -> Entry {
        serde_json::from_str::<Entry>(line).expect("parse")
    }

    #[test]
    fn last_prompt_old_format_inline_text() {
        let e = parse(r#"{"type":"last-prompt","lastPrompt":"hello world","sessionId":"S"}"#);
        match e {
            Entry::LastPrompt(x) => {
                assert_eq!(x.last_prompt.as_deref(), Some("hello world"));
                assert_eq!(x.leaf_uuid, None);
                assert_eq!(x.session_id, "S");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn last_prompt_new_format_leaf_uuid_only() {
        let e = parse(r#"{"type":"last-prompt","leafUuid":"u1","sessionId":"S"}"#);
        match e {
            Entry::LastPrompt(x) => {
                assert_eq!(x.last_prompt, None);
                assert_eq!(x.leaf_uuid.as_deref(), Some("u1"));
                assert_eq!(x.session_id, "S");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn last_prompt_hypothetical_both_fields() {
        let e = parse(
            r#"{"type":"last-prompt","lastPrompt":"inline","leafUuid":"u2","sessionId":"S"}"#,
        );
        match e {
            Entry::LastPrompt(x) => {
                assert_eq!(x.last_prompt.as_deref(), Some("inline"));
                assert_eq!(x.leaf_uuid.as_deref(), Some("u2"));
                assert_eq!(x.session_id, "S");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn last_prompt_hypothetical_neither_field() {
        let e = parse(r#"{"type":"last-prompt","sessionId":"S"}"#);
        match e {
            Entry::LastPrompt(x) => {
                assert_eq!(x.last_prompt, None);
                assert_eq!(x.leaf_uuid, None);
                assert_eq!(x.session_id, "S");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
