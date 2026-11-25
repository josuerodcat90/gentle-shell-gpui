//! Client adapter for `gentle-shell.desktop-rpc/v1`.
//!
//! The desktop app talks to Gentle Shell's extension, not to a session: each
//! call is a standard Pi RPC prompt whose message is `/gentle:desktop <json>`,
//! where `<json>` is the inner Desktop request. Pi answers that prompt with a
//! plain `response` record — `disposition: "handled"` only means the request
//! was accepted — and the domain answer arrives separately as an
//! `extension_ui_request` with method `setWidget`, widget key
//! `gentle-desktop-rpc` and the JSON record in `widgetLines`.
//!
//! This module owns the wire format and the correlation rules, no transport:
//! it builds the outer command to hand to [`crate::PiProcess::request`] and
//! decodes the widget records that arrive in [`crate::Incoming::Ui`]. The Pi
//! request `id` is assigned by the transport and never identifies a Desktop
//! request; correlation uses the inner `requestId` only.
//!
//! [`DesktopSession`] also owns the two recovery paths the protocol documents:
//! [`DesktopSession::abandon`] for a request that will never be answered (a
//! transport timeout, or a disposition other than `handled`), and
//! [`DesktopSession::reset`] / [`DesktopSession::resync_from_snapshot`] for a
//! backend restart or a lost event sequence.
//!
//! The server validates the domain: the client keeps only the rules the
//! contract documents. It checks input before sending only where the server
//! would merely refuse it — `set_config` refuses an empty `expectedRevision`
//! and an empty pin profile (a clear is an explicit `profile: null`), both of
//! which the server would only answer with `revision_conflict` /
//! `invalid_data`; `apply_profile` and `set_review_mode` refuse a profile name
//! or confirmation token outside the documented 1–128 characters; the id
//! reads refuse an empty id, `list_sessions` a limit outside 1–50, and
//! `get_snapshot` an empty or repeated section list.
//!
//! Which methods and events a peer offers is only what its `capabilities`
//! say: [`Capabilities::require`] refuses a method it does not list before a
//! request exists, so a Gentle Shell release older than a method is never
//! sent it.

use crate::record::{Response as PiResponse, UiMethod, UiRequest};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt;

/// A closed wire vocabulary: the enum, its wire names, parsing (an unknown
/// name is [`DesktopRpcError::InvalidValue`] for `$field`) and decoding, so a
/// value outside the documented set is refused instead of misread.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $name:ident, $field:literal {
            $($variant:ident => $wire:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            /// Every value the protocol speaks, in wire order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The wire name.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = $crate::desktop_rpc::DesktopRpcError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($wire => Ok(Self::$variant),)+
                    other => Err($crate::desktop_rpc::DesktopRpcError::InvalidValue {
                        field: $field,
                        value: other.to_string(),
                    }),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                <String as serde::Deserialize>::deserialize(deserializer)?
                    .parse()
                    .map_err(serde::de::Error::custom)
            }
        }
    };
}

mod auth;
mod background;
mod binding;
mod changes;
mod definitions;
mod output;
mod review;
mod session_state;
mod sessions;
mod stats;
mod status;
mod updates;

pub use auth::{AuthSource, AuthStatus, AuthType, ProviderAuth, ReloadCredentials};
pub use background::{
    BackgroundCapability, BackgroundConfirmation, BackgroundPolicy, BackgroundSource,
    BackgroundSubagents, PolicyFile, SetBackgroundSubagentsOutcome, SetBackgroundSubagentsResult,
};
pub use binding::{
    ClearBindingStatus, ClearProfileBindingResult, ProfileBinding, ProfileBindingChanged,
    ProfileBindingKind, ProfileBindingOrigin,
};
pub use changes::{ChangeDiff, ChangeRoot, ChangeStatus, ChangedFile, Changes};
pub use definitions::{
    AgentDefinition, AgentDefinitionProblems, AgentDefinitionScope, AgentDefinitionSource,
    AgentDefinitions, DefaultOrchestrator, OrchestratorStatus, SubagentDefaults,
};
pub use output::{JobOutput, SubagentThread, ThreadItem};
pub use review::{
    ReviewConfirmation, ReviewEffective, ReviewMode, ReviewModeChanged, ReviewScope, ReviewScopes,
    ReviewSetting, ReviewSource, SetReviewModeOutcome, SetReviewModeResult,
};
pub use session_state::{
    ActivityState, CommandSource, PendingQuestionKind, ServiceTier, SessionActivity,
    SessionAgentItem, SessionAgentRuns, SessionAgents, SessionChangedFile, SessionChanges,
    SessionChangesSummary, SessionCommand, SessionCommands, SessionContext, SessionCost,
    SessionJobs, SessionModel, SessionModelProfile, SessionOrchestratorRoute,
    SessionPendingQuestion, SessionPermission, SessionProfileItem, SessionProfiles, SessionProject,
    SessionQuestion, SessionQuestionOption, SessionRecentJob, SessionReview, SessionRoleRoute,
    SessionRunningJob, SessionSection, SessionSections, SessionSnapshotEvent, SessionSpend,
    SessionStats, SessionStatsModel, SessionTodo, SessionTodoItem, SessionTodoProgress,
    SessionUsage, SessionUsageEntry, SessionUsageLimit, SessionUsageWindow, SessionWorktree,
};
pub use sessions::{
    ListSessionsQuery, MAX_SESSION_ID_UNITS, MAX_SESSIONS_LIMIT, SessionEntry, SessionList,
    SessionsScope, SwitchSessionResult,
};
pub use stats::{
    StatsRange, StatsScope, UsageStats, UsageStatsDay, UsageStatsModel, UsageStatsProject,
    UsageStatsProvider, UsageStatsTotals, UsageStatsTruncated,
};
pub use status::{
    AssetStatus, DevBinary, DevBinaryState, ModelConfigPresence, ModelConfigStatus, RoutingState,
    ShellStatus, StatusAssets, StatusSeverity,
};
pub use updates::{
    PackageKind, PackageScope, PackageUpdate, PackageUpdateState, PackagesUpdate, PiUpdate,
    ShellChannel, ShellUpdate, UpdateState, UpdateStatus,
};

/// Schema of the inner request, response and event records.
pub const SCHEMA: &str = "gentle-shell.desktop-rpc/v1";
/// Slash command that carries an inner request to Gentle Shell's extension.
pub const PROMPT_COMMAND: &str = "/gentle:desktop";
/// Widget key the Desktop records are published under.
pub const WIDGET_KEY: &str = "gentle-desktop-rpc";
/// Largest inner request accepted, in bytes of its JSON encoding.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// Largest inner `requestId`, in UTF-16 code units — the unit the server
/// counts, so non-BMP characters use two.
pub const MAX_REQUEST_ID_UNITS: usize = 128;

/// The inner methods this client knows. Anything else is a server or
/// protocol change and is refused instead of guessed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DesktopMethod {
    Capabilities,
    GetSnapshot,
    GetPermissionMode,
    SetPermissionMode,
    ListSubagents,
    CancelSubagent,
    ListJobs,
    CancelJob,
    GetSubscriptionUsage,
    RefreshSubscriptionUsage,
    GetDesignCanvas,
    GetConfig,
    SetConfig,
    ApplyProfile,
    GetJobOutput,
    GetSubagentThread,
    ListSessions,
    SwitchSession,
    GetChanges,
    GetChangeDiff,
    GetReviewMode,
    SetReviewMode,
    GetUsageStats,
    GetUpdateStatus,
    ListAgentDefinitions,
    GetStatus,
    GetBackgroundSubagents,
    SetBackgroundSubagents,
    GetProfileBinding,
    ClearProfileBinding,
    GetAuthStatus,
    ReloadCredentials,
}

impl DesktopMethod {
    /// Every method this client knows, in the order `capabilities` lists
    /// them on Gentle Shell 5.0.0. Which ones a peer offers is only what its
    /// `capabilities` say: Gentle Shell 4.8.0 offers fourteen of them.
    pub const ALL: [Self; 32] = [
        Self::Capabilities,
        Self::GetSnapshot,
        Self::GetConfig,
        Self::SetConfig,
        Self::GetPermissionMode,
        Self::SetPermissionMode,
        Self::GetSubscriptionUsage,
        Self::RefreshSubscriptionUsage,
        Self::GetDesignCanvas,
        Self::ListJobs,
        Self::CancelJob,
        Self::GetJobOutput,
        Self::ApplyProfile,
        Self::ListSubagents,
        Self::CancelSubagent,
        Self::GetSubagentThread,
        Self::ListSessions,
        Self::SwitchSession,
        Self::GetChanges,
        Self::GetChangeDiff,
        Self::GetReviewMode,
        Self::SetReviewMode,
        Self::GetUsageStats,
        Self::GetUpdateStatus,
        Self::ListAgentDefinitions,
        Self::GetStatus,
        Self::GetBackgroundSubagents,
        Self::SetBackgroundSubagents,
        Self::GetProfileBinding,
        Self::ClearProfileBinding,
        Self::GetAuthStatus,
        Self::ReloadCredentials,
    ];

    /// The wire name of the method.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Capabilities => "capabilities",
            Self::GetSnapshot => "get_snapshot",
            Self::GetPermissionMode => "get_permission_mode",
            Self::SetPermissionMode => "set_permission_mode",
            Self::ListSubagents => "list_subagents",
            Self::CancelSubagent => "cancel_subagent",
            Self::ListJobs => "list_jobs",
            Self::CancelJob => "cancel_job",
            Self::GetSubscriptionUsage => "get_subscription_usage",
            Self::RefreshSubscriptionUsage => "refresh_subscription_usage",
            Self::GetDesignCanvas => "get_design_canvas",
            Self::GetConfig => "get_config",
            Self::SetConfig => "set_config",
            Self::ApplyProfile => "apply_profile",
            Self::GetJobOutput => "get_job_output",
            Self::GetSubagentThread => "get_subagent_thread",
            Self::ListSessions => "list_sessions",
            Self::SwitchSession => "switch_session",
            Self::GetChanges => "get_changes",
            Self::GetChangeDiff => "get_change_diff",
            Self::GetReviewMode => "get_review_mode",
            Self::SetReviewMode => "set_review_mode",
            Self::GetUsageStats => "get_usage_stats",
            Self::GetUpdateStatus => "get_update_status",
            Self::ListAgentDefinitions => "list_agent_definitions",
            Self::GetStatus => "get_status",
            Self::GetBackgroundSubagents => "get_background_subagents",
            Self::SetBackgroundSubagents => "set_background_subagents",
            Self::GetProfileBinding => "get_profile_binding",
            Self::ClearProfileBinding => "clear_profile_binding",
            Self::GetAuthStatus => "get_auth_status",
            Self::ReloadCredentials => "reload_credentials",
        }
    }

    /// Whether the method's params can only be built by its typed
    /// constructor: the config methods name a store, never a path,
    /// `apply_profile` takes exactly its documented keys, and the methods with
    /// a required param (an id, a root and path, a review or
    /// background-subagents change) would only be refused by the server with
    /// `{}`, so the generic `params` path is closed to them.
    fn requires_typed_params(self) -> bool {
        matches!(
            self,
            Self::GetConfig
                | Self::SetConfig
                | Self::ApplyProfile
                | Self::GetJobOutput
                | Self::GetSubagentThread
                | Self::SwitchSession
                | Self::GetChangeDiff
                | Self::SetReviewMode
                | Self::SetBackgroundSubagents
        )
    }

    /// Parse a wire method name.
    pub fn parse(method: &str) -> Result<Self, DesktopRpcError> {
        match method {
            "capabilities" => Ok(Self::Capabilities),
            "get_snapshot" => Ok(Self::GetSnapshot),
            "get_permission_mode" => Ok(Self::GetPermissionMode),
            "set_permission_mode" => Ok(Self::SetPermissionMode),
            "list_subagents" => Ok(Self::ListSubagents),
            "cancel_subagent" => Ok(Self::CancelSubagent),
            "list_jobs" => Ok(Self::ListJobs),
            "cancel_job" => Ok(Self::CancelJob),
            "get_subscription_usage" => Ok(Self::GetSubscriptionUsage),
            "refresh_subscription_usage" => Ok(Self::RefreshSubscriptionUsage),
            "get_design_canvas" => Ok(Self::GetDesignCanvas),
            "get_config" => Ok(Self::GetConfig),
            "set_config" => Ok(Self::SetConfig),
            "apply_profile" => Ok(Self::ApplyProfile),
            "get_job_output" => Ok(Self::GetJobOutput),
            "get_subagent_thread" => Ok(Self::GetSubagentThread),
            "list_sessions" => Ok(Self::ListSessions),
            "switch_session" => Ok(Self::SwitchSession),
            "get_changes" => Ok(Self::GetChanges),
            "get_change_diff" => Ok(Self::GetChangeDiff),
            "get_review_mode" => Ok(Self::GetReviewMode),
            "set_review_mode" => Ok(Self::SetReviewMode),
            "get_usage_stats" => Ok(Self::GetUsageStats),
            "get_update_status" => Ok(Self::GetUpdateStatus),
            "list_agent_definitions" => Ok(Self::ListAgentDefinitions),
            "get_status" => Ok(Self::GetStatus),
            "get_background_subagents" => Ok(Self::GetBackgroundSubagents),
            "set_background_subagents" => Ok(Self::SetBackgroundSubagents),
            "get_profile_binding" => Ok(Self::GetProfileBinding),
            "clear_profile_binding" => Ok(Self::ClearProfileBinding),
            "get_auth_status" => Ok(Self::GetAuthStatus),
            "reload_credentials" => Ok(Self::ReloadCredentials),
            other => Err(DesktopRpcError::UnsupportedMethod(other.to_string())),
        }
    }
}

impl fmt::Display for DesktopMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for DesktopMethod {
    type Err = DesktopRpcError;

    fn from_str(method: &str) -> Result<Self, Self::Err> {
        Self::parse(method)
    }
}

/// The permission modes the wire defines. The wire name is lowercase snake
/// case; `super-yolo` is the only mode with a hyphen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionMode {
    Normal,
    Auto,
    Yolo,
    SuperYolo,
}

impl PermissionMode {
    /// Every mode the protocol speaks, in wire order.
    pub const ALL: [Self; 4] = [Self::Normal, Self::Auto, Self::Yolo, Self::SuperYolo];

    /// The wire name of the mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Auto => "auto",
            Self::Yolo => "yolo",
            Self::SuperYolo => "super-yolo",
        }
    }
}

impl serde::Serialize for PermissionMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl std::str::FromStr for PermissionMode {
    type Err = DesktopRpcError;

    fn from_str(mode: &str) -> Result<Self, Self::Err> {
        match mode {
            "normal" => Ok(Self::Normal),
            "auto" => Ok(Self::Auto),
            "yolo" => Ok(Self::Yolo),
            "super-yolo" => Ok(Self::SuperYolo),
            other => Err(DesktopRpcError::InvalidPermissionMode(other.to_string())),
        }
    }
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The closed `lastStep` vocabulary of a subagent entry. The server derives
/// this label from the task status and a fixed set of safe active steps; raw
/// `TaskRecord.lastStep` text (an error, a stderr tail, a provider body) is
/// never forwarded, so any value outside this set is refused rather than
/// passed through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DesktopAgentStep {
    Queued,
    Running,
    Waiting,
    Starting,
    Writing,
    CleaningUp,
    GeneratingToolArguments,
    QuestionDismissed,
    Answered,
    Interrupted,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

impl DesktopAgentStep {
    /// Every step label the protocol speaks, in wire order.
    pub const ALL: [Self; 14] = [
        Self::Queued,
        Self::Running,
        Self::Waiting,
        Self::Starting,
        Self::Writing,
        Self::CleaningUp,
        Self::GeneratingToolArguments,
        Self::QuestionDismissed,
        Self::Answered,
        Self::Interrupted,
        Self::Completed,
        Self::Failed,
        Self::Cancelled,
        Self::TimedOut,
    ];

    /// The wire name of the step label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Starting => "starting",
            Self::Writing => "writing",
            Self::CleaningUp => "cleaning up",
            Self::GeneratingToolArguments => "generating tool arguments",
            Self::QuestionDismissed => "question dismissed",
            Self::Answered => "answered",
            Self::Interrupted => "interrupted",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }
}

impl std::str::FromStr for DesktopAgentStep {
    type Err = DesktopRpcError;

    fn from_str(step: &str) -> Result<Self, Self::Err> {
        match step {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "waiting" => Ok(Self::Waiting),
            "starting" => Ok(Self::Starting),
            "writing" => Ok(Self::Writing),
            "cleaning up" => Ok(Self::CleaningUp),
            "generating tool arguments" => Ok(Self::GeneratingToolArguments),
            "question dismissed" => Ok(Self::QuestionDismissed),
            "answered" => Ok(Self::Answered),
            "interrupted" => Ok(Self::Interrupted),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "timed_out" => Ok(Self::TimedOut),
            other => Err(DesktopRpcError::InvalidLastStep(other.to_string())),
        }
    }
}

impl fmt::Display for DesktopAgentStep {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One validated inner request, ready to be sent as a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRequest {
    request_id: String,
    method: DesktopMethod,
    payload: String,
}

#[derive(serde::Serialize)]
struct WireRequest<'a> {
    schema: &'a str,
    #[serde(rename = "requestId")]
    request_id: &'a str,
    method: &'a str,
    params: &'a Value,
}

/// The typed params of `set_permission_mode`; `persist` is omitted rather than
/// sent as `null`, so the server's documented default applies.
#[derive(serde::Serialize)]
struct SetPermissionParams {
    mode: PermissionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    persist: Option<bool>,
}

/// The wire `params` object for `set_permission_mode`.
fn set_permission_mode_params(
    mode: PermissionMode,
    persist: Option<bool>,
) -> Result<Value, DesktopRpcError> {
    serde_json::to_value(SetPermissionParams { mode, persist })
        .map_err(|error| DesktopRpcError::MalformedPayload(error.to_string()))
}

/// The wire `params` object for `cancel_subagent` and `cancel_job`: exactly
/// `{ id }` with a non-empty string id, nothing else.
fn cancel_params(id: String) -> Result<Value, DesktopRpcError> {
    if id.is_empty() {
        return Err(DesktopRpcError::InvalidCancelId);
    }
    Ok(json!({ "id": id }))
}

/// The wire `params` object for `get_subscription_usage` and
/// `refresh_subscription_usage`: `{}` for the whole account, or exactly
/// `{ provider }` with a non-empty provider string.
fn subscription_usage_params(provider: Option<&str>) -> Result<Value, DesktopRpcError> {
    match provider {
        None => Ok(json!({})),
        Some(provider) => {
            if provider.is_empty() {
                return Err(DesktopRpcError::InvalidProvider);
            }
            Ok(json!({ "provider": provider }))
        }
    }
}

/// The wire `params` object for `get_design_canvas`: `{}` when `include_url`
/// is `None`, or exactly `{ includeUrl }` with the explicit boolean. No other
/// key is ever sent.
fn get_design_canvas_params(include_url: Option<bool>) -> Value {
    match include_url {
        None => json!({}),
        Some(include_url) => json!({ "includeUrl": include_url }),
    }
}

/// The wire `params` object for `get_snapshot` with session sections:
/// exactly `{ sections }`, a non-empty list of unique names. The server
/// answers them in canonical order whatever order they were asked in.
fn snapshot_sections_params(sections: &[SessionSection]) -> Result<Value, DesktopRpcError> {
    let unique = sections
        .iter()
        .enumerate()
        .all(|(index, section)| !sections[..index].contains(section));
    if sections.is_empty() || !unique {
        return Err(DesktopRpcError::InvalidSessionSections);
    }
    let names: Vec<&str> = sections.iter().map(|section| section.as_str()).collect();
    Ok(json!({ "sections": names }))
}

/// The wire `params` object for `get_job_output`: exactly `{ id }` with a
/// non-empty id.
fn job_output_params(id: &str) -> Result<Value, DesktopRpcError> {
    if id.is_empty() {
        return Err(DesktopRpcError::InvalidId(DesktopMethod::GetJobOutput));
    }
    Ok(json!({ "id": id }))
}

/// The wire `params` object for `get_change_diff`: exactly `{ root, path }`,
/// as `get_changes` listed them. Whether they name a changed file is the
/// server's to answer (`not_found`).
fn change_diff_params(root: &str, path: &str) -> Value {
    json!({ "root": root, "path": path })
}

/// The wire `params` object for `get_update_status`: `{}` for a read of what
/// is already known, or exactly `{ refresh }`.
fn update_status_params(refresh: Option<bool>) -> Value {
    match refresh {
        None => json!({}),
        Some(refresh) => json!({ "refresh": refresh }),
    }
}

impl DesktopRequest {
    /// The `capabilities` request. It takes no params.
    pub fn capabilities(request_id: impl Into<String>) -> Result<Self, DesktopRpcError> {
        Self::new(request_id, DesktopMethod::Capabilities, json!({}))
    }

    /// The `get_snapshot` request. It takes no params.
    pub fn get_snapshot(request_id: impl Into<String>) -> Result<Self, DesktopRpcError> {
        Self::new(request_id, DesktopMethod::GetSnapshot, json!({}))
    }

    /// The `get_permission_mode` request. It takes no params.
    pub fn get_permission_mode(request_id: impl Into<String>) -> Result<Self, DesktopRpcError> {
        Self::new(request_id, DesktopMethod::GetPermissionMode, json!({}))
    }

    /// The `set_permission_mode` request. `persist` is `None` to let the
    /// server default it to `false`, or `Some` to send an explicit flag.
    pub fn set_permission_mode(
        request_id: impl Into<String>,
        mode: PermissionMode,
        persist: Option<bool>,
    ) -> Result<Self, DesktopRpcError> {
        Self::new(
            request_id,
            DesktopMethod::SetPermissionMode,
            set_permission_mode_params(mode, persist)?,
        )
    }

    /// The `list_subagents` request. It takes no params.
    pub fn list_subagents(request_id: impl Into<String>) -> Result<Self, DesktopRpcError> {
        Self::new(request_id, DesktopMethod::ListSubagents, json!({}))
    }

    /// The `list_jobs` request. It takes no params.
    pub fn list_jobs(request_id: impl Into<String>) -> Result<Self, DesktopRpcError> {
        Self::new(request_id, DesktopMethod::ListJobs, json!({}))
    }

    /// The `cancel_subagent` request. Params are exactly `{ id }` with a
    /// non-empty id, as reported by `list_subagents`.
    pub fn cancel_subagent(
        request_id: impl Into<String>,
        id: impl Into<String>,
    ) -> Result<Self, DesktopRpcError> {
        Self::new(
            request_id,
            DesktopMethod::CancelSubagent,
            cancel_params(id.into())?,
        )
    }

    /// The `cancel_job` request. Params are exactly `{ id }` with a
    /// non-empty id, as reported by `list_jobs`.
    pub fn cancel_job(
        request_id: impl Into<String>,
        id: impl Into<String>,
    ) -> Result<Self, DesktopRpcError> {
        Self::new(
            request_id,
            DesktopMethod::CancelJob,
            cancel_params(id.into())?,
        )
    }

    /// The `get_subscription_usage` request. `provider` is `None` for the
    /// whole-account snapshot, or `Some` for a single provider.
    pub fn get_subscription_usage(
        request_id: impl Into<String>,
        provider: Option<&str>,
    ) -> Result<Self, DesktopRpcError> {
        Self::new(
            request_id,
            DesktopMethod::GetSubscriptionUsage,
            subscription_usage_params(provider)?,
        )
    }

    /// The `refresh_subscription_usage` request, with the same optional
    /// provider param as [`Self::get_subscription_usage`].
    pub fn refresh_subscription_usage(
        request_id: impl Into<String>,
        provider: Option<&str>,
    ) -> Result<Self, DesktopRpcError> {
        Self::new(
            request_id,
            DesktopMethod::RefreshSubscriptionUsage,
            subscription_usage_params(provider)?,
        )
    }

    /// The `get_design_canvas` request. `include_url` is `None` to omit the
    /// param so the server's documented default (no URL) applies, or `Some` to
    /// send an explicit `includeUrl` flag (`true` or `false`).
    pub fn get_design_canvas(
        request_id: impl Into<String>,
        include_url: Option<bool>,
    ) -> Result<Self, DesktopRpcError> {
        Self::new(
            request_id,
            DesktopMethod::GetDesignCanvas,
            get_design_canvas_params(include_url),
        )
    }

    /// The `get_config` request. Params are exactly `{ store }`: a store
    /// identifier, never a path.
    pub fn get_config(
        request_id: impl Into<String>,
        store: ConfigStore,
    ) -> Result<Self, DesktopRpcError> {
        Self::build(
            request_id,
            DesktopMethod::GetConfig,
            get_config_params(store),
        )
    }

    /// The `set_config` request. Params are exactly `{ store, data,
    /// expectedRevision }`; `expected_revision` is the `writeRevision` of the
    /// target `write` replaces, as last read. Only the writable stores can
    /// be named, through [`ConfigWrite`]. Before sending, an empty
    /// `expected_revision` ([`DesktopRpcError::InvalidExpectedRevision`]) and
    /// an empty pin profile ([`DesktopRpcError::InvalidPinProfile`]) are
    /// refused; everything else in `data` is the server's to validate.
    pub fn set_config(
        request_id: impl Into<String>,
        write: ConfigWrite,
        expected_revision: impl Into<String>,
    ) -> Result<Self, DesktopRpcError> {
        Self::build(
            request_id,
            DesktopMethod::SetConfig,
            set_config_params(&write, &expected_revision.into())?,
        )
    }

    /// The `apply_profile` request. Params are exactly `{ profile, mode }`,
    /// plus `confirmation` when resending with the token of a
    /// `confirmation_required` answer the human approved. A profile name or
    /// token outside 1–128 characters is refused before sending.
    pub fn apply_profile(
        request_id: impl Into<String>,
        profile: &str,
        mode: ProfileApplyMode,
        confirmation: Option<&str>,
    ) -> Result<Self, DesktopRpcError> {
        Self::build(
            request_id,
            DesktopMethod::ApplyProfile,
            apply_profile_params(profile, mode, confirmation)?,
        )
    }

    /// The `get_snapshot` request with session sections: the legacy answer
    /// plus `state.session` with the requested sections. Gentle Shell 4.8.0
    /// ignores the param and answers without them, so send it only when
    /// [`Capabilities::supports_session_sections`] says so. An empty or
    /// repeated list is refused before sending.
    pub fn get_snapshot_sections(
        request_id: impl Into<String>,
        sections: &[SessionSection],
    ) -> Result<Self, DesktopRpcError> {
        Self::build(
            request_id,
            DesktopMethod::GetSnapshot,
            snapshot_sections_params(sections)?,
        )
    }

    /// The `get_job_output` request. Params are exactly `{ id }` with a
    /// non-empty id, as reported by `list_jobs` or `jobs_update`.
    pub fn get_job_output(
        request_id: impl Into<String>,
        id: &str,
    ) -> Result<Self, DesktopRpcError> {
        Self::build(
            request_id,
            DesktopMethod::GetJobOutput,
            job_output_params(id)?,
        )
    }

    /// The `get_subagent_thread` request. Params are `{ id }` with a
    /// non-empty id, plus `sinceVersion` (the `version` of the last read) to
    /// get no items when nothing changed.
    pub fn get_subagent_thread(
        request_id: impl Into<String>,
        id: &str,
        since_version: Option<u64>,
    ) -> Result<Self, DesktopRpcError> {
        Self::build(
            request_id,
            DesktopMethod::GetSubagentThread,
            output::subagent_thread_params(id, since_version)?,
        )
    }

    /// The `list_sessions` request with the given optional params. A limit
    /// outside 1–50 or an empty cursor is refused before sending.
    pub fn list_sessions(
        request_id: impl Into<String>,
        query: &ListSessionsQuery,
    ) -> Result<Self, DesktopRpcError> {
        Self::build(request_id, DesktopMethod::ListSessions, query.params()?)
    }

    /// The `switch_session` request: `{ id }` (1–256 characters) and, when
    /// given, the `scope` the id was listed with.
    pub fn switch_session(
        request_id: impl Into<String>,
        id: &str,
        scope: Option<SessionsScope>,
    ) -> Result<Self, DesktopRpcError> {
        Self::build(
            request_id,
            DesktopMethod::SwitchSession,
            sessions::switch_session_params(id, scope)?,
        )
    }

    /// The `get_changes` request. It takes no params.
    pub fn get_changes(request_id: impl Into<String>) -> Result<Self, DesktopRpcError> {
        Self::build(request_id, DesktopMethod::GetChanges, json!({}))
    }

    /// The `get_change_diff` request: exactly `{ root, path }`, as
    /// `get_changes` listed them.
    pub fn get_change_diff(
        request_id: impl Into<String>,
        root: &str,
        path: &str,
    ) -> Result<Self, DesktopRpcError> {
        Self::build(
            request_id,
            DesktopMethod::GetChangeDiff,
            change_diff_params(root, path),
        )
    }

    /// The `get_review_mode` request. It takes no params.
    pub fn get_review_mode(request_id: impl Into<String>) -> Result<Self, DesktopRpcError> {
        Self::build(request_id, DesktopMethod::GetReviewMode, json!({}))
    }

    /// The `set_review_mode` request: `{ enabled, scope }`, plus
    /// `confirmationToken` when resending the token of a
    /// `confirmation_required` answer the human approved. A token outside
    /// 1–128 characters is refused before sending.
    pub fn set_review_mode(
        request_id: impl Into<String>,
        enabled: bool,
        scope: ReviewScope,
        confirmation_token: Option<&str>,
    ) -> Result<Self, DesktopRpcError> {
        Self::build(
            request_id,
            DesktopMethod::SetReviewMode,
            review::set_review_mode_params(enabled, scope, confirmation_token)?,
        )
    }

    /// The `get_usage_stats` request; a `None` param is omitted so the
    /// server default (`7d`, `project`) applies.
    pub fn get_usage_stats(
        request_id: impl Into<String>,
        range: Option<StatsRange>,
        scope: Option<StatsScope>,
    ) -> Result<Self, DesktopRpcError> {
        Self::build(
            request_id,
            DesktopMethod::GetUsageStats,
            stats::usage_stats_params(range, scope),
        )
    }

    /// The `get_update_status` request: `None` omits `refresh` (a read of
    /// what is already known, no network), `Some` sends it explicitly.
    pub fn get_update_status(
        request_id: impl Into<String>,
        refresh: Option<bool>,
    ) -> Result<Self, DesktopRpcError> {
        Self::build(
            request_id,
            DesktopMethod::GetUpdateStatus,
            update_status_params(refresh),
        )
    }

    /// The `set_background_subagents` request: `{ policy }`, plus
    /// `confirmationToken` when resending the token of a
    /// `confirmation_required` answer the human approved. A token outside
    /// 1–128 characters is refused before sending.
    pub fn set_background_subagents(
        request_id: impl Into<String>,
        policy: BackgroundPolicy,
        confirmation_token: Option<&str>,
    ) -> Result<Self, DesktopRpcError> {
        Self::build(
            request_id,
            DesktopMethod::SetBackgroundSubagents,
            background::set_background_subagents_params(policy, confirmation_token)?,
        )
    }

    /// A request for a known method, validated against the id and size limits
    /// before anything reaches the transport. The config methods are refused
    /// here: their params come only from [`Self::get_config`] and
    /// [`Self::set_config`], as `apply_profile`'s come only from
    /// [`Self::apply_profile`].
    pub fn new(
        request_id: impl Into<String>,
        method: DesktopMethod,
        params: Value,
    ) -> Result<Self, DesktopRpcError> {
        if method.requires_typed_params() {
            return Err(DesktopRpcError::TypedParamsRequired(method));
        }
        Self::build(request_id, method, params)
    }

    fn build(
        request_id: impl Into<String>,
        method: DesktopMethod,
        params: Value,
    ) -> Result<Self, DesktopRpcError> {
        let request_id = request_id.into();
        let units = request_id.encode_utf16().count();
        if units == 0 || units > MAX_REQUEST_ID_UNITS {
            return Err(DesktopRpcError::InvalidRequestId { units });
        }
        if !params.is_object() {
            return Err(DesktopRpcError::ParamsNotObject);
        }
        let payload = serde_json::to_string(&WireRequest {
            schema: SCHEMA,
            request_id: &request_id,
            method: method.as_str(),
            params: &params,
        })
        .map_err(|error| DesktopRpcError::MalformedPayload(error.to_string()))?;
        if payload.len() > MAX_REQUEST_BYTES {
            return Err(DesktopRpcError::RequestTooLarge {
                bytes: payload.len(),
                limit: MAX_REQUEST_BYTES,
            });
        }
        Ok(Self {
            request_id,
            method,
            payload,
        })
    }

    /// The inner `requestId`, the only correlation key of this protocol.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// The method being called.
    pub fn method(&self) -> DesktopMethod {
        self.method
    }

    /// The inner request as the JSON document sent inside the prompt.
    pub fn payload(&self) -> &str {
        &self.payload
    }

    /// The exact prompt message: `/gentle:desktop <json>`.
    pub fn prompt_message(&self) -> String {
        format!("{PROMPT_COMMAND} {}", self.payload)
    }

    /// The `prompt` command to hand to the JSONL transport. It carries no
    /// `id`: the transport assigns the Pi request id, which is not the
    /// Desktop `requestId`.
    pub fn prompt_command(&self) -> Value {
        json!({ "type": "prompt", "message": self.prompt_message() })
    }
}

/// The server-reported failure of an inner request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopErrorInfo {
    pub code: String,
    pub message: String,
}

impl DesktopErrorInfo {
    /// The documented config failure this error reports, if it is one.
    pub fn config_error(&self) -> Option<ConfigErrorCode> {
        ConfigErrorCode::parse(&self.code)
    }

    /// The documented `apply_profile` failure this error reports, if it is
    /// one.
    pub fn profile_apply_error(&self) -> Option<ProfileApplyErrorCode> {
        ProfileApplyErrorCode::parse(&self.code)
    }

    /// The documented failure this error reports, if the code is one this
    /// client knows.
    pub fn kind(&self) -> Option<DesktopErrorCode> {
        self.code.parse().ok()
    }
}

wire_enum! {
    /// Every documented `error.code`. The config and `apply_profile` subsets
    /// are also [`ConfigErrorCode`] and [`ProfileApplyErrorCode`].
    /// `changes_unavailable`, `session_unavailable`, `updates_unavailable`,
    /// `agent_definitions_unavailable` and `status_unavailable` are answered
    /// by Gentle Shell but missing from its specification's list of codes;
    /// they are kept here because the server sends them.
    DesktopErrorCode, "error.code" {
        InvalidRequest => "invalid_request",
        UnknownStore => "unknown_store",
        ReadOnly => "read_only",
        InvalidData => "invalid_data",
        RevisionConflict => "revision_conflict",
        WriteFailed => "write_failed",
        UnsupportedMethod => "unsupported_method",
        PermissionUnavailable => "permission_unavailable",
        PermissionDenied => "permission_denied",
        ConfigUnavailable => "config_unavailable",
        UsageUnavailable => "usage_unavailable",
        DesignUnavailable => "design_unavailable",
        JobsUnavailable => "jobs_unavailable",
        AgentsUnavailable => "agents_unavailable",
        ChangesUnavailable => "changes_unavailable",
        ProfilesUnavailable => "profiles_unavailable",
        UnknownProfile => "unknown_profile",
        ProfileApplyBusy => "profile_apply_busy",
        SessionsUnavailable => "sessions_unavailable",
        SessionSwitching => "session_switching",
        SessionUnavailable => "session_unavailable",
        ReviewUnavailable => "review_unavailable",
        ReviewModeBusy => "review_mode_busy",
        StatsUnavailable => "stats_unavailable",
        UpdatesUnavailable => "updates_unavailable",
        AgentDefinitionsUnavailable => "agent_definitions_unavailable",
        StatusUnavailable => "status_unavailable",
        BackgroundSubagentsBusy => "background_subagents_busy",
        BackgroundSubagentsUnavailable => "background_subagents_unavailable",
        AuthUnavailable => "auth_unavailable",
        ReloadFailed => "reload_failed",
        NotFound => "not_found",
        InternalError => "internal_error",
    }
}

impl DesktopErrorCode {
    /// Whether the same request may succeed when sent again unchanged:
    /// `session_switching` answers every request while a `switch_session` is
    /// in flight, and the busy codes a request racing a single-flight write.
    /// Nothing was done in either case.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::SessionSwitching
                | Self::ReviewModeBusy
                | Self::ProfileApplyBusy
                | Self::BackgroundSubagentsBusy
        )
    }
}

/// A decoded inner response. `requestId` is kept so the caller can prove the
/// correlation instead of trusting arrival order; it is absent only for the
/// server's `requestId: null` report of a request it could not parse.
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopResponse {
    request_id: Option<String>,
    result: Result<Value, DesktopErrorInfo>,
}

impl DesktopResponse {
    /// The inner `requestId` this response answers, when the server had one.
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    /// Whether the server answered with data instead of an error.
    pub fn is_ok(&self) -> bool {
        self.result.is_ok()
    }

    /// The error the server reported, if it failed.
    pub fn error(&self) -> Option<&DesktopErrorInfo> {
        self.result.as_ref().err()
    }

    /// The response to `capabilities`.
    pub fn capabilities(&self) -> Result<Capabilities, DesktopRpcError> {
        Capabilities::from_data(self.data()?)
    }

    /// The response to `get_snapshot`.
    pub fn snapshot(&self) -> Result<Snapshot, DesktopRpcError> {
        Snapshot::from_data(self.data()?)
    }

    /// The response to `get_permission_mode`.
    pub fn permission_mode(&self) -> Result<PermissionModeState, DesktopRpcError> {
        PermissionModeState::from_data(self.data()?)
    }

    /// The response to `set_permission_mode`.
    pub fn set_permission_mode(&self) -> Result<SetPermissionModeResult, DesktopRpcError> {
        SetPermissionModeResult::from_data(self.data()?)
    }

    /// The response to `list_subagents`.
    pub fn list_subagents(&self) -> Result<SubagentList, DesktopRpcError> {
        SubagentList::from_data(self.data()?)
    }

    /// The response to `cancel_subagent`.
    pub fn cancel_subagent(&self) -> Result<CancelSubagentResult, DesktopRpcError> {
        CancelSubagentResult::from_data(self.data()?)
    }

    /// The response to `list_jobs`.
    pub fn list_jobs(&self) -> Result<JobList, DesktopRpcError> {
        JobList::from_data(self.data()?)
    }

    /// The response to `cancel_job`.
    pub fn cancel_job(&self) -> Result<CancelJobResult, DesktopRpcError> {
        CancelJobResult::from_data(self.data()?)
    }

    /// The response to `get_subscription_usage` and
    /// `refresh_subscription_usage`, which share the same data shape.
    pub fn subscription_usage(&self) -> Result<SubscriptionUsage, DesktopRpcError> {
        SubscriptionUsage::from_data(self.data()?)
    }

    /// The response to `get_design_canvas`.
    pub fn design_canvas(&self) -> Result<DesignCanvas, DesktopRpcError> {
        DesignCanvas::from_data(self.data()?)
    }

    /// The response to `get_config`.
    pub fn config(&self) -> Result<ConfigState, DesktopRpcError> {
        ConfigState::from_data(self.data()?)
    }

    /// The response to `set_config`.
    pub fn set_config(&self) -> Result<SetConfigResult, DesktopRpcError> {
        SetConfigResult::from_data(self.data()?)
    }

    /// The response to `apply_profile`.
    pub fn apply_profile(&self) -> Result<ApplyProfileResult, DesktopRpcError> {
        ApplyProfileResult::from_data(self.data()?)
    }

    /// The response to `get_job_output`.
    pub fn job_output(&self) -> Result<JobOutput, DesktopRpcError> {
        JobOutput::from_data(self.data()?)
    }

    /// The response to `get_subagent_thread`.
    pub fn subagent_thread(&self) -> Result<SubagentThread, DesktopRpcError> {
        SubagentThread::from_data(self.data()?)
    }

    /// The response to `list_sessions`.
    pub fn list_sessions(&self) -> Result<SessionList, DesktopRpcError> {
        SessionList::from_data(self.data()?)
    }

    /// The response to `switch_session`.
    pub fn switch_session(&self) -> Result<SwitchSessionResult, DesktopRpcError> {
        SwitchSessionResult::from_data(self.data()?)
    }

    /// The response to `get_changes`.
    pub fn changes(&self) -> Result<Changes, DesktopRpcError> {
        Changes::from_data(self.data()?)
    }

    /// The response to `get_change_diff`.
    pub fn change_diff(&self) -> Result<ChangeDiff, DesktopRpcError> {
        ChangeDiff::from_data(self.data()?)
    }

    /// The response to `get_review_mode`.
    pub fn review_mode(&self) -> Result<ReviewMode, DesktopRpcError> {
        ReviewMode::from_data(self.data()?)
    }

    /// The response to `set_review_mode`.
    pub fn set_review_mode(&self) -> Result<SetReviewModeResult, DesktopRpcError> {
        SetReviewModeResult::from_data(self.data()?)
    }

    /// The response to `get_usage_stats`.
    pub fn usage_stats(&self) -> Result<UsageStats, DesktopRpcError> {
        UsageStats::from_data(self.data()?)
    }

    /// The response to `get_update_status`.
    pub fn update_status(&self) -> Result<UpdateStatus, DesktopRpcError> {
        UpdateStatus::from_data(self.data()?)
    }

    /// The response to `list_agent_definitions`.
    pub fn agent_definitions(&self) -> Result<AgentDefinitions, DesktopRpcError> {
        AgentDefinitions::from_data(self.data()?)
    }

    /// The response to `get_status`.
    pub fn status(&self) -> Result<ShellStatus, DesktopRpcError> {
        ShellStatus::from_data(self.data()?)
    }

    /// The response to `get_background_subagents`.
    pub fn background_subagents(&self) -> Result<BackgroundSubagents, DesktopRpcError> {
        BackgroundSubagents::from_data(self.data()?)
    }

    /// The response to `set_background_subagents`.
    pub fn set_background_subagents(
        &self,
    ) -> Result<SetBackgroundSubagentsResult, DesktopRpcError> {
        SetBackgroundSubagentsResult::from_data(self.data()?)
    }

    /// The response to `get_profile_binding`.
    pub fn profile_binding(&self) -> Result<ProfileBinding, DesktopRpcError> {
        ProfileBinding::from_data(self.data()?)
    }

    /// The response to `clear_profile_binding`.
    pub fn clear_profile_binding(&self) -> Result<ClearProfileBindingResult, DesktopRpcError> {
        ClearProfileBindingResult::from_data(self.data()?)
    }

    /// The response to `get_auth_status`.
    pub fn auth_status(&self) -> Result<AuthStatus, DesktopRpcError> {
        AuthStatus::from_data(self.data()?)
    }

    /// The response to `reload_credentials`.
    pub fn reload_credentials(&self) -> Result<ReloadCredentials, DesktopRpcError> {
        ReloadCredentials::from_data(self.data()?)
    }

    fn data(&self) -> Result<&Value, DesktopRpcError> {
        self.result
            .as_ref()
            .map_err(|info| DesktopRpcError::RemoteError(info.clone()))
    }
}

/// One inner event. `seq` is session-scoped and advances by one.
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopEvent {
    pub seq: u64,
    pub event: String,
    pub data: Value,
}

impl DesktopEvent {
    /// Decode the data of a `permission_mode_changed` event. Like the other
    /// typed accessors it decodes the data as-is; an event of another name
    /// simply fails to fit the shape.
    pub fn permission_mode_changed(&self) -> Result<PermissionModeChanged, DesktopRpcError> {
        PermissionModeChanged::from_data(&self.data)
    }

    /// Decode the data of an `agents_update` event. Unlike the shape-only
    /// accessor above, this verifies the exact event name first, so a foreign
    /// payload that happens to fit the shape is refused instead of misread.
    pub fn agents_update(&self) -> Result<SubagentList, DesktopRpcError> {
        expect_event(self, "agents_update")?;
        SubagentList::from_data(&self.data)
    }

    /// Decode the data of a `jobs_update` event, verifying the exact event
    /// name before the shape, exactly like [`Self::agents_update`].
    pub fn jobs_update(&self) -> Result<JobList, DesktopRpcError> {
        expect_event(self, "jobs_update")?;
        JobList::from_data(&self.data)
    }

    /// Decode the data of a `rate_limit` event, verifying the exact event
    /// name before the shape, exactly like [`Self::agents_update`].
    pub fn rate_limit(&self) -> Result<RateLimitEvent, DesktopRpcError> {
        expect_event(self, "rate_limit")?;
        RateLimitEvent::from_data(&self.data)
    }

    /// Decode the data of a `design_canvas` event, verifying the exact event
    /// name before the shape, exactly like [`Self::agents_update`].
    pub fn design_canvas(&self) -> Result<DesignCanvasEvent, DesktopRpcError> {
        expect_event(self, "design_canvas")?;
        DesignCanvasEvent::from_data(&self.data)
    }

    /// Decode the data of a `config_changed` event, verifying the exact event
    /// name before the shape, exactly like [`Self::agents_update`].
    pub fn config_changed(&self) -> Result<ConfigChanged, DesktopRpcError> {
        expect_event(self, "config_changed")?;
        ConfigChanged::from_data(&self.data)
    }

    /// Decode the data of a `profile_applied` event, verifying the exact
    /// event name before the shape, exactly like [`Self::agents_update`].
    pub fn profile_applied(&self) -> Result<ProfileApplied, DesktopRpcError> {
        expect_event(self, "profile_applied")?;
        ProfileApplied::from_data(&self.data)
    }

    /// Decode the data of a `session_snapshot` event, verifying the exact
    /// event name before the shape, exactly like [`Self::agents_update`].
    pub fn session_snapshot(&self) -> Result<SessionSnapshotEvent, DesktopRpcError> {
        expect_event(self, "session_snapshot")?;
        SessionSnapshotEvent::from_data(&self.data)
    }

    /// Decode the data of a `review_mode_changed` event, verifying the exact
    /// event name before the shape, exactly like [`Self::agents_update`].
    pub fn review_mode_changed(&self) -> Result<ReviewModeChanged, DesktopRpcError> {
        expect_event(self, "review_mode_changed")?;
        ReviewModeChanged::from_data(&self.data)
    }

    /// Decode the data of a `profile_binding_changed` event, verifying the
    /// exact event name before the shape, exactly like
    /// [`Self::agents_update`].
    pub fn profile_binding_changed(&self) -> Result<ProfileBindingChanged, DesktopRpcError> {
        expect_event(self, "profile_binding_changed")?;
        ProfileBindingChanged::from_data(&self.data)
    }
}

/// A record decoded from the Desktop widget.
#[derive(Debug, Clone, PartialEq)]
pub enum DesktopRecord {
    Response(DesktopResponse),
    Event(DesktopEvent),
}

/// The decoded `capabilities` data: what the extension offers and how it
/// describes itself on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Inner methods the extension implements.
    pub methods: Vec<String>,
    /// Events it publishes.
    pub events: Vec<String>,
    /// The schema it declares, when it declares one.
    pub schema: Option<String>,
    /// The slash command it declares, when it declares one.
    pub command: Option<String>,
    /// The widget key it declares, when it declares one.
    pub widget: Option<String>,
}

impl Capabilities {
    /// Whether the peer offers `method`.
    pub fn supports(&self, method: DesktopMethod) -> bool {
        self.methods.iter().any(|name| name == method.as_str())
    }

    /// Whether the peer publishes the `event` name.
    pub fn emits(&self, event: &str) -> bool {
        self.events.iter().any(|name| name == event)
    }

    /// Refuse a method the peer does not offer, before anything is sent: the
    /// gate for every method newer than a peer (Gentle Shell 4.8.0 offers
    /// none of `get_job_output`, `get_subagent_thread`, `list_sessions`,
    /// `switch_session`, `get_changes`, `get_change_diff`, `get_review_mode`,
    /// `set_review_mode`, `get_usage_stats` or `get_update_status`, and 4.9.0
    /// none of `list_agent_definitions`, `get_profile_binding` or
    /// `clear_profile_binding`).
    pub fn require(&self, method: DesktopMethod) -> Result<(), DesktopRpcError> {
        if self.supports(method) {
            Ok(())
        } else {
            Err(DesktopRpcError::NotAdvertised(method))
        }
    }

    /// Whether `get_snapshot` accepts `sections`. The method itself is old,
    /// so its name cannot tell: the server advertises `session_snapshot`
    /// exactly while the feed owner the sections are read from is wired, and
    /// Gentle Shell 4.8.0 ignores the param instead of refusing it.
    pub fn supports_session_sections(&self) -> bool {
        self.supports(DesktopMethod::GetSnapshot) && self.emits("session_snapshot")
    }

    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let schema = text_field(data, "schema")?;
        expect_field("schema", SCHEMA, &schema)?;
        let command = text_field(data, "command")?;
        let command_name = command
            .as_ref()
            .map(|value| value.strip_prefix('/').unwrap_or(value).to_string());
        expect_field(
            "command",
            PROMPT_COMMAND.trim_start_matches('/'),
            &command_name,
        )?;
        let widget = text_field(data, "widget")?;
        expect_field("widget", WIDGET_KEY, &widget)?;
        Ok(Self {
            methods: name_list(data, "methods")?,
            events: name_list(data, "events")?,
            schema,
            command,
            widget,
        })
    }
}

/// An optional string field of a data document.
fn text_field(data: &Value, field: &'static str) -> Result<Option<String>, DesktopRpcError> {
    match data.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must be a string"
        ))),
    }
}

/// Decode a data document into its typed shape; any mismatch is a malformed
/// payload.
fn decode_data<T: serde::de::DeserializeOwned>(data: &Value) -> Result<T, DesktopRpcError> {
    T::deserialize(data).map_err(|error| DesktopRpcError::MalformedPayload(error.to_string()))
}

/// Refuse a declared wire value the client does not speak.
fn expect_field(
    field: &'static str,
    expected: &'static str,
    got: &Option<String>,
) -> Result<(), DesktopRpcError> {
    match got {
        Some(got) if got != expected => Err(DesktopRpcError::UnexpectedValue {
            field,
            expected,
            got: got.clone(),
        }),
        _ => Ok(()),
    }
}

/// A required array of names from a data document.
fn name_list(data: &Value, field: &'static str) -> Result<Vec<String>, DesktopRpcError> {
    let items = data.get(field).and_then(Value::as_array).ok_or_else(|| {
        DesktopRpcError::MalformedPayload(format!("capabilities response has no {field} array"))
    })?;
    let mut names = Vec::with_capacity(items.len());
    for item in items {
        let name = item.as_str().ok_or_else(|| {
            DesktopRpcError::MalformedPayload(format!("capabilities {field} must be strings"))
        })?;
        names.push(name.to_string());
    }
    Ok(names)
}

/// A required permission mode field of a data document.
fn permission_mode_field(
    data: &Value,
    field: &'static str,
) -> Result<PermissionMode, DesktopRpcError> {
    data.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| DesktopRpcError::MalformedPayload(format!("{field} must be a mode string")))?
        .parse()
}

/// A required boolean field of a data document.
fn bool_field(data: &Value, field: &'static str) -> Result<bool, DesktopRpcError> {
    data.get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| DesktopRpcError::MalformedPayload(format!("{field} must be a boolean")))
}

/// A required string field of a data document.
fn string_field(data: &Value, field: &'static str) -> Result<String, DesktopRpcError> {
    data.get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| DesktopRpcError::MalformedPayload(format!("{field} must be a string")))
}

/// An optional `{ code, message }` error object of a data document, such as
/// the `persist_error` of a `set_permission_mode` response.
fn optional_error_field(
    data: &Value,
    field: &'static str,
) -> Result<Option<DesktopErrorInfo>, DesktopRpcError> {
    match data.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(object)) => {
            let code = object
                .get("code")
                .and_then(Value::as_str)
                .filter(|code| !code.is_empty())
                .ok_or_else(|| DesktopRpcError::MalformedPayload(format!("{field} has no code")))?;
            let message = object
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(Some(DesktopErrorInfo {
                code: code.to_string(),
                message: message.to_string(),
            }))
        }
        Some(_) => Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must be an error object"
        ))),
    }
}

/// A required non-negative integer field of a data document (token counts,
/// millisecond timestamps).
fn u64_field(data: &Value, field: &'static str) -> Result<u64, DesktopRpcError> {
    data.get(field).and_then(Value::as_u64).ok_or_else(|| {
        DesktopRpcError::MalformedPayload(format!("{field} must be a non-negative integer"))
    })
}

/// An optional non-negative integer field: absent and `null` both decode to
/// `None`, matching the other optional field helpers.
fn optional_u64_field(data: &Value, field: &'static str) -> Result<Option<u64>, DesktopRpcError> {
    match data.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number.as_u64().map(Some).ok_or_else(|| {
            DesktopRpcError::MalformedPayload(format!("{field} must be a non-negative integer"))
        }),
        Some(_) => Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must be a non-negative integer"
        ))),
    }
}

/// A required cost field: a finite, non-negative number (it may be fractional).
fn cost_field(data: &Value, field: &'static str) -> Result<f64, DesktopRpcError> {
    let value = data
        .get(field)
        .and_then(Value::as_f64)
        .ok_or_else(|| DesktopRpcError::MalformedPayload(format!("{field} must be a number")))?;
    if !value.is_finite() || value < 0.0 {
        return Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must be a finite non-negative number"
        )));
    }
    Ok(value)
}

/// An optional boolean field: absent and `null` both decode to `None`.
fn optional_bool_field(data: &Value, field: &'static str) -> Result<Option<bool>, DesktopRpcError> {
    match data.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must be a boolean"
        ))),
    }
}

/// A job exit code field. Absent is `None` (no code recorded); a recorded
/// `null` (an exit without a code) is `Some(JobExitCode::Null)`, so the two
/// are never conflated.
fn exit_code_field(
    data: &Value,
    field: &'static str,
) -> Result<Option<JobExitCode>, DesktopRpcError> {
    match data.get(field) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(JobExitCode::Null)),
        Some(Value::Number(number)) => {
            let code = number
                .as_i64()
                .filter(|code| *code >= i32::MIN as i64 && *code <= i32::MAX as i64)
                .ok_or_else(|| {
                    DesktopRpcError::MalformedPayload(format!(
                        "{field} must be an integer exit code"
                    ))
                })?;
            Ok(Some(JobExitCode::Code(code as i32)))
        }
        Some(_) => Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must be an integer or null"
        ))),
    }
}

/// A required `lastStep` field, restricted to the closed [`DesktopAgentStep`]
/// vocabulary.
fn last_step_field(data: &Value, field: &'static str) -> Result<DesktopAgentStep, DesktopRpcError> {
    data.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| DesktopRpcError::MalformedPayload(format!("{field} must be a step string")))?
        .parse()
}

/// A required nested object field of a data document.
fn field_object<'a>(data: &'a Value, field: &'static str) -> Result<&'a Value, DesktopRpcError> {
    data.get(field)
        .filter(|value| value.is_object())
        .ok_or_else(|| DesktopRpcError::MalformedPayload(format!("{field} must be an object")))
}

/// A required array of projected subagent entries.
fn agent_entries(data: &Value) -> Result<Vec<DesktopAgent>, DesktopRpcError> {
    let items = data
        .get("agents")
        .and_then(Value::as_array)
        .ok_or_else(|| DesktopRpcError::MalformedPayload("list has no agents array".into()))?;
    items.iter().map(DesktopAgent::from_data).collect()
}

/// A required array of projected job entries.
fn job_entries(data: &Value) -> Result<Vec<DesktopJob>, DesktopRpcError> {
    let items = data
        .get("jobs")
        .and_then(Value::as_array)
        .ok_or_else(|| DesktopRpcError::MalformedPayload("list has no jobs array".into()))?;
    items.iter().map(DesktopJob::from_data).collect()
}

/// Require an event to carry the exact name before its data is decoded, so a
/// foreign payload that happens to fit the shape is refused instead of read.
fn expect_event(event: &DesktopEvent, expected: &'static str) -> Result<(), DesktopRpcError> {
    if event.event == expected {
        Ok(())
    } else {
        Err(DesktopRpcError::UnexpectedEvent {
            expected,
            got: event.event.clone(),
        })
    }
}

/// The decoded `get_snapshot` data. `session` is the decoded
/// `state.session`, present only when the request asked for `sections` and
/// the peer supports them; `state` keeps the whole projection as sent.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub sequence: u64,
    pub state: Value,
    pub session: Option<SessionSections>,
}

impl Snapshot {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let schema = text_field(data, "schema")?;
        expect_field("schema", SCHEMA, &schema)?;
        let sequence = data
            .get("sequence")
            .and_then(Value::as_u64)
            .ok_or_else(|| DesktopRpcError::MalformedPayload("snapshot has no sequence".into()))?;
        let state = data
            .get("state")
            .filter(|state| state.is_object())
            .cloned()
            .ok_or_else(|| {
                DesktopRpcError::MalformedPayload("snapshot has no state object".into())
            })?;
        let session = state
            .get("session")
            .map(SessionSections::from_data)
            .transpose()?;
        Ok(Self {
            sequence,
            state,
            session,
        })
    }
}

wire_enum! {
    /// Who made a change a `permission_mode_changed` or `config_changed`
    /// event reports: a Desktop request on this channel, or anything else
    /// (the TUI, an editor, the installer, another process). Gentle Shell
    /// 4.8.0 only sends `desktop`.
    ChangeSource, "source" {
        Desktop => "desktop",
        External => "external",
    }
}

/// The live permission state read by `get_permission_mode`: the session mode
/// in effect now and the trusted default a new primary session would start
/// from. Both are the server's actual state, never an echo of a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionModeState {
    pub mode: PermissionMode,
    pub default_mode: PermissionMode,
}

impl PermissionModeState {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            mode: permission_mode_field(data, "mode")?,
            default_mode: permission_mode_field(data, "default_mode")?,
        })
    }
}

/// The actual state after `set_permission_mode`. `mode` and `default_mode`
/// are re-read from the owner; `persisted` reports whether the request saved
/// the default; `changed` reports whether either value moved. `persist_error`
/// is present only when a requested save failed, and the live change stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetPermissionModeResult {
    pub mode: PermissionMode,
    pub default_mode: PermissionMode,
    pub persisted: bool,
    pub changed: bool,
    pub persist_error: Option<DesktopErrorInfo>,
}

impl SetPermissionModeResult {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            mode: permission_mode_field(data, "mode")?,
            default_mode: permission_mode_field(data, "default_mode")?,
            persisted: bool_field(data, "persisted")?,
            changed: bool_field(data, "changed")?,
            persist_error: optional_error_field(data, "persist_error")?,
        })
    }
}

/// The data of a `permission_mode_changed` event. A `desktop` change follows
/// the successful `set_permission_mode` response it reports, and `persisted`
/// says whether that request saved the default. An `external` change
/// (`/gentle:mode`, `/gentle:yolo`, the owner revoking YOLO) arrives on its
/// own, debounced; `previous_mode` is the last state this channel reported
/// and `persisted` says the default is what changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionModeChanged {
    pub mode: PermissionMode,
    pub previous_mode: PermissionMode,
    pub default_mode: PermissionMode,
    pub persisted: bool,
    pub source: ChangeSource,
}

impl PermissionModeChanged {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            mode: permission_mode_field(data, "mode")?,
            previous_mode: permission_mode_field(data, "previous_mode")?,
            default_mode: permission_mode_field(data, "default_mode")?,
            persisted: bool_field(data, "persisted")?,
            source: closed_field(data, "source")?,
        })
    }
}

/// A subagent's accumulated usage: the billed total and cost the store tracks,
/// plus the final-assistant input/output/cache/reasoning token breakdown once
/// the run settled. The breakdown fields are absent for an in-flight task
/// whose breakdown is not recorded yet, and for a restored task whose run
/// never recorded one.
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopAgentUsage {
    pub total: u64,
    pub cost: f64,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache: Option<u64>,
    pub reasoning: Option<u64>,
}

impl DesktopAgentUsage {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            total: u64_field(data, "total")?,
            cost: cost_field(data, "cost")?,
            input: optional_u64_field(data, "input")?,
            output: optional_u64_field(data, "output")?,
            cache: optional_u64_field(data, "cache")?,
            reasoning: optional_u64_field(data, "reasoning")?,
        })
    }
}

/// One projected subagent entry. Only these fields exist on the wire: identity,
/// the agent name, the status word, the routed model plus its launch-layer
/// source and the fallback it moved away from, the thinking level, the closed
/// step label, the in-run progress (turns finished, tool calls started, the
/// tool running now) and the bounded usage window. The progress fields are
/// `None` from an owner that predates them (Gentle Shell before 5.0.0);
/// `active_tool` is also `None` while no tool runs or the task is not active.
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopAgent {
    pub id: String,
    pub agent: String,
    pub status: String,
    pub model: Option<String>,
    pub model_source: Option<String>,
    pub fallback_from: Option<String>,
    pub thinking: Option<String>,
    pub last_step: DesktopAgentStep,
    pub turns: Option<u64>,
    pub tool_calls: Option<u64>,
    pub active_tool: Option<String>,
    pub usage: DesktopAgentUsage,
}

impl DesktopAgent {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let id = string_field(data, "id")?;
        if id.is_empty() {
            return Err(DesktopRpcError::MalformedPayload(
                "subagent has no id".into(),
            ));
        }
        Ok(Self {
            id,
            agent: string_field(data, "agent")?,
            status: string_field(data, "status")?,
            model: text_field(data, "model")?,
            model_source: text_field(data, "modelSource")?,
            fallback_from: text_field(data, "fallbackFrom")?,
            thinking: text_field(data, "thinking")?,
            last_step: last_step_field(data, "lastStep")?,
            turns: optional_u64_field(data, "turns")?,
            tool_calls: optional_u64_field(data, "toolCalls")?,
            active_tool: text_field(data, "activeTool")?,
            usage: DesktopAgentUsage::from_data(field_object(data, "usage")?)?,
        })
    }
}

/// A job's recorded exit code. The outer `Option` is `None` when the field is
/// absent (no code recorded yet); `Some(Null)` preserves a recorded `null`,
/// an exit without a code, distinct from absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobExitCode {
    Code(i32),
    Null,
}

/// One projected background job entry. Only identity, the human label, the
/// status word, the start/end timestamps, and the exit code cross the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopJob {
    pub id: String,
    pub label: String,
    pub status: String,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub exit_code: Option<JobExitCode>,
}

impl DesktopJob {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let id = string_field(data, "id")?;
        if id.is_empty() {
            return Err(DesktopRpcError::MalformedPayload("job has no id".into()));
        }
        Ok(Self {
            id,
            label: string_field(data, "label")?,
            status: string_field(data, "status")?,
            started_at: u64_field(data, "startedAt")?,
            ended_at: optional_u64_field(data, "endedAt")?,
            exit_code: exit_code_field(data, "exitCode")?,
        })
    }
}

/// The decoded `list_subagents` (and `agents_update`) data.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentList {
    pub agents: Vec<DesktopAgent>,
}

impl SubagentList {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            agents: agent_entries(data)?,
        })
    }
}

/// The decoded `list_jobs` (and `jobs_update`) data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobList {
    pub jobs: Vec<DesktopJob>,
}

impl JobList {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            jobs: job_entries(data)?,
        })
    }
}

/// The actual state after `cancel_subagent`, re-read from the owner. `agent`
/// is the post-action projection; `requested` and `changed` are present only
/// when the server supplies them. They are never defaulted: a running task's
/// cancellation is asynchronous, so a `requested: true` answer with no
/// `changed` marker must not be read as `changed: false`.
#[derive(Debug, Clone, PartialEq)]
pub struct CancelSubagentResult {
    pub agent: DesktopAgent,
    pub requested: Option<bool>,
    pub changed: Option<bool>,
}

impl CancelSubagentResult {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            agent: DesktopAgent::from_data(field_object(data, "agent")?)?,
            requested: optional_bool_field(data, "requested")?,
            changed: optional_bool_field(data, "changed")?,
        })
    }
}

/// The actual state after `cancel_job`, re-read from the owner. `changed` is
/// present only when the server supplies it and is never defaulted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelJobResult {
    pub job: DesktopJob,
    pub changed: Option<bool>,
}

impl CancelJobResult {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            job: DesktopJob::from_data(field_object(data, "job")?)?,
            changed: optional_bool_field(data, "changed")?,
        })
    }
}

/// The closed `status` vocabulary of a provider usage entry. Anything else is
/// a server change and is refused rather than read as usable usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageStatus {
    Ok,
    Pending,
    Stale,
    Failed,
    Unavailable,
}

impl UsageStatus {
    /// Every status the protocol speaks, in wire order.
    pub const ALL: [Self; 5] = [
        Self::Ok,
        Self::Pending,
        Self::Stale,
        Self::Failed,
        Self::Unavailable,
    ];

    /// The wire name of the status.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Pending => "pending",
            Self::Stale => "stale",
            Self::Failed => "failed",
            Self::Unavailable => "unavailable",
        }
    }
}

impl std::str::FromStr for UsageStatus {
    type Err = DesktopRpcError;

    fn from_str(status: &str) -> Result<Self, Self::Err> {
        match status {
            "ok" => Ok(Self::Ok),
            "pending" => Ok(Self::Pending),
            "stale" => Ok(Self::Stale),
            "failed" => Ok(Self::Failed),
            "unavailable" => Ok(Self::Unavailable),
            other => Err(DesktopRpcError::InvalidUsageStatus(other.to_string())),
        }
    }
}

impl fmt::Display for UsageStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The closed `refresh` disposition of a provider usage entry: whether the
/// server can refresh the provider's usage on request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageRefresh {
    Unsupported,
    OutOfScope,
}

impl UsageRefresh {
    /// Every disposition the protocol speaks, in wire order.
    pub const ALL: [Self; 2] = [Self::Unsupported, Self::OutOfScope];

    /// The wire name of the disposition.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::OutOfScope => "out_of_scope",
        }
    }
}

impl std::str::FromStr for UsageRefresh {
    type Err = DesktopRpcError;

    fn from_str(refresh: &str) -> Result<Self, Self::Err> {
        match refresh {
            "unsupported" => Ok(Self::Unsupported),
            "out_of_scope" => Ok(Self::OutOfScope),
            other => Err(DesktopRpcError::InvalidUsageRefresh(other.to_string())),
        }
    }
}

impl fmt::Display for UsageRefresh {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The closed `scope` vocabulary of a usage limit. Only `provider` is spoken
/// today; an unknown scope is refused rather than misread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageLimitScope {
    Provider,
}

impl UsageLimitScope {
    /// The wire name of the scope.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
        }
    }
}

impl std::str::FromStr for UsageLimitScope {
    type Err = DesktopRpcError;

    fn from_str(scope: &str) -> Result<Self, Self::Err> {
        match scope {
            "provider" => Ok(Self::Provider),
            other => Err(DesktopRpcError::InvalidUsageScope(other.to_string())),
        }
    }
}

impl fmt::Display for UsageLimitScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The closed `source` vocabulary of a `rate_limit` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitSource {
    Refresh,
    Response,
}

impl RateLimitSource {
    /// Every source the protocol speaks, in wire order.
    pub const ALL: [Self; 2] = [Self::Refresh, Self::Response];

    /// The wire name of the source.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Refresh => "refresh",
            Self::Response => "response",
        }
    }
}

impl std::str::FromStr for RateLimitSource {
    type Err = DesktopRpcError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        match source {
            "refresh" => Ok(Self::Refresh),
            "response" => Ok(Self::Response),
            other => Err(DesktopRpcError::InvalidRateLimitSource(other.to_string())),
        }
    }
}

impl fmt::Display for RateLimitSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The `resetAt` of a usage window. A window without a recorded reset point
/// is `Null`; an absent `resetAt` is malformed and never decoded, so a null
/// reset and a missing one are never conflated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UsageResetAt {
    At(f64),
    Null,
}

/// A finite number field of a usage document. Usage numbers may be fractional
/// but never non-finite.
fn finite_number_field(data: &Value, field: &'static str) -> Result<f64, DesktopRpcError> {
    let value = data
        .get(field)
        .and_then(Value::as_f64)
        .ok_or_else(|| DesktopRpcError::MalformedPayload(format!("{field} must be a number")))?;
    if !value.is_finite() {
        return Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must be a finite number"
        )));
    }
    Ok(value)
}

/// An optional finite number field: absent and `null` both decode to `None`.
fn optional_finite_number_field(
    data: &Value,
    field: &'static str,
) -> Result<Option<f64>, DesktopRpcError> {
    match data.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => {
            let value = number.as_f64().ok_or_else(|| {
                DesktopRpcError::MalformedPayload(format!("{field} must be a finite number"))
            })?;
            if !value.is_finite() {
                return Err(DesktopRpcError::MalformedPayload(format!(
                    "{field} must be a finite number"
                )));
            }
            Ok(Some(value))
        }
        Some(_) => Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must be a finite number"
        ))),
    }
}

/// A required `resetAt` field: a finite number or `null`, never absent.
fn usage_reset_at_field(
    data: &Value,
    field: &'static str,
) -> Result<UsageResetAt, DesktopRpcError> {
    match data.get(field) {
        Some(Value::Null) => Ok(UsageResetAt::Null),
        Some(Value::Number(number)) => {
            let value = number.as_f64().ok_or_else(|| {
                DesktopRpcError::MalformedPayload(format!(
                    "{field} must be a finite number or null"
                ))
            })?;
            if !value.is_finite() {
                return Err(DesktopRpcError::MalformedPayload(format!(
                    "{field} must be a finite number or null"
                )));
            }
            Ok(UsageResetAt::At(value))
        }
        _ => Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must be a finite number or null"
        ))),
    }
}

/// A required `status` field of a provider usage entry.
fn usage_status_field(data: &Value, field: &'static str) -> Result<UsageStatus, DesktopRpcError> {
    data.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            DesktopRpcError::MalformedPayload(format!("{field} must be a status string"))
        })?
        .parse()
}

/// An optional closed-vocabulary string field: absent and `null` decode to
/// `None`, any other value is parsed through the vocabulary.
fn optional_usage_refresh_field(
    data: &Value,
    field: &'static str,
) -> Result<Option<UsageRefresh>, DesktopRpcError> {
    match data.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => text.parse().map(Some),
        Some(_) => Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must be a refresh disposition string"
        ))),
    }
}

/// An optional `scope` field of a usage limit.
fn optional_usage_scope_field(
    data: &Value,
    field: &'static str,
) -> Result<Option<UsageLimitScope>, DesktopRpcError> {
    match data.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => text.parse().map(Some),
        Some(_) => Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must be a scope string"
        ))),
    }
}

/// A required `source` field of a `rate_limit` event.
fn rate_limit_source_field(
    data: &Value,
    field: &'static str,
) -> Result<RateLimitSource, DesktopRpcError> {
    data.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            DesktopRpcError::MalformedPayload(format!("{field} must be a source string"))
        })?
        .parse()
}

/// A required array of provider usage entries from a usage document, named by
/// the calling field (`providers` or `snapshots`).
fn provider_entries(
    data: &Value,
    field: &'static str,
) -> Result<Vec<ProviderUsage>, DesktopRpcError> {
    let items = data.get(field).and_then(Value::as_array).ok_or_else(|| {
        DesktopRpcError::MalformedPayload(format!("usage data has no {field} array"))
    })?;
    items.iter().map(ProviderUsage::from_data).collect()
}

/// A provider usage entry's `limits`. Absent (and `null`) default to an empty
/// list — a pending, failed or unavailable provider has no limits to report —
/// while a present non-array value stays malformed.
fn usage_limit_entries(data: &Value) -> Result<Vec<UsageLimit>, DesktopRpcError> {
    match data.get("limits") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items.iter().map(UsageLimit::from_data).collect(),
        Some(_) => Err(DesktopRpcError::MalformedPayload(
            "usage entry limits must be an array".into(),
        )),
    }
}

/// A required `windows` array of a usage limit.
fn usage_window_entries(data: &Value) -> Result<Vec<UsageWindow>, DesktopRpcError> {
    let items = data
        .get("windows")
        .and_then(Value::as_array)
        .ok_or_else(|| DesktopRpcError::MalformedPayload("limit has no windows array".into()))?;
    items.iter().map(UsageWindow::from_data).collect()
}

/// The decoded `get_subscription_usage` / `refresh_subscription_usage` data:
/// one entry per provider the server reports.
#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionUsage {
    pub providers: Vec<ProviderUsage>,
}

impl SubscriptionUsage {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            providers: provider_entries(data, "providers")?,
        })
    }
}

/// One projected provider usage entry. Only these fields exist on the wire:
/// identity, the closed status, the optional fetch timestamp and plan name,
/// the optional refresh disposition, and the limits. No credential, account
/// or URL fields are part of this projection.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsage {
    pub provider: String,
    pub status: UsageStatus,
    pub fetched_at: Option<f64>,
    pub plan: Option<String>,
    pub refresh: Option<UsageRefresh>,
    pub limits: Vec<UsageLimit>,
}

impl ProviderUsage {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let provider = string_field(data, "provider")?;
        if provider.is_empty() {
            return Err(DesktopRpcError::MalformedPayload(
                "provider entry has no provider".into(),
            ));
        }
        Ok(Self {
            provider,
            status: usage_status_field(data, "status")?,
            fetched_at: optional_finite_number_field(data, "fetchedAt")?,
            plan: text_field(data, "plan")?,
            refresh: optional_usage_refresh_field(data, "refresh")?,
            limits: usage_limit_entries(data)?,
        })
    }
}

/// One projected usage limit: its name, whether it is reached, the optional
/// scope hint, and the bounded windows.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageLimit {
    pub name: String,
    pub limit_reached: bool,
    pub scope: Option<UsageLimitScope>,
    pub windows: Vec<UsageWindow>,
}

impl UsageLimit {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let name = string_field(data, "name")?;
        if name.is_empty() {
            return Err(DesktopRpcError::MalformedPayload(
                "limit has no name".into(),
            ));
        }
        Ok(Self {
            name,
            limit_reached: bool_field(data, "limitReached")?,
            scope: optional_usage_scope_field(data, "scope")?,
            windows: usage_window_entries(data)?,
        })
    }
}

/// One projected usage window: the human label, the derived usage percentage,
/// the window length, its reset point, and the raw used/budget allowances when
/// the server supplies them.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageWindow {
    pub label: String,
    pub used_percent: f64,
    pub window_seconds: f64,
    pub reset_at: UsageResetAt,
    pub used: Option<f64>,
    pub budget: Option<f64>,
}

impl UsageWindow {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let label = string_field(data, "label")?;
        if label.is_empty() {
            return Err(DesktopRpcError::MalformedPayload(
                "window has no label".into(),
            ));
        }
        Ok(Self {
            label,
            used_percent: finite_number_field(data, "usedPercent")?,
            window_seconds: finite_number_field(data, "windowSeconds")?,
            reset_at: usage_reset_at_field(data, "resetAt")?,
            used: optional_finite_number_field(data, "used")?,
            budget: optional_finite_number_field(data, "budget")?,
        })
    }
}

/// The decoded `rate_limit` event data: which path hit the limit and the
/// provider snapshots it carries.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitEvent {
    pub source: RateLimitSource,
    pub snapshots: Vec<ProviderUsage>,
}

impl RateLimitEvent {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            source: rate_limit_source_field(data, "source")?,
            snapshots: provider_entries(data, "snapshots")?,
        })
    }
}

/// The closed `transition` vocabulary of a `design_canvas` event. Anything
/// else is a server change and is refused rather than read as a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DesignTransition {
    Open,
    Refresh,
    Stop,
}

impl DesignTransition {
    /// Every transition the protocol speaks, in wire order.
    pub const ALL: [Self; 3] = [Self::Open, Self::Refresh, Self::Stop];

    /// The wire name of the transition.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Refresh => "refresh",
            Self::Stop => "stop",
        }
    }
}

impl std::str::FromStr for DesignTransition {
    type Err = DesktopRpcError;

    fn from_str(transition: &str) -> Result<Self, Self::Err> {
        match transition {
            "open" => Ok(Self::Open),
            "refresh" => Ok(Self::Refresh),
            "stop" => Ok(Self::Stop),
            other => Err(DesktopRpcError::InvalidDesignTransition(other.to_string())),
        }
    }
}

impl fmt::Display for DesignTransition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A required `transition` field of a `design_canvas` event.
fn design_transition_field(
    data: &Value,
    field: &'static str,
) -> Result<DesignTransition, DesktopRpcError> {
    data.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            DesktopRpcError::MalformedPayload(format!("{field} must be a transition string"))
        })?
        .parse()
}

/// A required array of projected design entries.
fn design_entries(data: &Value) -> Result<Vec<DesignEntry>, DesktopRpcError> {
    let items = data
        .get("designs")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            DesktopRpcError::MalformedPayload("design canvas data has no designs array".into())
        })?;
    items.iter().map(DesignEntry::from_data).collect()
}

/// One projected design entry: identity by slug, the human title, and the
/// owner-reported modification time as a non-negative finite number.
#[derive(Debug, Clone, PartialEq)]
pub struct DesignEntry {
    pub slug: String,
    pub title: String,
    pub mtime: f64,
}

impl DesignEntry {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let slug = string_field(data, "slug")?;
        if slug.is_empty() {
            return Err(DesktopRpcError::MalformedPayload(
                "design entry has no slug".into(),
            ));
        }
        Ok(Self {
            slug,
            title: string_field(data, "title")?,
            mtime: cost_field(data, "mtime")?,
        })
    }
}

/// The decoded `get_design_canvas` data, and the same state every
/// `design_canvas` event carries alongside its `transition`. `slug` is `null`
/// when no design is open; `url` is absent unless an `includeUrl` request (or
/// an `open`/`refresh` transition) chose to carry it.
#[derive(Debug, Clone, PartialEq)]
pub struct DesignCanvas {
    pub running: bool,
    pub slug: Option<String>,
    pub connected_clients: u64,
    pub designs: Vec<DesignEntry>,
    pub url: Option<String>,
}

impl DesignCanvas {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Self::with_designs(data, design_entries(data)?)
    }

    fn with_designs(data: &Value, designs: Vec<DesignEntry>) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            running: bool_field(data, "running")?,
            slug: text_field(data, "slug")?,
            connected_clients: u64_field(data, "connectedClients")?,
            designs,
            url: text_field(data, "url")?,
        })
    }
}

/// The decoded `design_canvas` event: the transition that happened plus the
/// same canvas state as `get_design_canvas`, except the design list, which
/// only the request carries (Gentle Shell 4.8.0's events have `running`,
/// `slug`, `connectedClients` and the optional `url`), so `state.designs` is
/// empty unless an event does send one. A `stop` transition carries no URL;
/// the projection keeps whatever the wire actually supplied.
#[derive(Debug, Clone, PartialEq)]
pub struct DesignCanvasEvent {
    pub transition: DesignTransition,
    pub state: DesignCanvas,
}

impl DesignCanvasEvent {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let designs = match data.get("designs") {
            None => Vec::new(),
            Some(_) => design_entries(data)?,
        };
        Ok(Self {
            transition: design_transition_field(data, "transition")?,
            state: DesignCanvas::with_designs(data, designs)?,
        })
    }
}

/// The nine configuration stores the protocol names. A store is an
/// identifier, never a path; anything else is refused. Gentle Shell 4.9.0
/// knows only the first six: it answers `unknown_store` for the settings
/// stores (`persona`, `banner`, `notifications`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigStore {
    Profiles,
    Models,
    ProfileFallbacks,
    Subagents,
    Pins,
    ManagedAssets,
    Persona,
    Banner,
    Notifications,
}

impl ConfigStore {
    /// Every store the protocol speaks, in wire order.
    pub const ALL: [Self; 9] = [
        Self::Profiles,
        Self::Models,
        Self::ProfileFallbacks,
        Self::Subagents,
        Self::Pins,
        Self::ManagedAssets,
        Self::Persona,
        Self::Banner,
        Self::Notifications,
    ];

    /// The wire name of the store.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Profiles => "profiles",
            Self::Models => "models",
            Self::ProfileFallbacks => "profile_fallbacks",
            Self::Subagents => "subagents",
            Self::Pins => "pins",
            Self::ManagedAssets => "managed_assets",
            Self::Persona => "persona",
            Self::Banner => "banner",
            Self::Notifications => "notifications",
        }
    }

    /// The owner layers the store is assembled from.
    pub fn scope(self) -> ConfigScope {
        match self {
            Self::Profiles
            | Self::ProfileFallbacks
            | Self::ManagedAssets
            | Self::Banner
            | Self::Notifications => ConfigScope::Global,
            Self::Models | Self::Subagents | Self::Persona => ConfigScope::GlobalProject,
            Self::Pins => ConfigScope::LocalRepo,
        }
    }

    /// Whether the store is derived or installer-owned, so `set_config` can
    /// never target it.
    pub fn is_read_only(self) -> bool {
        self.write_targets().is_empty()
    }

    /// The files a write to this store may replace; empty when read-only.
    /// `persona`'s `global` target also covers the project file when that
    /// one already exists.
    pub fn write_targets(self) -> &'static [ConfigWriteTarget] {
        match self {
            Self::Profiles
            | Self::ProfileFallbacks
            | Self::Persona
            | Self::Banner
            | Self::Notifications => &[ConfigWriteTarget::Global],
            Self::Pins => &[ConfigWriteTarget::Local, ConfigWriteTarget::Repo],
            Self::Models | Self::Subagents | Self::ManagedAssets => &[],
        }
    }
}

impl std::str::FromStr for ConfigStore {
    type Err = DesktopRpcError;

    fn from_str(store: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == store)
            .ok_or_else(|| DesktopRpcError::UnknownConfigStore(store.to_string()))
    }
}

impl fmt::Display for ConfigStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The closed `scope` vocabulary of a config store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigScope {
    Global,
    GlobalProject,
    LocalRepo,
}

impl ConfigScope {
    /// Every scope the protocol speaks, in wire order.
    pub const ALL: [Self; 3] = [Self::Global, Self::GlobalProject, Self::LocalRepo];

    /// The wire name of the scope.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::GlobalProject => "global+project",
            Self::LocalRepo => "local+repo",
        }
    }
}

impl std::str::FromStr for ConfigScope {
    type Err = DesktopRpcError;

    fn from_str(scope: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == scope)
            .ok_or_else(|| DesktopRpcError::InvalidConfigScope(scope.to_string()))
    }
}

impl fmt::Display for ConfigScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The closed `status` vocabulary of a store, a layer and a write target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigStatus {
    Ok,
    Missing,
    Invalid,
}

impl ConfigStatus {
    /// Every status the protocol speaks, in wire order.
    pub const ALL: [Self; 3] = [Self::Ok, Self::Missing, Self::Invalid];

    /// The wire name of the status.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::Invalid => "invalid",
        }
    }
}

impl std::str::FromStr for ConfigStatus {
    type Err = DesktopRpcError;

    fn from_str(status: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == status)
            .ok_or_else(|| DesktopRpcError::InvalidConfigStatus(status.to_string()))
    }
}

impl fmt::Display for ConfigStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The closed `source` vocabulary of a store layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigLayerSource {
    Global,
    Project,
    Local,
    Repo,
}

impl ConfigLayerSource {
    /// Every layer source the protocol speaks, in wire order.
    pub const ALL: [Self; 4] = [Self::Global, Self::Project, Self::Local, Self::Repo];

    /// The wire name of the layer source.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
            Self::Local => "local",
            Self::Repo => "repo",
        }
    }
}

impl std::str::FromStr for ConfigLayerSource {
    type Err = DesktopRpcError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == source)
            .ok_or_else(|| DesktopRpcError::InvalidConfigLayerSource(source.to_string()))
    }
}

impl fmt::Display for ConfigLayerSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The closed `target` vocabulary of a write target: one file, not a store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigWriteTarget {
    Global,
    Local,
    Repo,
}

impl ConfigWriteTarget {
    /// Every write target the protocol speaks, in wire order.
    pub const ALL: [Self; 3] = [Self::Global, Self::Local, Self::Repo];

    /// The wire name of the write target.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Local => "local",
            Self::Repo => "repo",
        }
    }
}

impl std::str::FromStr for ConfigWriteTarget {
    type Err = DesktopRpcError;

    fn from_str(target: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == target)
            .ok_or_else(|| DesktopRpcError::InvalidConfigWriteTarget(target.to_string()))
    }
}

impl fmt::Display for ConfigWriteTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The pin layer a `pins` write selects. There is no default and no
/// inference from which file exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PinSource {
    /// Clone-wide, under the Git common dir.
    Local,
    /// Committable, per worktree.
    Repo,
}

impl PinSource {
    /// The wire name of the pin layer.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Repo => "repo",
        }
    }

    /// The write target whose `writeRevision` guards this layer.
    pub fn write_target(self) -> ConfigWriteTarget {
        match self {
            Self::Local => ConfigWriteTarget::Local,
            Self::Repo => ConfigWriteTarget::Repo,
        }
    }
}

impl fmt::Display for PinSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Check that `data` is a JSON object. The server's owner normalizer is the
/// validator of the content; the `set_config` envelope, not this check, keeps
/// paths and extra keys out of the request.
fn store_document(data: &Value, kind: &'static str) -> Result<(), DesktopRpcError> {
    if data.is_object() {
        Ok(())
    } else {
        Err(DesktopRpcError::InvalidConfigData(format!(
            "{kind} data must be an object"
        )))
    }
}

/// The complete `profiles` store content (`AgentProfilesFile`), as
/// `get_config` returned it. Only the JSON object shape is checked here; the
/// server's owner normalizer validates the content and answers `invalid_data`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfilesData(Value);

impl ProfilesData {
    pub const KIND: &'static str = "gentle-pi.agent_model_profiles";

    pub fn new(data: Value) -> Result<Self, DesktopRpcError> {
        store_document(&data, Self::KIND)?;
        Ok(Self(data))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

/// The complete `profile_fallbacks` store content (`FallbacksFile`), as
/// `get_config` returned it. Only the JSON object shape is checked here; the
/// server's owner normalizer validates the content and answers `invalid_data`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileFallbacksData(Value);

impl ProfileFallbacksData {
    pub const KIND: &'static str = "gentle-pi.profile_model_fallbacks";

    pub fn new(data: Value) -> Result<Self, DesktopRpcError> {
        store_document(&data, Self::KIND)?;
        Ok(Self(data))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

wire_enum! {
    /// The persona a `persona` store holds (`{ mode }`).
    PersonaMode, "mode" {
        Gentleman => "gentleman",
        Neutral => "neutral",
    }
}

/// The complete `banner` store content, exactly `{ showRose, showTextLogo,
/// color }`. Only the JSON object shape is checked here; the server keeps
/// only those three settings and answers `invalid_data` for anything else.
#[derive(Debug, Clone, PartialEq)]
pub struct BannerData(Value);

impl BannerData {
    pub fn new(data: Value) -> Result<Self, DesktopRpcError> {
        store_document(&data, "banner")?;
        Ok(Self(data))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

/// The complete `notifications` store content, as `get_config` returned it:
/// the validated settings without `schema`, every file sound as the
/// `"file"` marker (sent back where it was, it keeps that file). Only the
/// JSON object shape is checked here; the server's owner validates the rest.
#[derive(Debug, Clone, PartialEq)]
pub struct NotificationsData(Value);

impl NotificationsData {
    pub fn new(data: Value) -> Result<Self, DesktopRpcError> {
        store_document(&data, "notifications")?;
        Ok(Self(data))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

/// One `set_config` write. Only the writable stores exist here, so a
/// read-only store cannot be targeted through the typed API.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigWrite {
    /// Replace the whole `profiles` store.
    Profiles(ProfilesData),
    /// Replace the whole `profile_fallbacks` store.
    ProfileFallbacks(ProfileFallbacksData),
    /// Pin `profile` in one layer, or clear that layer with `None` (sent as
    /// `profile: null`).
    Pin {
        source: PinSource,
        profile: Option<String>,
    },
    /// Set the persona as `/gentle:persona` does: the global file, plus the
    /// project file only when it already exists.
    Persona(PersonaMode),
    /// Replace the whole `banner` store.
    Banner(BannerData),
    /// Replace the whole `notifications` store.
    Notifications(NotificationsData),
}

impl ConfigWrite {
    /// Pin a profile the `profiles` store defines in one layer.
    pub fn pin(source: PinSource, profile: impl Into<String>) -> Self {
        Self::Pin {
            source,
            profile: Some(profile.into()),
        }
    }

    /// Remove one layer's pin file; the other layer is left as it is.
    pub fn clear_pin(source: PinSource) -> Self {
        Self::Pin {
            source,
            profile: None,
        }
    }

    /// The store this write replaces.
    pub fn store(&self) -> ConfigStore {
        match self {
            Self::Profiles(_) => ConfigStore::Profiles,
            Self::ProfileFallbacks(_) => ConfigStore::ProfileFallbacks,
            Self::Pin { .. } => ConfigStore::Pins,
            Self::Persona(_) => ConfigStore::Persona,
            Self::Banner(_) => ConfigStore::Banner,
            Self::Notifications(_) => ConfigStore::Notifications,
        }
    }

    /// The target whose `writeRevision` must be sent as `expectedRevision`.
    pub fn write_target(&self) -> ConfigWriteTarget {
        match self {
            Self::Pin { source, .. } => source.write_target(),
            Self::Profiles(_)
            | Self::ProfileFallbacks(_)
            | Self::Persona(_)
            | Self::Banner(_)
            | Self::Notifications(_) => ConfigWriteTarget::Global,
        }
    }
}

/// The wire `params` object for `get_config`: exactly `{ store }`.
fn get_config_params(store: ConfigStore) -> Value {
    json!({ "store": store.as_str() })
}

/// The wire `params` object for `set_config`: exactly `{ store, data,
/// expectedRevision }`, with a non-empty revision and, for a pin, a non-empty
/// profile or an explicit `null`.
fn set_config_params(
    write: &ConfigWrite,
    expected_revision: &str,
) -> Result<Value, DesktopRpcError> {
    if expected_revision.is_empty() {
        return Err(DesktopRpcError::InvalidExpectedRevision);
    }
    let data = match write {
        ConfigWrite::Profiles(data) => data.as_value().clone(),
        ConfigWrite::ProfileFallbacks(data) => data.as_value().clone(),
        ConfigWrite::Pin { source, profile } => {
            if profile.as_deref() == Some("") {
                return Err(DesktopRpcError::InvalidPinProfile);
            }
            json!({ "source": source.as_str(), "profile": profile })
        }
        ConfigWrite::Persona(mode) => json!({ "mode": mode.as_str() }),
        ConfigWrite::Banner(data) => data.as_value().clone(),
        ConfigWrite::Notifications(data) => data.as_value().clone(),
    };
    Ok(json!({
        "store": write.store().as_str(),
        "data": data,
        "expectedRevision": expected_revision,
    }))
}

/// The documented `get_config` / `set_config` failure codes. Other codes
/// (`invalid_request`, `internal_error`) are not config-specific.
/// `profile_apply_busy` is also an `apply_profile` failure: `set_config` for
/// `profiles`, `profile_fallbacks` or `pins` answers it while an apply runs,
/// and writes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigErrorCode {
    UnknownStore,
    ReadOnly,
    InvalidData,
    RevisionConflict,
    WriteFailed,
    ConfigUnavailable,
    ProfileApplyBusy,
}

impl ConfigErrorCode {
    /// Every config failure code, in documented order.
    pub const ALL: [Self; 7] = [
        Self::UnknownStore,
        Self::ReadOnly,
        Self::InvalidData,
        Self::RevisionConflict,
        Self::WriteFailed,
        Self::ConfigUnavailable,
        Self::ProfileApplyBusy,
    ];

    /// The wire code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownStore => "unknown_store",
            Self::ReadOnly => "read_only",
            Self::InvalidData => "invalid_data",
            Self::RevisionConflict => "revision_conflict",
            Self::WriteFailed => "write_failed",
            Self::ConfigUnavailable => "config_unavailable",
            Self::ProfileApplyBusy => "profile_apply_busy",
        }
    }

    /// The config failure a wire code names, if it is one.
    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == code)
    }
}

impl fmt::Display for ConfigErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A required closed-vocabulary string field, parsed through its vocabulary.
fn closed_field<T>(data: &Value, field: &'static str) -> Result<T, DesktopRpcError>
where
    T: std::str::FromStr<Err = DesktopRpcError>,
{
    data.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| DesktopRpcError::MalformedPayload(format!("{field} must be a string")))?
        .parse()
}

/// A required non-empty revision string.
fn revision_field(data: &Value, field: &'static str) -> Result<String, DesktopRpcError> {
    let revision = string_field(data, field)?;
    if revision.is_empty() {
        return Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must not be empty"
        )));
    }
    Ok(revision)
}

/// Refuse a write target the store does not own.
fn expect_write_target(
    store: ConfigStore,
    target: ConfigWriteTarget,
) -> Result<(), DesktopRpcError> {
    if store.write_targets().contains(&target) {
        Ok(())
    } else {
        Err(DesktopRpcError::MalformedPayload(format!(
            "{store} has no write target {target}"
        )))
    }
}

/// One file a store is assembled from, and what reading it produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigLayer {
    pub source: ConfigLayerSource,
    pub status: ConfigStatus,
}

impl ConfigLayer {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            source: closed_field(data, "source")?,
            status: closed_field(data, "status")?,
        })
    }
}

/// One file a writable store may replace, with the `writeRevision` a write to
/// it must send as `expectedRevision`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWriteTargetState {
    pub target: ConfigWriteTarget,
    pub status: ConfigStatus,
    pub write_revision: String,
}

impl ConfigWriteTargetState {
    fn from_data(data: &Value, store: ConfigStore) -> Result<Self, DesktopRpcError> {
        let target = closed_field(data, "target")?;
        expect_write_target(store, target)?;
        Ok(Self {
            target,
            status: closed_field(data, "status")?,
            write_revision: revision_field(data, "writeRevision")?,
        })
    }
}

/// The decoded `get_config` data: one store's identity, mutability, status,
/// read revision, normalized content and layers. `store`, `scope` and
/// `readOnly` must agree with the documented store table. `value` is present
/// for an `ok` read and always for `pins`; for any other `missing` or
/// `invalid` read it is `None`, even if the server sent one. `write_targets` is `Some` exactly for
/// the writable stores (possibly empty: `pins` outside a Git worktree) and
/// `None` for the read-only ones.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigState {
    pub store: ConfigStore,
    pub scope: ConfigScope,
    pub read_only: bool,
    pub status: ConfigStatus,
    pub revision: String,
    pub value: Option<Value>,
    pub layers: Vec<ConfigLayer>,
    pub write_targets: Option<Vec<ConfigWriteTargetState>>,
}

impl ConfigState {
    /// The `writeRevision` of one target, the token `set_config` requires.
    pub fn write_revision(&self, target: ConfigWriteTarget) -> Option<&str> {
        self.write_targets
            .as_deref()?
            .iter()
            .find(|state| state.target == target)
            .map(|state| state.write_revision.as_str())
    }

    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let store: ConfigStore = closed_field(data, "store")?;
        let scope: ConfigScope = closed_field(data, "scope")?;
        if scope != store.scope() {
            return Err(DesktopRpcError::MalformedPayload(format!(
                "{store} scope must be {}, got {scope}",
                store.scope()
            )));
        }
        let read_only = bool_field(data, "readOnly")?;
        if read_only != store.is_read_only() {
            return Err(DesktopRpcError::MalformedPayload(format!(
                "{store} readOnly must be {}",
                store.is_read_only()
            )));
        }
        let status = closed_field(data, "status")?;
        // `ok` means normalized content follows, and `pins` always describes
        // what its layers amount to. Any other store that read `missing` or
        // `invalid` produced no content, so a stray `value` there is ignored.
        let value = if status == ConfigStatus::Ok || store == ConfigStore::Pins {
            match data.get("value") {
                Some(value @ Value::Object(_)) => Some(value.clone()),
                _ => {
                    return Err(DesktopRpcError::MalformedPayload(format!(
                        "{store} with status {status} must carry a value object"
                    )));
                }
            }
        } else {
            None
        };
        let layers = data
            .get("layers")
            .and_then(Value::as_array)
            .ok_or_else(|| DesktopRpcError::MalformedPayload("config has no layers array".into()))?
            .iter()
            .map(ConfigLayer::from_data)
            .collect::<Result<_, _>>()?;
        let write_targets = match data.get("writeTargets") {
            None | Some(Value::Null) if read_only => None,
            Some(Value::Array(items)) if !read_only => Some(
                items
                    .iter()
                    .map(|item| ConfigWriteTargetState::from_data(item, store))
                    .collect::<Result<_, _>>()?,
            ),
            _ if read_only => {
                return Err(DesktopRpcError::MalformedPayload(format!(
                    "read-only {store} must not offer writeTargets"
                )));
            }
            _ => {
                return Err(DesktopRpcError::MalformedPayload(format!(
                    "writable {store} has no writeTargets array"
                )));
            }
        };
        Ok(Self {
            store,
            scope,
            read_only,
            status,
            revision: revision_field(data, "revision")?,
            value,
            layers,
            write_targets,
        })
    }
}

/// The decoded `set_config` data: the store re-read after the attempt, the
/// target this request named, that target's resulting `writeRevision` (the
/// next `expectedRevision`), and whether the content actually changed.
#[derive(Debug, Clone, PartialEq)]
pub struct SetConfigResult {
    pub state: ConfigState,
    pub write_target: ConfigWriteTarget,
    pub write_revision: String,
    pub changed: bool,
}

impl SetConfigResult {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let state = ConfigState::from_data(data)?;
        let write_target = closed_field(data, "writeTarget")?;
        expect_write_target(state.store, write_target)?;
        let write_revision = revision_field(data, "writeRevision")?;
        if state.write_revision(write_target) != Some(write_revision.as_str()) {
            return Err(DesktopRpcError::MalformedPayload(format!(
                "writeRevision does not match the {write_target} write target"
            )));
        }
        Ok(Self {
            state,
            write_target,
            write_revision,
            changed: bool_field(data, "changed")?,
        })
    }
}

/// Where a `config_changed` came from, and what only that origin carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigChangeOrigin {
    /// A `set_config` on this store, after its response: the target it
    /// wrote and that target's resulting `writeRevision`.
    Desktop {
        write_target: ConfigWriteTarget,
        write_revision: String,
    },
    /// A best-effort observation of a change made elsewhere (or a store a
    /// Desktop write changed indirectly, such as a pin gone stale): the
    /// store's status now. No write target: re-read with `get_config`.
    External { status: ConfigStatus },
}

/// The data of a `config_changed` event: the store's resulting `revision`
/// (what `get_config` and `get_snapshot` answer now) and its origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigChanged {
    pub store: ConfigStore,
    pub scope: ConfigScope,
    pub revision: String,
    pub origin: ConfigChangeOrigin,
}

impl ConfigChanged {
    /// The wire `source` of the event.
    pub fn source(&self) -> ChangeSource {
        match self.origin {
            ConfigChangeOrigin::Desktop { .. } => ChangeSource::Desktop,
            ConfigChangeOrigin::External { .. } => ChangeSource::External,
        }
    }

    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let store: ConfigStore = closed_field(data, "store")?;
        let scope = closed_field(data, "scope")?;
        if scope != store.scope() {
            return Err(DesktopRpcError::MalformedPayload(format!(
                "{store} scope must be {}, got {scope}",
                store.scope()
            )));
        }
        let origin = match closed_field(data, "source")? {
            ChangeSource::Desktop => {
                let write_target = closed_field(data, "writeTarget")?;
                expect_write_target(store, write_target)?;
                ConfigChangeOrigin::Desktop {
                    write_target,
                    write_revision: revision_field(data, "writeRevision")?,
                }
            }
            ChangeSource::External => ConfigChangeOrigin::External {
                status: closed_field(data, "status")?,
            },
        };
        Ok(Self {
            store,
            scope,
            revision: revision_field(data, "revision")?,
            origin,
        })
    }
}

/// Longest `apply_profile` profile name and confirmation token, in UTF-16
/// code units like the request id.
const MAX_PROFILE_APPLY_UNITS: usize = 128;

/// Whether `text` is 1 to [`MAX_PROFILE_APPLY_UNITS`] UTF-16 code units.
fn within_apply_limit(text: &str) -> bool {
    (1..=MAX_PROFILE_APPLY_UNITS).contains(&text.encode_utf16().count())
}

/// What `apply_profile` does with the saved profile: the `/gentle:profiles`
/// screen's Make default for new sessions, or its Use in this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProfileApplyMode {
    Default,
    Session,
}

impl ProfileApplyMode {
    /// Every mode the protocol speaks, in wire order.
    pub const ALL: [Self; 2] = [Self::Default, Self::Session];

    /// The wire name of the mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Session => "session",
        }
    }
}

impl std::str::FromStr for ProfileApplyMode {
    type Err = DesktopRpcError;

    fn from_str(mode: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == mode)
            .ok_or_else(|| DesktopRpcError::InvalidProfileApplyMode(mode.to_string()))
    }
}

impl fmt::Display for ProfileApplyMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The wire `params` object for `apply_profile`: exactly `{ profile, mode }`
/// and, only when given, `confirmation`.
fn apply_profile_params(
    profile: &str,
    mode: ProfileApplyMode,
    confirmation: Option<&str>,
) -> Result<Value, DesktopRpcError> {
    if !within_apply_limit(profile) {
        return Err(DesktopRpcError::InvalidProfileName);
    }
    let mut params = json!({ "profile": profile, "mode": mode.as_str() });
    if let Some(token) = confirmation {
        if !within_apply_limit(token) {
            return Err(DesktopRpcError::InvalidConfirmationToken);
        }
        params["confirmation"] = json!(token);
    }
    Ok(params)
}

/// The closed `level` vocabulary of an `apply_profile` note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProfileNoteLevel {
    Info,
    Warning,
    Error,
}

impl ProfileNoteLevel {
    /// Every level the protocol speaks, in wire order.
    pub const ALL: [Self; 3] = [Self::Info, Self::Warning, Self::Error];

    /// The wire name of the level.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl std::str::FromStr for ProfileNoteLevel {
    type Err = DesktopRpcError;

    fn from_str(level: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == level)
            .ok_or_else(|| DesktopRpcError::InvalidProfileNoteLevel(level.to_string()))
    }
}

impl fmt::Display for ProfileNoteLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One notice the `/gentle:profiles` screen raised while applying, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileNote {
    pub level: ProfileNoteLevel,
    pub text: String,
}

impl ProfileNote {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            level: closed_field(data, "level")?,
            text: string_field(data, "text")?,
        })
    }
}

/// The dialog the screen would show before writing global routing. Desktop
/// shows `title` and `message` to the human and resends with `token` only
/// when they approve; the token is bound to this exact text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileConfirmation {
    pub title: String,
    pub message: String,
    pub token: String,
}

impl ProfileConfirmation {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let token = string_field(data, "token")?;
        if token.is_empty() {
            return Err(DesktopRpcError::MalformedPayload(
                "confirmation token must not be empty".into(),
            ));
        }
        Ok(Self {
            title: string_field(data, "title")?,
            message: string_field(data, "message")?,
            token,
        })
    }
}

/// How an `apply_profile` request ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyProfileOutcome {
    /// Nothing was written: the human must see this dialog first.
    ConfirmationRequired(ProfileConfirmation),
    /// The screen's last notice was `info`; a `profile_applied` event follows.
    Applied(Vec<ProfileNote>),
    /// The screen ended with a warning or with no notice; the notes say why.
    NotApplied(Vec<ProfileNote>),
}

/// The decoded `apply_profile` data: the profile and mode it answers, and
/// its outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyProfileResult {
    pub profile: String,
    pub mode: ProfileApplyMode,
    pub outcome: ApplyProfileOutcome,
}

impl ApplyProfileResult {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let profile = profile_field(data)?;
        let mode = closed_field(data, "mode")?;
        let status = string_field(data, "status")?;
        let outcome = match status.as_str() {
            "confirmation_required" => ApplyProfileOutcome::ConfirmationRequired(
                ProfileConfirmation::from_data(field_object(data, "confirmation")?)?,
            ),
            "applied" => ApplyProfileOutcome::Applied(note_entries(data)?),
            "not_applied" => ApplyProfileOutcome::NotApplied(note_entries(data)?),
            _ => return Err(DesktopRpcError::InvalidProfileApplyStatus(status)),
        };
        Ok(Self {
            profile,
            mode,
            outcome,
        })
    }
}

/// The data of a `profile_applied` event: the profile and mode an
/// `applied` answer just reported. `source` is `desktop` because an apply
/// from the `/gentle:profiles` screen emits nothing on this channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileApplied {
    pub profile: String,
    pub mode: ProfileApplyMode,
    pub source: String,
}

impl ProfileApplied {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            profile: profile_field(data)?,
            mode: closed_field(data, "mode")?,
            source: string_field(data, "source")?,
        })
    }
}

/// A required non-empty `profile` name.
fn profile_field(data: &Value) -> Result<String, DesktopRpcError> {
    let profile = string_field(data, "profile")?;
    if profile.is_empty() {
        return Err(DesktopRpcError::MalformedPayload(
            "profile must not be empty".into(),
        ));
    }
    Ok(profile)
}

/// A required array of `apply_profile` notes, possibly empty.
fn note_entries(data: &Value) -> Result<Vec<ProfileNote>, DesktopRpcError> {
    data.get("notes")
        .and_then(Value::as_array)
        .ok_or_else(|| DesktopRpcError::MalformedPayload("apply has no notes array".into()))?
        .iter()
        .map(ProfileNote::from_data)
        .collect()
}

/// The documented `apply_profile` failure codes. Other codes
/// (`invalid_request`, `internal_error`) are not specific to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProfileApplyErrorCode {
    UnknownProfile,
    ProfileApplyBusy,
    ProfilesUnavailable,
}

impl ProfileApplyErrorCode {
    /// Every apply failure code, in documented order.
    pub const ALL: [Self; 3] = [
        Self::UnknownProfile,
        Self::ProfileApplyBusy,
        Self::ProfilesUnavailable,
    ];

    /// The wire code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownProfile => "unknown_profile",
            Self::ProfileApplyBusy => "profile_apply_busy",
            Self::ProfilesUnavailable => "profiles_unavailable",
        }
    }

    /// The apply failure a wire code names, if it is one.
    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == code)
    }
}

impl fmt::Display for ProfileApplyErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Pi's answer to the outer prompt: `disposition: "handled"` means accepted.
/// The domain result is never here, it arrives as a widget record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handoff {
    pub disposition: String,
}

impl Handoff {
    /// Read the outer `response` record for a Desktop prompt.
    pub fn from_pi_response(response: &PiResponse) -> Result<Self, DesktopRpcError> {
        if !response.success {
            return Err(DesktopRpcError::OuterFailed {
                error: response.error.clone(),
            });
        }
        let disposition = response
            .data
            .get("disposition")
            .and_then(Value::as_str)
            .map(str::to_string);
        match disposition.as_deref() {
            // Only `handled` is acceptance. `started` and `queued` describe an
            // intermediate step, so the caller keeps waiting for the widget
            // record (or abandons the request when its transport times out).
            Some("handled") => Ok(Self {
                disposition: "handled".to_string(),
            }),
            _ => Err(DesktopRpcError::UnexpectedDisposition { disposition }),
        }
    }

    /// Whether Pi accepted the request. It says nothing about the outcome.
    pub fn is_handled(&self) -> bool {
        self.disposition == "handled"
    }
}

/// A session-scoped client: it hands out request ids, keeps one request in
/// flight and enforces the event sequence, so a caller never confuses a
/// response with an event or an old answer with a new one.
///
/// The session is not cloneable on purpose: a copy would hand out the same
/// request ids for different requests. Create one session per backend
/// connection and keep it in the owner of that connection.
#[derive(Debug)]
pub struct DesktopSession {
    next_request_id: u64,
    pending: Option<Pending>,
    last_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    request_id: String,
    method: DesktopMethod,
}

impl Default for DesktopSession {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopSession {
    /// A session whose first request id is `desktop-1`.
    pub fn new() -> Self {
        Self {
            next_request_id: 1,
            pending: None,
            last_seq: None,
        }
    }

    /// The request still awaiting its response, with its method.
    pub fn pending(&self) -> Option<(&str, DesktopMethod)> {
        self.pending
            .as_ref()
            .map(|pending| (pending.request_id.as_str(), pending.method))
    }

    /// The last accepted event sequence.
    pub fn last_sequence(&self) -> Option<u64> {
        self.last_seq
    }

    /// A new request with a fresh id. Only one request may be in flight:
    /// responses are correlated by id, but a serial client keeps the failure
    /// modes obvious.
    pub fn issue(&mut self, method: DesktopMethod) -> Result<DesktopRequest, DesktopRpcError> {
        if method.requires_typed_params() {
            return Err(DesktopRpcError::TypedParamsRequired(method));
        }
        self.issue_with(method, json!({}))
    }

    /// Issue `get_config` with exactly `{ store }`.
    pub fn issue_get_config(
        &mut self,
        store: ConfigStore,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(DesktopMethod::GetConfig, get_config_params(store))
    }

    /// Issue `set_config` with exactly `{ store, data, expectedRevision }`,
    /// refusing the same empty revision and empty pin profile as
    /// [`DesktopRequest::set_config`] without occupying the session.
    pub fn issue_set_config(
        &mut self,
        write: ConfigWrite,
        expected_revision: impl Into<String>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::SetConfig,
            set_config_params(&write, &expected_revision.into())?,
        )
    }

    /// Issue `apply_profile` with exactly `{ profile, mode }` and, when
    /// resending an approved confirmation, its `confirmation` token, refusing
    /// the same names and tokens as [`DesktopRequest::apply_profile`] without
    /// occupying the session.
    pub fn issue_apply_profile(
        &mut self,
        profile: &str,
        mode: ProfileApplyMode,
        confirmation: Option<&str>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::ApplyProfile,
            apply_profile_params(profile, mode, confirmation)?,
        )
    }

    /// Issue `set_permission_mode` with its typed params. `get_permission_mode`
    /// has no params and is issued with [`Self::issue`].
    pub fn issue_set_permission_mode(
        &mut self,
        mode: PermissionMode,
        persist: Option<bool>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::SetPermissionMode,
            set_permission_mode_params(mode, persist)?,
        )
    }

    /// Issue `list_subagents` with its empty params.
    pub fn issue_list_subagents(&mut self) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(DesktopMethod::ListSubagents, json!({}))
    }

    /// Issue `list_jobs` with its empty params.
    pub fn issue_list_jobs(&mut self) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(DesktopMethod::ListJobs, json!({}))
    }

    /// Issue `cancel_subagent` with exactly `{ id }`.
    pub fn issue_cancel_subagent(
        &mut self,
        id: impl Into<String>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(DesktopMethod::CancelSubagent, cancel_params(id.into())?)
    }

    /// Issue `cancel_job` with exactly `{ id }`.
    pub fn issue_cancel_job(
        &mut self,
        id: impl Into<String>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(DesktopMethod::CancelJob, cancel_params(id.into())?)
    }

    /// Issue `get_subscription_usage` with `{}` or exactly `{ provider }`.
    pub fn issue_get_subscription_usage(
        &mut self,
        provider: Option<&str>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::GetSubscriptionUsage,
            subscription_usage_params(provider)?,
        )
    }

    /// Issue `refresh_subscription_usage` with `{}` or exactly `{ provider }`.
    pub fn issue_refresh_subscription_usage(
        &mut self,
        provider: Option<&str>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::RefreshSubscriptionUsage,
            subscription_usage_params(provider)?,
        )
    }

    /// Issue `get_design_canvas` with `{}` or exactly `{ includeUrl }`.
    pub fn issue_get_design_canvas(
        &mut self,
        include_url: Option<bool>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::GetDesignCanvas,
            get_design_canvas_params(include_url),
        )
    }

    /// Issue `get_snapshot` with exactly `{ sections }`, refusing an empty or
    /// repeated list without occupying the session. Gate it on
    /// [`Capabilities::supports_session_sections`].
    pub fn issue_get_snapshot_sections(
        &mut self,
        sections: &[SessionSection],
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::GetSnapshot,
            snapshot_sections_params(sections)?,
        )
    }

    /// Issue `get_job_output` with exactly `{ id }`.
    pub fn issue_get_job_output(&mut self, id: &str) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(DesktopMethod::GetJobOutput, job_output_params(id)?)
    }

    /// Issue `get_subagent_thread` with `{ id }` and, when given,
    /// `sinceVersion`.
    pub fn issue_get_subagent_thread(
        &mut self,
        id: &str,
        since_version: Option<u64>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::GetSubagentThread,
            output::subagent_thread_params(id, since_version)?,
        )
    }

    /// Issue `list_sessions` with the given optional params.
    pub fn issue_list_sessions(
        &mut self,
        query: &ListSessionsQuery,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(DesktopMethod::ListSessions, query.params()?)
    }

    /// Issue `switch_session` with `{ id }` and, when given, `scope`. A
    /// switch replaces the backend session and restarts its event sequence at
    /// 1, so while it is in flight, and once it is answered, [`Self::accept`]
    /// takes the next event as a new baseline instead of refusing it as a
    /// replay. Re-read `get_snapshot` after a changed switch.
    pub fn issue_switch_session(
        &mut self,
        id: &str,
        scope: Option<SessionsScope>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::SwitchSession,
            sessions::switch_session_params(id, scope)?,
        )
    }

    /// Issue `get_change_diff` with exactly `{ root, path }`.
    pub fn issue_get_change_diff(
        &mut self,
        root: &str,
        path: &str,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(DesktopMethod::GetChangeDiff, change_diff_params(root, path))
    }

    /// Issue `set_review_mode` with `{ enabled, scope }` and, when resending
    /// an approved confirmation, its `confirmationToken`.
    pub fn issue_set_review_mode(
        &mut self,
        enabled: bool,
        scope: ReviewScope,
        confirmation_token: Option<&str>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::SetReviewMode,
            review::set_review_mode_params(enabled, scope, confirmation_token)?,
        )
    }

    /// Issue `get_usage_stats` with only the given params.
    pub fn issue_get_usage_stats(
        &mut self,
        range: Option<StatsRange>,
        scope: Option<StatsScope>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::GetUsageStats,
            stats::usage_stats_params(range, scope),
        )
    }

    /// Issue `get_update_status` with `{}` or exactly `{ refresh }`.
    pub fn issue_get_update_status(
        &mut self,
        refresh: Option<bool>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::GetUpdateStatus,
            update_status_params(refresh),
        )
    }

    /// Issue `set_background_subagents` with `{ policy }` and, when resending
    /// an approved confirmation, its `confirmationToken`.
    pub fn issue_set_background_subagents(
        &mut self,
        policy: BackgroundPolicy,
        confirmation_token: Option<&str>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::SetBackgroundSubagents,
            background::set_background_subagents_params(policy, confirmation_token)?,
        )
    }

    /// A new request with a fresh id; `params` is built by the typed callers
    /// above, never from app-supplied JSON.
    fn issue_with(
        &mut self,
        method: DesktopMethod,
        params: Value,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        if let Some(pending) = &self.pending {
            return Err(DesktopRpcError::RequestInFlight {
                request_id: pending.request_id.clone(),
            });
        }
        let request =
            DesktopRequest::build(format!("desktop-{}", self.next_request_id), method, params)?;
        self.next_request_id += 1;
        self.pending = Some(Pending {
            request_id: request.request_id().to_string(),
            method,
        });
        Ok(request)
    }

    /// Give up on the request in flight and return its id: the recovery for a
    /// transport timeout, a cancelled or reloaded session, or an outer
    /// disposition other than `handled`. A late answer to it is refused as
    /// uncorrelated instead of being applied to the next request.
    pub fn abandon(&mut self) -> Option<String> {
        self.pending.take().map(|pending| pending.request_id)
    }

    /// Forget the in-flight request and the event baseline: the backend was
    /// replaced (restart, reload) and neither its answers nor its sequence
    /// apply any more. Ids keep advancing, so a late answer from the previous
    /// backend can never match a new request.
    pub fn reset(&mut self) {
        self.pending = None;
        self.last_seq = None;
    }

    /// Adopt a snapshot's sequence as the event baseline, the documented
    /// recovery after an [`DesktopRpcError::OutOfOrderSequence`] gap. A
    /// snapshot older than the last accepted event is refused, so a stale one
    /// cannot rewind the session into replays.
    pub fn resync_from_snapshot(&mut self, snapshot: &Snapshot) -> Result<(), DesktopRpcError> {
        if let Some(last) = self.last_seq.filter(|last| snapshot.sequence < *last) {
            return Err(DesktopRpcError::StaleSnapshot {
                last,
                sequence: snapshot.sequence,
            });
        }
        self.last_seq = Some(snapshot.sequence);
        Ok(())
    }

    /// Decode and correlate an `extension_ui_request`: `Ok(None)` when it is
    /// not this protocol's widget, an error when it is one but unusable or
    /// unexpected, otherwise the record.
    pub fn accept(
        &mut self,
        request: &UiRequest,
    ) -> Result<Option<DesktopRecord>, DesktopRpcError> {
        let Some(record) = decode_widget(request)? else {
            return Ok(None);
        };
        let switching = self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.method == DesktopMethod::SwitchSession);
        match &record {
            DesktopRecord::Response(response) => {
                let expected = self
                    .pending
                    .as_ref()
                    .map(|pending| pending.request_id.clone());
                let got = response.request_id().map(str::to_string);
                if got.is_some() && got == expected {
                    self.pending = None;
                } else if got.is_none() && !response.is_ok() && expected.is_some() {
                    // A null `requestId` is the server reporting a request it
                    // could not parse. With one request in flight it can only
                    // answer that one, and it must not stay pending forever.
                    self.pending = None;
                } else {
                    return Err(DesktopRpcError::UncorrelatedResponse { expected, got });
                }
                if switching {
                    // The new session numbers its events from 1 again.
                    self.last_seq = None;
                }
            }
            DesktopRecord::Event(event) => match self.last_seq {
                // During a switch either session may speak, and the new one
                // restarts at 1: each event is a new baseline.
                _ if switching => self.last_seq = Some(event.seq),
                Some(last) if event.seq <= last => {
                    return Err(DesktopRpcError::DuplicateSequence {
                        last,
                        got: event.seq,
                    });
                }
                Some(last) if event.seq != last + 1 => {
                    return Err(DesktopRpcError::OutOfOrderSequence {
                        expected: last + 1,
                        got: event.seq,
                    });
                }
                // The first event seen sets the baseline: the session does not
                // know how many events preceded its subscription.
                _ => self.last_seq = Some(event.seq),
            },
        }
        Ok(Some(record))
    }
}

/// Decode one inner JSON document.
pub fn decode_payload(payload: &str) -> Result<DesktopRecord, DesktopRpcError> {
    let value: Value = serde_json::from_str(payload)
        .map_err(|error| DesktopRpcError::MalformedPayload(error.to_string()))?;
    if !value.is_object() {
        return Err(DesktopRpcError::MalformedPayload(
            "record must be a JSON object".into(),
        ));
    }
    let schema = value
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| DesktopRpcError::MalformedPayload("record has no schema".into()))?;
    if schema != SCHEMA {
        return Err(DesktopRpcError::ForeignSchema(schema.to_string()));
    }
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| DesktopRpcError::MalformedPayload("record has no kind".into()))?;
    match kind {
        "response" => decode_response(value),
        "event" => decode_event(value),
        other => Err(DesktopRpcError::UnexpectedKind(other.to_string())),
    }
}

/// Extract the record from the Desktop widget. UI requests of other methods or
/// keys, and a cleared widget, are not this protocol and decode to `None`.
pub fn decode_widget(request: &UiRequest) -> Result<Option<DesktopRecord>, DesktopRpcError> {
    let UiMethod::SetWidget { key, lines, .. } = &request.method else {
        return Ok(None);
    };
    if key != WIDGET_KEY {
        return Ok(None);
    }
    let Some(lines) = lines.as_ref().filter(|lines| !lines.is_empty()) else {
        return Ok(None);
    };
    match lines.as_slice() {
        [line] => decode_payload(line).map(Some),
        lines => Err(DesktopRpcError::MalformedPayload(format!(
            "desktop widget carries {} lines, expected 1",
            lines.len()
        ))),
    }
}

#[derive(Deserialize)]
struct RawResponse {
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    ok: Option<bool>,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    error: Option<RawError>,
}

#[derive(Deserialize)]
struct RawError {
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
struct RawEvent {
    seq: Option<u64>,
    event: Option<String>,
    #[serde(default)]
    data: Option<Value>,
}

fn decode_response(value: Value) -> Result<DesktopRecord, DesktopRpcError> {
    let raw: RawResponse = serde_json::from_value(value)
        .map_err(|error| DesktopRpcError::MalformedPayload(error.to_string()))?;
    // `requestId: null` is documented for a request the server could not
    // parse; an empty id cannot correlate either, so both decode to `None`.
    let request_id = raw.request_id.filter(|request_id| !request_id.is_empty());
    let label = request_id.as_deref().unwrap_or("<no requestId>");
    let ok = raw
        .ok
        .ok_or_else(|| DesktopRpcError::MalformedPayload("response has no ok".into()))?;
    let result: Result<Value, DesktopErrorInfo> = if ok {
        let data = raw.data.ok_or_else(|| {
            DesktopRpcError::MalformedPayload(format!("response {label} has no data"))
        })?;
        Ok(data)
    } else {
        let error = raw.error.ok_or_else(|| {
            DesktopRpcError::MalformedPayload(format!("response {label} has no error"))
        })?;
        let code = error.code.filter(|code| !code.is_empty()).ok_or_else(|| {
            DesktopRpcError::MalformedPayload(format!("response {label} error has no code"))
        })?;
        Err(DesktopErrorInfo {
            code,
            message: error.message.unwrap_or_default(),
        })
    };
    Ok(DesktopRecord::Response(DesktopResponse {
        request_id,
        result,
    }))
}

fn decode_event(value: Value) -> Result<DesktopRecord, DesktopRpcError> {
    let raw: RawEvent = serde_json::from_value(value)
        .map_err(|error| DesktopRpcError::MalformedPayload(error.to_string()))?;
    let seq = raw
        .seq
        .ok_or_else(|| DesktopRpcError::MalformedPayload("event has no seq".into()))?;
    let event = raw
        .event
        .filter(|event| !event.is_empty())
        .ok_or_else(|| DesktopRpcError::MalformedPayload("event has no name".into()))?;
    let data = raw
        .data
        .ok_or_else(|| DesktopRpcError::MalformedPayload("event has no data".into()))?;
    Ok(DesktopRecord::Event(DesktopEvent { seq, event, data }))
}

/// Everything that can go wrong before, while and after a request: request
/// validation, decoding and correlation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopRpcError {
    /// The inner `requestId` is empty or longer than
    /// [`MAX_REQUEST_ID_UNITS`] UTF-16 code units; `units` is its length.
    InvalidRequestId { units: usize },
    /// A method this client does not implement.
    UnsupportedMethod(String),
    /// A permission mode the wire does not define.
    InvalidPermissionMode(String),
    /// A cancel request named an empty id.
    InvalidCancelId,
    /// A subscription usage request named an empty provider.
    InvalidProvider,
    /// A provider usage `status` outside the closed vocabulary.
    InvalidUsageStatus(String),
    /// A provider usage `refresh` disposition outside the closed vocabulary.
    InvalidUsageRefresh(String),
    /// A usage limit `scope` outside the closed vocabulary.
    InvalidUsageScope(String),
    /// A `rate_limit` `source` outside the closed vocabulary.
    InvalidRateLimitSource(String),
    /// A subagent `lastStep` label outside the closed vocabulary.
    InvalidLastStep(String),
    /// A design canvas `transition` outside the closed vocabulary.
    InvalidDesignTransition(String),
    /// A config store identifier outside the nine documented stores.
    UnknownConfigStore(String),
    /// A config `scope` outside the closed vocabulary.
    InvalidConfigScope(String),
    /// A config `status` outside the closed vocabulary.
    InvalidConfigStatus(String),
    /// A config layer `source` outside the closed vocabulary.
    InvalidConfigLayerSource(String),
    /// A config write `target` outside the closed vocabulary.
    InvalidConfigWriteTarget(String),
    /// Store data that is not a JSON object.
    InvalidConfigData(String),
    /// A pin write named an empty profile; clearing a pin is `None`.
    InvalidPinProfile,
    /// A `set_config` request carried an empty `expectedRevision`.
    InvalidExpectedRevision,
    /// An `apply_profile` profile name outside 1–128 characters.
    InvalidProfileName,
    /// An `apply_profile` confirmation token outside 1–128 characters.
    InvalidConfirmationToken,
    /// An `apply_profile` mode outside the closed vocabulary.
    InvalidProfileApplyMode(String),
    /// An `apply_profile` answer `status` outside the closed vocabulary.
    InvalidProfileApplyStatus(String),
    /// An `apply_profile` note `level` outside the closed vocabulary.
    InvalidProfileNoteLevel(String),
    /// A wire value outside the closed vocabulary of `field`.
    InvalidValue { field: &'static str, value: String },
    /// The peer's `capabilities` do not list the method, so it is not sent.
    NotAdvertised(DesktopMethod),
    /// A request named an empty (or, for `switch_session`, over 256
    /// characters) id.
    InvalidId(DesktopMethod),
    /// A `list_sessions` limit outside 1–50.
    InvalidSessionsLimit(u32),
    /// A `list_sessions` cursor that is empty.
    InvalidSessionsCursor,
    /// `get_snapshot` sections that are empty or repeat a name.
    InvalidSessionSections,
    /// A method whose params only its typed constructor may build was sent
    /// through the generic path.
    TypedParamsRequired(DesktopMethod),
    /// An event was decoded through an accessor for another event name.
    UnexpectedEvent { expected: &'static str, got: String },
    /// `params` is not a JSON object.
    ParamsNotObject,
    /// The encoded inner request exceeds [`MAX_REQUEST_BYTES`].
    RequestTooLarge { bytes: usize, limit: usize },
    /// A second request was issued before the first was answered.
    RequestInFlight { request_id: String },
    /// The record belongs to another schema.
    ForeignSchema(String),
    /// The envelope `kind` is neither `response` nor `event`.
    UnexpectedKind(String),
    /// A JSON document that is not the documented shape.
    MalformedPayload(String),
    /// A data document declares a wire value this client does not speak.
    UnexpectedValue {
        field: &'static str,
        expected: &'static str,
        got: String,
    },
    /// No request with that id is awaiting an answer. `got` is `None` when the
    /// record carried no `requestId`.
    UncorrelatedResponse {
        expected: Option<String>,
        got: Option<String>,
    },
    /// An event sequence already seen or already passed.
    DuplicateSequence { last: u64, got: u64 },
    /// An event sequence that skipped numbers, so events were lost.
    OutOfOrderSequence { expected: u64, got: u64 },
    /// A snapshot older than the last accepted event, which would rewind the
    /// session into replays.
    StaleSnapshot { last: u64, sequence: u64 },
    /// The server answered the inner request with an error.
    RemoteError(DesktopErrorInfo),
    /// Pi rejected the outer prompt.
    OuterFailed { error: Option<String> },
    /// Pi answered the prompt with a disposition other than `handled`.
    UnexpectedDisposition { disposition: Option<String> },
}

impl fmt::Display for DesktopRpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequestId { units } => write!(
                formatter,
                "desktop request id must be 1 to {MAX_REQUEST_ID_UNITS} UTF-16 code units, got {units}"
            ),
            Self::UnsupportedMethod(method) => {
                write!(formatter, "unsupported desktop method: {method}")
            }
            Self::InvalidPermissionMode(mode) => {
                write!(formatter, "unknown desktop permission mode: {mode}")
            }
            Self::InvalidCancelId => {
                formatter.write_str("desktop cancel id must be a non-empty string")
            }
            Self::InvalidProvider => {
                formatter.write_str("desktop subscription provider must be a non-empty string")
            }
            Self::InvalidUsageStatus(status) => {
                write!(formatter, "unknown desktop usage status: {status}")
            }
            Self::InvalidUsageRefresh(refresh) => {
                write!(
                    formatter,
                    "unknown desktop usage refresh disposition: {refresh}"
                )
            }
            Self::InvalidUsageScope(scope) => {
                write!(formatter, "unknown desktop usage limit scope: {scope}")
            }
            Self::InvalidRateLimitSource(source) => {
                write!(formatter, "unknown desktop rate_limit source: {source}")
            }
            Self::InvalidLastStep(step) => {
                write!(formatter, "unknown desktop subagent last step: {step}")
            }
            Self::InvalidDesignTransition(transition) => {
                write!(
                    formatter,
                    "unknown desktop design canvas transition: {transition}"
                )
            }
            Self::UnknownConfigStore(store) => {
                write!(formatter, "unknown desktop config store: {store}")
            }
            Self::InvalidConfigScope(scope) => {
                write!(formatter, "unknown desktop config scope: {scope}")
            }
            Self::InvalidConfigStatus(status) => {
                write!(formatter, "unknown desktop config status: {status}")
            }
            Self::InvalidConfigLayerSource(source) => {
                write!(formatter, "unknown desktop config layer source: {source}")
            }
            Self::InvalidConfigWriteTarget(target) => {
                write!(formatter, "unknown desktop config write target: {target}")
            }
            Self::InvalidConfigData(reason) => {
                write!(formatter, "invalid desktop config data: {reason}")
            }
            Self::InvalidPinProfile => {
                formatter.write_str("desktop pin profile must be a non-empty name")
            }
            Self::InvalidExpectedRevision => {
                formatter.write_str("desktop expectedRevision must be a non-empty string")
            }
            Self::InvalidProfileName => write!(
                formatter,
                "desktop profile name must be 1 to {MAX_PROFILE_APPLY_UNITS} characters"
            ),
            Self::InvalidConfirmationToken => write!(
                formatter,
                "desktop confirmation token must be 1 to {MAX_PROFILE_APPLY_UNITS} characters"
            ),
            Self::InvalidProfileApplyMode(mode) => {
                write!(formatter, "unknown desktop profile apply mode: {mode}")
            }
            Self::InvalidProfileApplyStatus(status) => {
                write!(formatter, "unknown desktop profile apply status: {status}")
            }
            Self::InvalidProfileNoteLevel(level) => {
                write!(formatter, "unknown desktop profile note level: {level}")
            }
            Self::InvalidValue { field, value } => {
                write!(formatter, "unknown desktop {field}: {value}")
            }
            Self::NotAdvertised(method) => {
                write!(formatter, "the backend does not offer desktop {method}")
            }
            Self::InvalidId(method) => {
                write!(formatter, "desktop {method} id is empty or too long")
            }
            Self::InvalidSessionsLimit(limit) => write!(
                formatter,
                "desktop list_sessions limit must be 1 to {MAX_SESSIONS_LIMIT}, got {limit}"
            ),
            Self::InvalidSessionsCursor => {
                formatter.write_str("desktop list_sessions cursor must not be empty")
            }
            Self::InvalidSessionSections => formatter
                .write_str("desktop snapshot sections must be a non-empty list of unique names"),
            Self::TypedParamsRequired(method) => {
                write!(
                    formatter,
                    "desktop {method} params must come from its typed request"
                )
            }
            Self::UnexpectedEvent { expected, got } => {
                write!(formatter, "desktop event {got} is not {expected}")
            }
            Self::ParamsNotObject => {
                formatter.write_str("desktop request params must be an object")
            }
            Self::RequestTooLarge { bytes, limit } => write!(
                formatter,
                "desktop request is {bytes} bytes, over the {limit} byte limit"
            ),
            Self::RequestInFlight { request_id } => {
                write!(
                    formatter,
                    "desktop request {request_id} is still unanswered"
                )
            }
            Self::ForeignSchema(schema) => {
                write!(formatter, "not a {SCHEMA} record: {schema}")
            }
            Self::UnexpectedKind(kind) => {
                write!(formatter, "unknown desktop record kind: {kind}")
            }
            Self::MalformedPayload(reason) => {
                write!(formatter, "malformed desktop record: {reason}")
            }
            Self::UnexpectedValue {
                field,
                expected,
                got,
            } => write!(
                formatter,
                "desktop data declares {field} {got}, expected {expected}"
            ),
            Self::UncorrelatedResponse { expected, got } => {
                let got = got.as_deref().unwrap_or("<no requestId>");
                match expected {
                    Some(expected) => write!(
                        formatter,
                        "desktop response answers {got}, but {expected} is pending"
                    ),
                    None => write!(formatter, "desktop response {got} answers no request"),
                }
            }
            Self::DuplicateSequence { last, got } => write!(
                formatter,
                "desktop event sequence {got} was already consumed (last {last})"
            ),
            Self::OutOfOrderSequence { expected, got } => write!(
                formatter,
                "desktop event sequence {got} skipped ahead, expected {expected}"
            ),
            Self::StaleSnapshot { last, sequence } => write!(
                formatter,
                "desktop snapshot sequence {sequence} is older than the last event {last}"
            ),
            Self::RemoteError(info) => {
                write!(formatter, "desktop {}: {}", info.code, info.message)
            }
            Self::OuterFailed { error } => match error {
                Some(error) => write!(formatter, "prompt failed: {error}"),
                None => formatter.write_str("prompt failed"),
            },
            Self::UnexpectedDisposition { disposition } => match disposition {
                Some(disposition) => write!(
                    formatter,
                    "prompt was not handled, disposition: {disposition}"
                ),
                None => formatter.write_str("prompt response carried no disposition"),
            },
        }
    }
}

impl std::error::Error for DesktopRpcError {}
