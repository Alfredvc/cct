use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Top-level Entry — one per JSONL line
// ---------------------------------------------------------------------------

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

    // ── Skills ───────────────────────────────────────────────────────────
    SkillListing {
        content: String,
        /// True on the very first skill listing injection for a session.
        #[serde(rename = "isInitial", skip_serializing_if = "Option::is_none")]
        is_initial: Option<bool>,
        /// Total number of skills listed.
        #[serde(rename = "skillCount", skip_serializing_if = "Option::is_none")]
        skill_count: Option<u32>,
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
    },

    McpInstructionsDelta {
        #[serde(rename = "addedNames")]
        added_names: Vec<String>,
        #[serde(rename = "addedBlocks")]
        added_blocks: Vec<String>,
        #[serde(rename = "removedNames", skip_serializing_if = "Option::is_none")]
        removed_names: Option<Vec<String>>,
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
    },

    // ── Nested memory (CLAUDE.md imports) ────────────────────────────────
    NestedMemory {
        path: String,
        content: NestedMemoryContent,
        #[serde(rename = "displayPath")]
        display_path: String,
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
