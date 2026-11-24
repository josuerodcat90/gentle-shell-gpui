//! The one-shot reads window screens make of a tab's Gentle Shell over the
//! Desktop channel: the Sessions browser (`list_sessions`, `switch_session`),
//! Usage › Local stats (`get_usage_stats`), Configuration's update status
//! (`get_update_status`), the Profiles screen's agent list and Pi default
//! orchestrator and the model pickers' default model
//! (`list_agent_definitions`), Gentle Shell's own status (`get_status`),
//! background-subagents policy (`get_background_subagents`,
//! `set_background_subagents`) and sign-ins (`get_auth_status`), and the
//! tab's own credentials reload (`reload_credentials`). Each method is used
//! only when `capabilities` advertises it; otherwise the caller keeps its own
//! path (session files, the stats file scan, the npm registry, the agent
//! files and Pi's `settings.json`, no status, the policy files and
//! `/gentle:background-subagents`, `auth.json`, a tab restart; the model
//! pickers omit the default) and nothing is sent.
//!
//! Requests wait for the shared slot in a queue, like the config stores; each
//! is answered once, as an outcome for its ticket that the screen takes.
//! A `session_switching` answer means a switch was in flight: nothing was
//! done, so screen requests are queued again (a few times) instead of failing.
//! The default-model read never retries. These methods emit no event of their own.

use std::collections::VecDeque;

use pi_rpc::UiRequest;
use pi_rpc::desktop_rpc::{
    AgentDefinitions, AuthStatus, BackgroundPolicy, BackgroundSubagents, Capabilities,
    DesktopErrorCode, DesktopMethod, DesktopRecord, DesktopResponse, DesktopRpcError,
    DesktopSession, ListSessionsQuery, ReloadCredentials, SessionList, SessionsScope,
    SetBackgroundSubagentsResult, ShellStatus, StatsRange, StatsScope, SwitchSessionResult,
    UpdateStatus, UsageStats,
};

use super::Send;

/// Identifies one queued read; never rewound, so a ticket from before a reset
/// never matches.
pub(crate) type ReadTicket = u64;

/// How often a request answered `session_switching` is sent again.
const MAX_RETRIES: u8 = 3;

/// Outcomes nobody took (the screen closed) are not kept forever.
const MAX_OUTCOMES: usize = 16;

/// Whether a screen asks the tab's Gentle Shell or keeps its own path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadSupport {
    /// The channel is still probing: the answer is not known yet.
    NotReady,
    /// The peer advertised the method: ask it.
    Rpc,
    /// No backend, no extension, a failed channel, or capabilities without
    /// the method: the screen's own path.
    Legacy,
}

/// One read a screen asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadRequest {
    ListSessions(ListSessionsQuery),
    /// Switch this tab's session by id, resolved in `scope` (the server's
    /// default, the project, when `None`).
    SwitchSession {
        id: String,
        scope: Option<SessionsScope>,
    },
    UsageStats {
        range: StatsRange,
        scope: StatsScope,
    },
    /// `refresh` runs the lookups (bounded by the server); otherwise only
    /// what is already known is answered.
    UpdateStatus {
        refresh: bool,
    },
    ModelDefault,
    /// The agent definitions and Pi's default orchestrator.
    AgentDefinitions,
    /// The facts `/gentle:status` prints, with its severity.
    Status,
    /// The background-subagents policy as this session's project resolves it.
    BackgroundSubagents,
    /// Write the machine-wide policy: without a token the Shell answers the
    /// confirmation to show; with the token of an approved one it writes.
    SetBackgroundSubagents {
        policy: BackgroundPolicy,
        confirmation_token: Option<String>,
    },
    /// Which providers this session has credentials for (status only).
    AuthStatus,
    /// Make this session pick up credentials added or removed elsewhere.
    ReloadCredentials,
}

impl ReadRequest {
    pub(crate) fn method(&self) -> DesktopMethod {
        match self {
            Self::ListSessions(_) => DesktopMethod::ListSessions,
            Self::SwitchSession { .. } => DesktopMethod::SwitchSession,
            Self::UsageStats { .. } => DesktopMethod::GetUsageStats,
            Self::UpdateStatus { .. } => DesktopMethod::GetUpdateStatus,
            Self::ModelDefault => DesktopMethod::ListAgentDefinitions,
            Self::AgentDefinitions => DesktopMethod::ListAgentDefinitions,
            Self::Status => DesktopMethod::GetStatus,
            Self::BackgroundSubagents => DesktopMethod::GetBackgroundSubagents,
            Self::SetBackgroundSubagents { .. } => DesktopMethod::SetBackgroundSubagents,
            Self::AuthStatus => DesktopMethod::GetAuthStatus,
            Self::ReloadCredentials => DesktopMethod::ReloadCredentials,
        }
    }

    fn issue(
        &self,
        session: &mut DesktopSession,
    ) -> Result<pi_rpc::desktop_rpc::DesktopRequest, DesktopRpcError> {
        match self {
            Self::ListSessions(query) => session.issue_list_sessions(query),
            Self::SwitchSession { id, scope } => session.issue_switch_session(id, *scope),
            Self::UsageStats { range, scope } => {
                session.issue_get_usage_stats(Some(*range), Some(*scope))
            }
            Self::UpdateStatus { refresh } => {
                session.issue_get_update_status(refresh.then_some(true))
            }
            Self::ModelDefault => session.issue(DesktopMethod::ListAgentDefinitions),
            Self::AgentDefinitions => session.issue(DesktopMethod::ListAgentDefinitions),
            Self::Status => session.issue(DesktopMethod::GetStatus),
            Self::BackgroundSubagents => session.issue(DesktopMethod::GetBackgroundSubagents),
            Self::SetBackgroundSubagents {
                policy,
                confirmation_token,
            } => session.issue_set_background_subagents(*policy, confirmation_token.as_deref()),
            Self::AuthStatus => session.issue(DesktopMethod::GetAuthStatus),
            Self::ReloadCredentials => session.issue(DesktopMethod::ReloadCredentials),
        }
    }
}

/// Why a read has no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadFailure {
    /// Asked before the probe said, or of a peer without the method.
    NotOffered,
    /// The server answered `unsupported_method` for an advertised method:
    /// the screen takes its own path.
    Unsupported,
    /// A documented refusal (`not_found`, `sessions_unavailable`,
    /// `stats_unavailable`…), with the server's fixed message.
    Refused {
        code: Option<DesktopErrorCode>,
        message: String,
    },
    Timeout,
    /// The backend was replaced, exited or its channel stopped before the
    /// request was answered.
    Dropped,
    /// A transport or decoding failure.
    Failure(String),
}

impl ReadFailure {
    /// What a screen says about it.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::NotOffered | Self::Unsupported => {
                "This Gentle Shell does not offer that over Desktop.".to_string()
            }
            Self::Refused { message, .. } => message.clone(),
            Self::Timeout => "Gentle Shell did not answer in time.".to_string(),
            Self::Dropped => "Gentle Shell restarted before it answered.".to_string(),
            Self::Failure(error) => format!("The request failed ({error})."),
        }
    }

    /// The documented code, when the server refused.
    pub(crate) fn code(&self) -> Option<DesktopErrorCode> {
        match self {
            Self::Refused { code, .. } => *code,
            _ => None,
        }
    }
}

/// The answer to one ticket.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ReadOutcome {
    Sessions(SessionList),
    Switched(SwitchSessionResult),
    UsageStats(UsageStats),
    UpdateStatus(UpdateStatus),
    ModelDefault(Option<String>),
    AgentDefinitions(AgentDefinitions),
    Status(ShellStatus),
    BackgroundSubagents(BackgroundSubagents),
    BackgroundSubagentsSet(SetBackgroundSubagentsResult),
    AuthStatus(AuthStatus),
    CredentialsReloaded(ReloadCredentials),
    Failed(ReadFailure),
}

/// What one decision produced: the requests to send; `switched` when the
/// tab's session was replaced (the wiring forgets the conversation and
/// re-reads everything, as after Pi's own `switch_session`); `retry` when a
/// request was queued again behind a switch and the queue should be pumped
/// again shortly.
#[derive(Debug, Default)]
pub(in crate::features::session) struct Applied {
    pub(in crate::features::session) sends: Vec<Send>,
    pub(in crate::features::session) switched: bool,
    pub(in crate::features::session) retry: bool,
}

#[derive(Debug)]
struct Queued {
    ticket: ReadTicket,
    request: ReadRequest,
    retries: u8,
}

#[derive(Debug, Default)]
pub(in crate::features::session) struct Reads {
    capabilities: Option<Capabilities>,
    queue: VecDeque<Queued>,
    /// The request on the wire.
    in_flight: Option<Queued>,
    next_ticket: ReadTicket,
    /// Bumped per request, so an old timer cannot drop a newer one.
    generation: u64,
    outcomes: Vec<(ReadTicket, ReadOutcome)>,
    /// A request was queued again behind a switch: nothing is sent until the
    /// wiring's short delay releases the queue.
    held: bool,
}

impl Reads {
    /// Adopt the peer's capabilities once the probe completes.
    pub(in crate::features::session) fn set_capabilities(&mut self, capabilities: Capabilities) {
        self.capabilities = Some(capabilities);
    }

    /// Whether the peer offers `method`; `None` until the probe has said.
    pub(in crate::features::session) fn offers(&self, method: DesktopMethod) -> Option<bool> {
        self.capabilities
            .as_ref()
            .map(|capabilities| capabilities.supports(method))
    }

    /// Queue `request`; [`Self::pump`] sends it.
    pub(in crate::features::session) fn enqueue(
        &mut self,
        request: ReadRequest,
    ) -> Result<ReadTicket, ReadFailure> {
        if self.offers(request.method()) != Some(true) {
            return Err(ReadFailure::NotOffered);
        }
        self.next_ticket += 1;
        self.queue.push_back(Queued {
            ticket: self.next_ticket,
            request,
            retries: 0,
        });
        Ok(self.next_ticket)
    }

    /// Send the next queued request when nothing is in flight on the shared
    /// session. A request that fails validation is answered at once and the
    /// next one is tried.
    pub(in crate::features::session) fn pump(&mut self, session: &mut DesktopSession) -> Applied {
        if self.held {
            return Applied::default();
        }
        while self.in_flight.is_none() && session.pending().is_none() {
            let Some(queued) = self.queue.pop_front() else {
                break;
            };
            match queued.request.issue(session) {
                Ok(request) => {
                    self.in_flight = Some(queued);
                    self.generation += 1;
                    return Applied {
                        sends: vec![Send {
                            request,
                            generation: self.generation,
                        }],
                        ..Applied::default()
                    };
                }
                Err(error) => self.answer(
                    queued.ticket,
                    ReadOutcome::Failed(ReadFailure::Failure(error.to_string())),
                ),
            }
        }
        Applied::default()
    }

    /// Pi answered the outer `prompt` of the request in flight.
    pub(in crate::features::session) fn outer(
        &mut self,
        session: &mut DesktopSession,
        generation: u64,
        handled: bool,
    ) -> Applied {
        if handled || generation != self.generation || !owns_pending(session) {
            return Applied::default();
        }
        let error = DesktopRpcError::UnexpectedDisposition { disposition: None };
        self.rollback(session, ReadFailure::Failure(error.to_string()));
        Applied::default()
    }

    /// The pending timeout for `generation` fired.
    pub(in crate::features::session) fn expire(
        &mut self,
        session: &mut DesktopSession,
        generation: u64,
    ) -> Applied {
        if generation != self.generation || !owns_pending(session) {
            return Applied::default();
        }
        self.rollback(session, ReadFailure::Timeout);
        Applied::default()
    }

    /// One incoming UI request: the answer to the read in flight. These
    /// methods emit no event, so anything else is left alone (the session
    /// still accepts it once, here, as every slice does).
    pub(in crate::features::session) fn apply(
        &mut self,
        session: &mut DesktopSession,
        request: &UiRequest,
    ) -> Applied {
        let awaiting = session.pending().map(|(_, method)| method);
        match session.accept(request) {
            Ok(Some(DesktopRecord::Response(response))) if awaiting.is_some_and(is_read) => {
                self.apply_response(response)
            }
            _ => Applied::default(),
        }
    }

    /// The delay after a `session_switching` answer is over.
    pub(in crate::features::session) fn release(&mut self) {
        self.held = false;
    }

    /// The backend was replaced or exited: every ticket still open is
    /// answered `Dropped`. Outcomes already answered stay for their screen.
    pub(in crate::features::session) fn reset(&mut self) {
        self.capabilities = None;
        if let Some(queued) = self.in_flight.take() {
            self.answer(queued.ticket, ReadOutcome::Failed(ReadFailure::Dropped));
        }
        self.drop_queued();
        self.held = false;
        self.generation += 1;
    }

    /// Answer every queued (not yet sent) ticket `Dropped`: the channel
    /// stopped and nothing will send them.
    pub(in crate::features::session) fn drop_queued(&mut self) {
        while let Some(queued) = self.queue.pop_front() {
            self.answer(queued.ticket, ReadOutcome::Failed(ReadFailure::Dropped));
        }
    }

    /// The answer to `ticket`, once, if it arrived.
    pub(in crate::features::session) fn take(&mut self, ticket: ReadTicket) -> Option<ReadOutcome> {
        let index = self.outcomes.iter().position(|(t, _)| *t == ticket)?;
        Some(self.outcomes.remove(index).1)
    }

    fn answer(&mut self, ticket: ReadTicket, outcome: ReadOutcome) {
        if self.outcomes.len() >= MAX_OUTCOMES {
            self.outcomes.remove(0);
        }
        self.outcomes.push((ticket, outcome));
    }

    /// Drop the request in flight and answer its ticket. The generation
    /// moves, so a late timer or disposition is stale.
    fn rollback(&mut self, session: &mut DesktopSession, failure: ReadFailure) {
        session.abandon();
        self.generation += 1;
        if let Some(queued) = self.in_flight.take() {
            self.answer(queued.ticket, ReadOutcome::Failed(failure));
        }
    }

    fn apply_response(&mut self, response: DesktopResponse) -> Applied {
        let Some(mut queued) = self.in_flight.take() else {
            return Applied::default();
        };
        let decoded = match &queued.request {
            ReadRequest::ListSessions(_) => response.list_sessions().map(ReadOutcome::Sessions),
            ReadRequest::SwitchSession { .. } => {
                response.switch_session().map(ReadOutcome::Switched)
            }
            ReadRequest::UsageStats { .. } => response.usage_stats().map(ReadOutcome::UsageStats),
            ReadRequest::UpdateStatus { .. } => {
                response.update_status().map(ReadOutcome::UpdateStatus)
            }
            ReadRequest::ModelDefault => response.agent_definitions().map(|definitions| {
                ReadOutcome::ModelDefault(
                    definitions
                        .default_orchestrator
                        .route()
                        .map(|(model, _)| model.to_owned()),
                )
            }),
            ReadRequest::AgentDefinitions => response
                .agent_definitions()
                .map(ReadOutcome::AgentDefinitions),
            ReadRequest::Status => response.status().map(ReadOutcome::Status),
            ReadRequest::BackgroundSubagents => response
                .background_subagents()
                .map(ReadOutcome::BackgroundSubagents),
            ReadRequest::SetBackgroundSubagents { .. } => response
                .set_background_subagents()
                .map(ReadOutcome::BackgroundSubagentsSet),
            ReadRequest::AuthStatus => response.auth_status().map(ReadOutcome::AuthStatus),
            ReadRequest::ReloadCredentials => response
                .reload_credentials()
                .map(ReadOutcome::CredentialsReloaded),
        };
        let outcome = match decoded {
            Ok(outcome) => outcome,
            Err(DesktopRpcError::RemoteError(info)) => {
                let code = info.kind();
                if queued.request != ReadRequest::ModelDefault
                    && code.is_some_and(DesktopErrorCode::is_retryable)
                    && queued.retries < MAX_RETRIES
                {
                    // Nothing was done: ask again once the switch answered.
                    queued.retries += 1;
                    self.queue.push_front(queued);
                    self.held = true;
                    return Applied {
                        retry: true,
                        ..Applied::default()
                    };
                }
                if code == Some(DesktopErrorCode::UnsupportedMethod) {
                    if queued.request == ReadRequest::ModelDefault
                        && let Some(capabilities) = &mut self.capabilities
                    {
                        // Treat a refused advertisement like a missing one until reset.
                        capabilities
                            .methods
                            .retain(|name| name != queued.request.method().as_str());
                    }
                    ReadOutcome::Failed(ReadFailure::Unsupported)
                } else {
                    ReadOutcome::Failed(ReadFailure::Refused {
                        code,
                        message: info.message,
                    })
                }
            }
            Err(error) => ReadOutcome::Failed(ReadFailure::Failure(error.to_string())),
        };
        let switched = matches!(&outcome, ReadOutcome::Switched(result) if result.changed);
        self.answer(queued.ticket, outcome);
        Applied {
            switched,
            ..Applied::default()
        }
    }
}

/// The methods this slice issues.
pub(in crate::features::session) fn is_read(method: DesktopMethod) -> bool {
    matches!(
        method,
        DesktopMethod::ListSessions
            | DesktopMethod::SwitchSession
            | DesktopMethod::GetUsageStats
            | DesktopMethod::GetUpdateStatus
            | DesktopMethod::ListAgentDefinitions
            | DesktopMethod::GetStatus
            | DesktopMethod::GetBackgroundSubagents
            | DesktopMethod::SetBackgroundSubagents
            | DesktopMethod::GetAuthStatus
            | DesktopMethod::ReloadCredentials
    )
}

/// Whether the pending request is this slice's: the other slices share the
/// session, so a stale read timer must not drop theirs.
fn owns_pending(session: &DesktopSession) -> bool {
    session.pending().is_some_and(|(_, method)| is_read(method))
}

#[cfg(test)]
mod live;

#[cfg(test)]
mod tests {
    use pi_rpc::desktop_rpc::{
        BackgroundPolicy, DesktopErrorCode, DesktopMethod, ListSessionsQuery, PROMPT_COMMAND,
        SCHEMA, SessionsScope, SetBackgroundSubagentsOutcome, StatsRange, StatsScope, WIDGET_KEY,
    };
    use pi_rpc::{UiMethod, UiRequest};
    use serde_json::{Value, json};

    use super::super::{Desktop, Send};
    use super::{ReadFailure, ReadOutcome, ReadRequest, ReadSupport};

    /// The methods Gentle Shell 4.8.0 advertises: none of the reads.
    const RELEASE_METHODS: [&str; 14] = [
        "capabilities",
        "get_snapshot",
        "get_config",
        "set_config",
        "get_permission_mode",
        "set_permission_mode",
        "get_subscription_usage",
        "refresh_subscription_usage",
        "get_design_canvas",
        "list_jobs",
        "cancel_job",
        "apply_profile",
        "list_subagents",
        "cancel_subagent",
    ];

    fn widget(record: Value) -> UiRequest {
        UiRequest {
            id: "ui-1".into(),
            timeout: None,
            method: UiMethod::SetWidget {
                key: WIDGET_KEY.into(),
                lines: Some(vec![record.to_string()]),
                placement: None,
            },
        }
    }

    fn ok(request_id: &str, data: Value) -> UiRequest {
        widget(json!({
            "schema": SCHEMA, "kind": "response", "requestId": request_id, "ok": true, "data": data,
        }))
    }

    fn error(request_id: &str, code: &str) -> UiRequest {
        widget(json!({
            "schema": SCHEMA, "kind": "response", "requestId": request_id, "ok": false,
            "error": {"code": code, "message": format!("fixed {code} message")},
        }))
    }

    /// A `list_agent_definitions` answer with `orchestrator` as the default.
    fn definitions(orchestrator: Value) -> Value {
        json!({
            "agents": [],
            "truncated": false,
            "problems": { "invalid": 0, "notices": 0 },
            "subagentDefaults": { "model": "p/subagent", "thinking": null, "mode": "task" },
            "defaultOrchestrator": orchestrator
        })
    }

    /// Land the probe with `methods` advertised.
    fn probe(desktop: &mut Desktop, methods: &[&str]) {
        let probe = desktop.ready(true);
        let id = probe.sends[0].request.request_id().to_string();
        let snapshot = desktop.ui(&ok(
            &id,
            json!({
                "schema": SCHEMA, "command": PROMPT_COMMAND, "widget": WIDGET_KEY,
                "methods": methods, "events": [],
            }),
        ));
        let id = snapshot.sends[0].request.request_id().to_string();
        let _ = desktop.ui(&ok(
            &id,
            json!({"schema": SCHEMA, "sequence": 0, "state": {}}),
        ));
    }

    fn probed(methods: &[&str]) -> Desktop {
        let mut desktop = Desktop::new();
        probe(&mut desktop, methods);
        desktop
    }

    fn main_shell() -> Desktop {
        let mut methods = RELEASE_METHODS.to_vec();
        methods.extend([
            "list_sessions",
            "switch_session",
            "get_usage_stats",
            "get_update_status",
            "get_status",
        ]);
        probed(&methods)
    }

    fn sent(sends: &[Send]) -> (String, Value) {
        assert_eq!(sends.len(), 1, "one request at a time");
        let request = &sends[0].request;
        let payload: Value = serde_json::from_str(request.payload()).unwrap();
        (request.request_id().to_string(), payload)
    }

    #[test]
    fn a_release_shell_is_never_asked() {
        let mut desktop = probed(&RELEASE_METHODS);
        for method in [
            DesktopMethod::ListSessions,
            DesktopMethod::SwitchSession,
            DesktopMethod::GetUsageStats,
            DesktopMethod::GetUpdateStatus,
            DesktopMethod::GetStatus,
        ] {
            assert_eq!(desktop.reads_support(method), ReadSupport::Legacy);
        }
        let refused = desktop.reads_enqueue(ReadRequest::UpdateStatus { refresh: false });
        assert_eq!(refused, Err(ReadFailure::NotOffered));
        assert!(desktop.reads_pump().sends.is_empty());
        assert!(desktop.session().pending().is_none(), "nothing was sent");
    }

    #[test]
    fn support_waits_for_the_probe_and_stops_with_the_channel() {
        let mut desktop = Desktop::new();
        assert_eq!(
            desktop.reads_support(DesktopMethod::ListSessions),
            ReadSupport::NotReady,
            "a backend still starting is not decided"
        );
        let _ = desktop.ready(true);
        assert_eq!(
            desktop.reads_support(DesktopMethod::ListSessions),
            ReadSupport::NotReady
        );
        let mut missing = Desktop::new();
        let _ = missing.ready(false);
        assert_eq!(
            missing.reads_support(DesktopMethod::ListSessions),
            ReadSupport::Legacy
        );
        assert_eq!(
            main_shell().reads_support(DesktopMethod::ListSessions),
            ReadSupport::Rpc
        );
    }

    #[test]
    fn model_default_waits_for_the_probe_and_refreshes_on_request() {
        let mut desktop = Desktop::new();
        desktop.request_model_default();
        assert_eq!(desktop.sync_model_default(), None);
        assert!(desktop.reads_pump().sends.is_empty());

        probe(
            &mut desktop,
            &["capabilities", "get_snapshot", "list_agent_definitions"],
        );
        assert_eq!(desktop.sync_model_default(), None);
        let (id, payload) = sent(&desktop.reads_pump().sends);
        assert_eq!(payload["method"], "list_agent_definitions");
        assert_eq!(payload["params"], json!({}));
        let _ = desktop.reads_apply(&ok(
            &id,
            definitions(json!({"status": "set", "model": "p/original", "thinking": null})),
        ));
        assert_eq!(
            desktop.sync_model_default(),
            Some(Some("p/original".into()))
        );
        assert!(desktop.reads_pump().sends.is_empty());

        desktop.request_model_default();
        assert_eq!(desktop.sync_model_default(), None);
        let (refresh, _) = sent(&desktop.reads_pump().sends);
        assert_ne!(id, refresh);
        let _ = desktop.reads_apply(&ok(
            &refresh,
            definitions(json!({"status": "set", "model": "p/new", "thinking": null})),
        ));
        assert_eq!(desktop.sync_model_default(), Some(Some("p/new".into())));
        assert!(desktop.reads_pump().sends.is_empty());
    }

    #[test]
    fn model_default_errors_never_retry_and_unsupported_stays_off() {
        let mut legacy = probed(&RELEASE_METHODS);
        legacy.request_model_default();
        assert_eq!(legacy.sync_model_default(), Some(None));
        assert!(legacy.reads_pump().sends.is_empty());

        let mut desktop = probed(&["capabilities", "get_snapshot", "list_agent_definitions"]);
        for code in [
            "session_switching",
            "agent_definitions_unavailable",
            "unsupported_method",
        ] {
            desktop.request_model_default();
            assert_eq!(desktop.sync_model_default(), None);
            let (id, _) = sent(&desktop.reads_pump().sends);
            let applied = desktop.reads_apply(&error(&id, code));
            assert!(!applied.retry, "{code}");
            assert_eq!(desktop.sync_model_default(), Some(None));
            assert!(desktop.reads_pump().sends.is_empty());
        }
        desktop.request_model_default();
        assert_eq!(desktop.sync_model_default(), Some(None));
        assert!(desktop.reads_pump().sends.is_empty());
    }

    #[test]
    fn model_default_timeout_clears_without_retrying() {
        let mut desktop = probed(&["capabilities", "get_snapshot", "list_agent_definitions"]);
        desktop.request_model_default();
        assert_eq!(desktop.sync_model_default(), None);
        let request = desktop.reads_pump().sends.remove(0);
        let expired = desktop.reads_expire(request.generation);
        assert!(!expired.retry);
        assert_eq!(desktop.sync_model_default(), Some(None));
        assert!(desktop.reads_pump().sends.is_empty());
        assert!(desktop.session().pending().is_none());
    }

    #[test]
    fn timed_out_model_default_cannot_be_restored_by_a_late_reply() {
        let mut desktop = probed(&["capabilities", "get_snapshot", "list_agent_definitions"]);
        desktop.request_model_default();
        assert_eq!(desktop.sync_model_default(), None);
        let send = desktop.reads_pump().sends.remove(0);
        let id = send.request.request_id().to_owned();

        assert!(!desktop.reads_expire(send.generation).retry);
        assert_eq!(desktop.sync_model_default(), Some(None));

        let _ = desktop.reads_apply(&ok(
            &id,
            definitions(json!({"status": "set", "model": "p/late"})),
        ));
        assert_eq!(desktop.sync_model_default(), None);
        assert!(desktop.reads_pump().sends.is_empty());
    }

    #[test]
    fn model_default_ignores_a_replaced_backends_response() {
        let methods = ["capabilities", "get_snapshot", "list_agent_definitions"];
        let mut desktop = probed(&methods);
        desktop.request_model_default();
        assert_eq!(desktop.sync_model_default(), None);
        let old = desktop.reads_pump().sends;
        let (old_id, _) = sent(&old);
        desktop.reset();

        desktop.request_model_default();
        probe(&mut desktop, &methods);
        assert_eq!(desktop.sync_model_default(), None);
        let (new_id, _) = sent(&desktop.reads_pump().sends);
        assert_ne!(old_id, new_id);
        let _ = desktop.reads_expire(old[0].generation);
        let _ = desktop.reads_apply(&ok(
            &old_id,
            definitions(json!({"status": "set", "model": "p/old", "thinking": null})),
        ));
        assert_eq!(desktop.sync_model_default(), None);
        let _ = desktop.reads_apply(&ok(
            &new_id,
            definitions(json!({"status": "unset", "model": null, "thinking": null})),
        ));
        assert_eq!(desktop.sync_model_default(), Some(None));
    }

    #[test]
    fn a_page_is_listed_and_answered_once() {
        let mut desktop = main_shell();
        let query = ListSessionsQuery {
            scope: Some(SessionsScope::All),
            limit: Some(50),
            cursor: Some("next".into()),
        };
        let ticket = desktop
            .reads_enqueue(ReadRequest::ListSessions(query))
            .unwrap();
        let (id, payload) = sent(&desktop.reads_pump().sends);
        assert_eq!(payload["method"], "list_sessions");
        assert_eq!(
            payload["params"],
            json!({"scope": "all", "limit": 50, "cursor": "next"})
        );
        assert!(desktop.reads_take(ticket).is_none(), "not answered yet");
        let applied = desktop.reads_apply(&ok(
            &id,
            json!({"sessions": [{"id": "s1", "modified": 1, "messageCount": 2, "firstMessage": "hi", "project": "w", "current": true}]}),
        ));
        assert!(!applied.switched);
        let Some(ReadOutcome::Sessions(list)) = desktop.reads_take(ticket) else {
            panic!("expected the page");
        };
        assert_eq!(list.sessions[0].id, "s1");
        assert_eq!(list.next_cursor, None);
        assert!(desktop.reads_take(ticket).is_none(), "taken once");
    }

    #[test]
    fn a_switch_retries_behind_another_switch_then_replaces_the_session() {
        let mut desktop = main_shell();
        let ticket = desktop
            .reads_enqueue(ReadRequest::SwitchSession {
                id: "s2".into(),
                scope: Some(SessionsScope::Project),
            })
            .unwrap();
        let (first, payload) = sent(&desktop.reads_pump().sends);
        assert_eq!(payload["params"], json!({"id": "s2", "scope": "project"}));

        let busy = desktop.reads_apply(&error(&first, "session_switching"));
        assert!(busy.retry, "nothing was done: sent again shortly");
        assert!(desktop.reads_take(ticket).is_none());
        assert!(
            desktop.reads_pump().sends.is_empty(),
            "held until the delay passes"
        );
        desktop.reads_release();
        let (second, _) = sent(&desktop.reads_pump().sends);
        assert_ne!(first, second);

        let applied = desktop.reads_apply(&ok(&second, json!({"id": "s2", "changed": true})));
        assert!(applied.switched, "the tab forgets and re-reads its session");
        assert!(matches!(
            desktop.reads_take(ticket),
            Some(ReadOutcome::Switched(result)) if result.changed && !result.cancelled
        ));
    }

    #[test]
    fn retries_are_bounded() {
        let mut desktop = main_shell();
        let ticket = desktop
            .reads_enqueue(ReadRequest::UpdateStatus { refresh: true })
            .unwrap();
        for _ in 0..3 {
            let (id, _) = sent(&desktop.reads_pump().sends);
            assert!(desktop.reads_apply(&error(&id, "session_switching")).retry);
            desktop.reads_release();
        }
        let (id, _) = sent(&desktop.reads_pump().sends);
        assert!(!desktop.reads_apply(&error(&id, "session_switching")).retry);
        assert!(matches!(
            desktop.reads_take(ticket),
            Some(ReadOutcome::Failed(failure)) if failure.code() == Some(DesktopErrorCode::SessionSwitching)
        ));
    }

    #[test]
    fn a_cancelled_switch_keeps_the_session() {
        let mut desktop = main_shell();
        let ticket = desktop
            .reads_enqueue(ReadRequest::SwitchSession {
                id: "s2".into(),
                scope: None,
            })
            .unwrap();
        let (id, payload) = sent(&desktop.reads_pump().sends);
        assert_eq!(payload["params"], json!({"id": "s2"}));
        let applied = desktop.reads_apply(&ok(
            &id,
            json!({"id": "s2", "changed": false, "cancelled": true}),
        ));
        assert!(!applied.switched);
        assert!(matches!(
            desktop.reads_take(ticket),
            Some(ReadOutcome::Switched(result)) if result.cancelled
        ));
    }

    #[test]
    fn refusals_are_told_apart() {
        let mut desktop = main_shell();
        let not_found = desktop
            .reads_enqueue(ReadRequest::SwitchSession {
                id: "gone".into(),
                scope: None,
            })
            .unwrap();
        let stats = desktop
            .reads_enqueue(ReadRequest::UsageStats {
                range: StatsRange::Week,
                scope: StatsScope::All,
            })
            .unwrap();
        let (id, _) = sent(&desktop.reads_pump().sends);
        let _ = desktop.reads_apply(&error(&id, "not_found"));
        let (id, payload) = sent(&desktop.reads_pump().sends);
        assert_eq!(payload["params"], json!({"range": "7d", "scope": "all"}));
        let _ = desktop.reads_apply(&error(&id, "unsupported_method"));
        assert!(matches!(
            desktop.reads_take(not_found),
            Some(ReadOutcome::Failed(failure)) if failure.code() == Some(DesktopErrorCode::NotFound)
        ));
        assert_eq!(
            desktop.reads_take(stats),
            Some(ReadOutcome::Failed(ReadFailure::Unsupported))
        );
    }

    #[test]
    fn timeouts_and_resets_answer_every_ticket() {
        let mut desktop = main_shell();
        let first = desktop
            .reads_enqueue(ReadRequest::UpdateStatus { refresh: false })
            .unwrap();
        let queued = desktop
            .reads_enqueue(ReadRequest::UpdateStatus { refresh: true })
            .unwrap();
        let sends = desktop.reads_pump().sends;
        let (id, payload) = sent(&sends);
        assert_eq!(payload["params"], json!({}));
        let _ = desktop.reads_expire(sends[0].generation);
        assert_eq!(
            desktop.reads_take(first),
            Some(ReadOutcome::Failed(ReadFailure::Timeout))
        );
        // A late answer to the dropped request is not an outcome.
        let _ = desktop.reads_apply(&ok(&id, json!({})));
        desktop.reset();
        assert_eq!(
            desktop.reads_take(queued),
            Some(ReadOutcome::Failed(ReadFailure::Dropped))
        );
    }

    #[test]
    fn reads_wait_for_the_shared_slot() {
        let mut desktop = main_shell();
        // Another slice's request holds the one slot.
        let other = desktop.session().issue(DesktopMethod::GetSnapshot).unwrap();
        let ticket = desktop
            .reads_enqueue(ReadRequest::UpdateStatus { refresh: false })
            .unwrap();
        assert!(desktop.reads_pump().sends.is_empty());
        let _ = desktop.ui(&ok(
            other.request_id(),
            json!({"schema": SCHEMA, "sequence": 0, "state": {}}),
        ));
        let (_, payload) = sent(&desktop.reads_pump().sends);
        assert_eq!(payload["method"], "get_update_status");
        assert!(desktop.reads_take(ticket).is_none());
    }

    /// Gentle Shell 5.0.0 advertises the background-subagents methods: the
    /// policy is read, and a change takes the two calls, the token only in
    /// the second. 4.9.0 does not: the screen keeps the files and the
    /// command, and nothing is sent.
    #[test]
    fn background_subagents_are_read_and_changed_in_two_calls() {
        let mut old = main_shell();
        assert_eq!(
            old.reads_support(DesktopMethod::GetBackgroundSubagents),
            ReadSupport::Legacy
        );
        assert_eq!(
            old.reads_enqueue(ReadRequest::BackgroundSubagents),
            Err(ReadFailure::NotOffered)
        );

        let mut methods = RELEASE_METHODS.to_vec();
        methods.extend(["get_background_subagents", "set_background_subagents"]);
        let mut desktop = probed(&methods);
        let state = json!({ "effective": "off", "source": "default", "malformed": false,
                            "projectFile": "absent", "globalFile": "absent", "capability": "ready" });
        let read = desktop
            .reads_enqueue(ReadRequest::BackgroundSubagents)
            .unwrap();
        let (id, payload) = sent(&desktop.reads_pump().sends);
        assert_eq!(
            (payload["method"].as_str(), &payload["params"]),
            (Some("get_background_subagents"), &json!({}))
        );
        let _ = desktop.reads_apply(&ok(&id, state.clone()));
        let Some(ReadOutcome::BackgroundSubagents(policy)) = desktop.reads_take(read) else {
            panic!("the policy");
        };
        assert_eq!(policy.effective, BackgroundPolicy::Off);

        let ask = desktop
            .reads_enqueue(ReadRequest::SetBackgroundSubagents {
                policy: BackgroundPolicy::On,
                confirmation_token: None,
            })
            .unwrap();
        let (id, payload) = sent(&desktop.reads_pump().sends);
        assert_eq!(payload["params"], json!({ "policy": "on" }));
        let _ = desktop.reads_apply(&ok(
            &id,
            json!({ "status": "confirmation_required", "policy": "on", "state": state,
                    "confirmation": { "title": "Turn background subagents on machine-wide?",
                                      "message": "…", "token": "t1" } }),
        ));
        let Some(ReadOutcome::BackgroundSubagentsSet(asked)) = desktop.reads_take(ask) else {
            panic!("the confirmation");
        };
        let SetBackgroundSubagentsOutcome::ConfirmationRequired(confirmation) = asked.outcome
        else {
            panic!("nothing written without a token");
        };
        let confirm = desktop
            .reads_enqueue(ReadRequest::SetBackgroundSubagents {
                policy: BackgroundPolicy::On,
                confirmation_token: Some(confirmation.token),
            })
            .unwrap();
        let (id, payload) = sent(&desktop.reads_pump().sends);
        assert_eq!(
            payload["params"],
            json!({ "policy": "on", "confirmationToken": "t1" })
        );
        let mut on = state;
        on["effective"] = json!("on");
        on["source"] = json!("global");
        on["globalFile"] = json!("present");
        let _ = desktop.reads_apply(&ok(
            &id,
            json!({ "status": "applied", "policy": "on", "changed": true, "state": on }),
        ));
        let Some(ReadOutcome::BackgroundSubagentsSet(applied)) = desktop.reads_take(confirm) else {
            panic!("applied");
        };
        assert_eq!(
            applied.outcome,
            SetBackgroundSubagentsOutcome::Applied { changed: true }
        );
    }

    #[test]
    fn agent_definitions_are_asked_only_of_a_shell_that_offers_them() {
        // Gentle Shell 4.9.0 (the reads of `main`, no definitions): the
        // Profiles screen keeps reading the agent files.
        let mut older = main_shell();
        assert_eq!(
            older.reads_support(DesktopMethod::ListAgentDefinitions),
            ReadSupport::Legacy
        );
        assert_eq!(
            older.reads_enqueue(ReadRequest::AgentDefinitions),
            Err(ReadFailure::NotOffered)
        );

        let mut methods = RELEASE_METHODS.to_vec();
        methods.push("list_agent_definitions");
        let mut desktop = probed(&methods);
        assert_eq!(
            desktop.reads_support(DesktopMethod::ListAgentDefinitions),
            ReadSupport::Rpc
        );
        let ticket = desktop
            .reads_enqueue(ReadRequest::AgentDefinitions)
            .unwrap();
        let sends = desktop.reads_pump().sends;
        let (id, payload) = sent(&sends);
        assert_eq!(payload["method"], "list_agent_definitions");
        assert_eq!(payload["params"], json!({}));
        let _ = desktop.reads_apply(&ok(
            &id,
            json!({
                "agents": [
                    { "name": "explore", "description": "Global explorer.", "scope": "global", "model": "openai-codex/gpt-5.6-terra", "thinking": "high", "mode": null, "tools": ["read", "grep"], "toolsTruncated": false }
                ],
                "truncated": false,
                "problems": { "invalid": 0, "notices": 0 },
                "subagentDefaults": { "model": null, "thinking": null, "mode": "task" },
                "defaultOrchestrator": { "status": "set", "model": "openai-codex/gpt-5.6-terra", "thinking": "medium" }
            }),
        ));
        let Some(ReadOutcome::AgentDefinitions(definitions)) = desktop.reads_take(ticket) else {
            panic!("the definitions answer the ticket")
        };
        assert_eq!(definitions.agents[0].name, "explore");

        // A documented refusal is the screen's cue to read the files.
        let ticket = desktop
            .reads_enqueue(ReadRequest::AgentDefinitions)
            .unwrap();
        let (id, _) = sent(&desktop.reads_pump().sends);
        let _ = desktop.reads_apply(&error(&id, "agent_definitions_unavailable"));
        assert!(matches!(
            desktop.reads_take(ticket),
            Some(ReadOutcome::Failed(ReadFailure::Refused {
                code: Some(DesktopErrorCode::AgentDefinitionsUnavailable),
                ..
            }))
        ));
    }

    #[test]
    fn status_is_read_only_from_a_shell_that_advertises_it() {
        // A Shell with every other read but no `get_status` (4.9.0): the
        // Configuration screen shows no status, and nothing is sent.
        let mut methods = RELEASE_METHODS.to_vec();
        methods.extend(["get_usage_stats", "get_update_status"]);
        let mut older = probed(&methods);
        assert_eq!(
            older.reads_support(DesktopMethod::GetStatus),
            ReadSupport::Legacy
        );
        assert_eq!(
            older.reads_enqueue(ReadRequest::Status),
            Err(ReadFailure::NotOffered)
        );
        assert!(older.session().pending().is_none());

        let mut desktop = main_shell();
        assert_eq!(
            desktop.reads_support(DesktopMethod::GetStatus),
            ReadSupport::Rpc
        );
        let ticket = desktop.reads_enqueue(ReadRequest::Status).unwrap();
        let (id, payload) = sent(&desktop.reads_pump().sends);
        assert_eq!(payload["method"], "get_status");
        assert_eq!(payload["params"], json!({}));
        let _ = desktop.reads_apply(&ok(
            &id,
            json!({
                "package": "active", "odd": "active", "persona": "gentleman",
                "devBinary": { "state": "inactive", "sha256Prefix": null },
                "assets": {
                    "delegation": { "stale": 0, "userOverrides": 1, "activeAgentOverrides": 0 },
                    "review": { "stale": 0, "userOverrides": 0, "activeAgentOverrides": 0 }
                },
                "modelConfig": { "global": "present", "routing": "valid" },
                "severity": "info"
            }),
        ));
        let Some(ReadOutcome::Status(status)) = desktop.reads_take(ticket) else {
            panic!("expected the status");
        };
        assert_eq!(status.persona, "gentleman");

        // `status_unavailable` is a refusal the screen shows nothing for.
        let ticket = desktop.reads_enqueue(ReadRequest::Status).unwrap();
        let (id, _) = sent(&desktop.reads_pump().sends);
        let _ = desktop.reads_apply(&error(&id, "status_unavailable"));
        assert_eq!(
            desktop
                .reads_take(ticket)
                .and_then(|outcome| match outcome {
                    ReadOutcome::Failed(failure) => failure.code(),
                    _ => None,
                }),
            Some(DesktopErrorCode::StatusUnavailable)
        );
    }
}
