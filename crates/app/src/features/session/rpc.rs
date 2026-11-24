//! Requests a tab makes of its backend: the request/response plumbing, the
//! state it reads back, and the commands the UI and window run.

use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::Result;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use pi_rpc::PiProcess;
use pi_rpc::desktop_rpc::{ConfigStore, ConfigWrite, DesktopMethod, ReviewScope};
use serde_json::{Value, json};

use super::state::desktop::config::Applied as ConfigApplied;
use super::state::desktop::design::Applied as DesignApplied;
use super::state::desktop::output::Applied as OutputApplied;
use super::state::desktop::permission::Applied as PermissionApplied;
use super::state::desktop::review::Applied as ReviewApplied;
use super::state::desktop::subscription::Applied as SubscriptionApplied;
use super::state::desktop::supervision::Applied as SupervisionApplied;
use super::state::desktop::{self, Applied as DesktopApplied, Send as DesktopSend};
use super::state::{BackendStatus, ModelInfo};
use super::{SessionEvent, SessionView, attachments, builtins};
use crate::features::profiles::{
    ConfigChange, ConfigFailure, ConfigOutcome, ConfigSupport, ConfigTicket, ProfileApply,
};
use gentle_core::transcript::NoticeLevel;

/// How long a Desktop request may stay unanswered before the tab drops it.
/// The extension answers from the same session, so this only fires when
/// something is genuinely wrong.
const DESKTOP_TIMEOUT: Duration = Duration::from_secs(10);

/// The reads that may legitimately take longer: a listing or stats scan
/// reads every session file in scope, a switch replaces the session, an
/// update refresh is bounded by the server's own 15-second deadline, and a
/// status read audits every managed asset file.
const DESKTOP_SLOW_TIMEOUT: Duration = Duration::from_secs(30);

fn desktop_timeout(method: DesktopMethod) -> Duration {
    match method {
        DesktopMethod::ListSessions
        | DesktopMethod::SwitchSession
        | DesktopMethod::GetUsageStats
        | DesktopMethod::GetUpdateStatus
        | DesktopMethod::GetStatus => DESKTOP_SLOW_TIMEOUT,
        _ => DESKTOP_TIMEOUT,
    }
}

/// Whether Pi's `get_commands` answer lists the Gentle Desktop extension. A
/// failed request lists nothing, so its prompt is never sent.
fn discovers_desktop(result: &Result<Value>) -> bool {
    result
        .as_ref()
        .is_ok_and(|data| desktop::registers_desktop(&parse_commands(data)))
}

/// Pi's `get_commands` data as `(name, description)`, skipping anything that is
/// not a named command.
fn parse_commands(data: &Value) -> Vec<(String, String)> {
    data["commands"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|command| {
            Some((
                command["name"].as_str()?.to_string(),
                command["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ))
        })
        .collect()
}

impl SessionView {
    pub(super) fn process(&self) -> Option<Arc<PiProcess>> {
        match &self.backend.status {
            BackendStatus::Ready(process) => Some(process.clone()),
            _ => None,
        }
    }

    /// Send `command`; `then` runs on the UI thread with the response data.
    /// A response from a backend since replaced (restart, folder change) or
    /// gone is dropped: it belongs to a conversation no longer shown, and its
    /// "exited before answering" is the shutdown the app asked for.
    pub(super) fn request(
        &self,
        command: Value,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Result<Value>, &mut Context<Self>) + 'static,
    ) {
        let Some(process) = self.process() else {
            return;
        };
        // The task holds the response and a weak handle, not the process:
        // closing the tab still stops the backend with a quiet request (`!`
        // command, compaction) in flight.
        let response = process.request(command);
        let sent_to = Arc::downgrade(&process);
        cx.spawn(async move |this, cx| {
            let result = response.await.map(|r| r.data);
            let _ = this.update(cx, |this, cx| {
                let current = matches!(&this.backend.status,
                    BackendStatus::Ready(p) if Weak::as_ptr(&sent_to) == Arc::as_ptr(p));
                if !current {
                    return;
                }
                then(this, result, cx);
                this.sync_view(cx);
                this.sync_chrome(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The response data, or `None` with the error shown in the conversation.
    pub(super) fn report(&mut self, result: Result<Value>) -> Option<Value> {
        match result {
            Ok(data) => Some(data),
            Err(error) => {
                self.transcript
                    .notice(NoticeLevel::Error, format!("{error:#}"));
                None
            }
        }
    }

    /// Act on a Desktop decision: say what failed through the tab's report
    /// path, then send whatever the channel asked for.
    pub(super) fn apply_desktop(&mut self, applied: DesktopApplied, cx: &mut Context<Self>) {
        if let Some(report) = applied.report {
            self.transcript
                .notice(NoticeLevel::Warning, report.message());
            cx.notify();
        }
        self.send_desktop(applied.sends, cx);
    }

    /// Act on a supervision decision: say what failed through the tab's report
    /// path, then send whatever the subagent/job channel asked for.
    pub(super) fn apply_supervision(
        &mut self,
        applied: SupervisionApplied,
        cx: &mut Context<Self>,
    ) {
        if let Some(report) = applied.report {
            self.transcript
                .notice(NoticeLevel::Warning, report.message());
            cx.notify();
        }
        self.send_desktop(applied.sends, cx);
    }

    /// Act on a subscription decision: say what failed through the tab's report
    /// path (legacy signals are the Usage screen's concern, not a session
    /// error), then send whatever the usage slice asked for.
    pub(super) fn apply_subscription(
        &mut self,
        applied: SubscriptionApplied,
        cx: &mut Context<Self>,
    ) {
        if let Some(report) = &applied.report
            && !report.is_legacy()
        {
            self.transcript
                .notice(NoticeLevel::Warning, report.message());
            cx.notify();
        }
        self.send_desktop(applied.sends, cx);
    }

    /// Act on a design decision: say what failed (old Shells without the
    /// method stay silent, their legacy canvas path still works), send what
    /// the slice asked for, and hand the canvas notice to the browser flow.
    /// The report never carries the canvas URL; only the notice does.
    pub(super) fn apply_design(&mut self, applied: DesignApplied, cx: &mut Context<Self>) {
        if let Some(report) = &applied.report
            && !report.is_legacy()
        {
            self.transcript
                .notice(NoticeLevel::Warning, report.message());
            cx.notify();
        }
        self.send_desktop(applied.sends, cx);
        // A gap seen while the design request was in flight is due now that it
        // settled or rolled back: one snapshot, and the canvas re-seeds behind.
        let resync = self.desktop.flush_resync();
        self.send_desktop(resync, cx);
        if let Some(canvas) = applied.notice {
            cx.emit(SessionEvent::DesignCanvas(canvas));
        }
    }

    /// Act on a config decision: say what failed (old Shells without the
    /// methods stay silent, the Profiles screen keeps its direct-file path),
    /// send what the slice asked for, and wake the screen, which takes its
    /// outcomes and changes from the session. A profile Gentle Shell applied
    /// may have switched this session's orchestrator, so its model is re-read.
    pub(super) fn apply_config(&mut self, applied: ConfigApplied, cx: &mut Context<Self>) {
        if let Some(report) = &applied.report
            && !report.is_legacy()
        {
            self.transcript
                .notice(NoticeLevel::Warning, report.message());
        }
        if applied.profile_applied {
            self.refresh_model(cx);
        }
        self.send_desktop(applied.sends, cx);
        // A gap seen while the config request was in flight is due now that it
        // settled or rolled back.
        let resync = self.desktop.flush_resync();
        self.send_desktop(resync, cx);
        cx.notify();
    }

    /// Send the next queued config request once the channel is free, then
    /// collect the default model and pump the reads queue. Both queues share one slot.
    pub(super) fn sync_config(&mut self, cx: &mut Context<Self>) {
        let applied = self.desktop.config_pump();
        if !applied.sends.is_empty() || applied.report.is_some() {
            self.apply_config(applied, cx);
        }
        if let Some(default) = self.desktop.sync_model_default() {
            self.set_model_default(default, cx);
        }
        self.sync_reads(cx);
    }

    /// Send what an output read decided. Its failures show where the output
    /// or thread would be, never in the conversation.
    pub(super) fn apply_output(&mut self, applied: OutputApplied, cx: &mut Context<Self>) {
        self.send_desktop(applied.sends, cx);
        // A gap seen while the read was in flight is due now that it settled.
        let resync = self.desktop.flush_resync();
        self.send_desktop(resync, cx);
        cx.notify();
    }

    /// Act on a review-mode decision: say what failed, send what the slice
    /// asked for, and redraw the chip and its confirmation.
    pub(super) fn apply_review(&mut self, applied: ReviewApplied, cx: &mut Context<Self>) {
        if let Some(report) = applied.report {
            self.transcript
                .notice(NoticeLevel::Warning, report.message());
        }
        self.send_desktop(applied.sends, cx);
        // A gap seen while the review request was in flight is due now that
        // it settled or rolled back.
        let resync = self.desktop.flush_resync();
        self.send_desktop(resync, cx);
        cx.notify();
    }

    /// Send an owed job output or thread read once the channel is free.
    pub(super) fn sync_output(&mut self, cx: &mut Context<Self>) {
        let applied = self.desktop.output_pump();
        if !applied.sends.is_empty() {
            self.apply_output(applied, cx);
        }
    }

    /// Bring what the session feed's sections drive up to date: the todo
    /// island (the feed's list wins over tool results while it reports one)
    /// and the context meter. The phase, a pending question and Pi's
    /// built-in commands are read from the sections where they are drawn.
    #[cfg(test)]
    pub(super) fn sync_sections(&mut self, cx: &mut Context<Self>) {
        self.sync_sections_if_context(true, cx);
    }

    pub(super) fn sync_sections_if_context(
        &mut self,
        context_changed: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.desktop.take_sections_changed() {
            return;
        }
        let sections = self.desktop.sections();
        let todo = sections.todo();
        let context = sections.context().map(|context| {
            context.map(|context| (context.percent, context.tokens, context.window))
        });
        let question = sections.question_pending();
        if let Some(tasks) = todo {
            self.todo.apply(tasks);
        }
        if context_changed && let Some(context) = context {
            let (percent, tokens, window) = context.unwrap_or((None, None, None));
            self.usage.context_window = window;
            if self.usage.context_estimated && percent.is_none() && tokens.is_none() {
                let estimated_percent = self
                    .usage
                    .context_tokens
                    .zip(window.filter(|w| *w > 0))
                    .map(|(tokens, window)| tokens as f64 * 100.0 / window as f64);
                self.set_context_percent(estimated_percent, cx);
            } else {
                self.usage.context_revision = self.usage.context_revision.wrapping_add(1);
                self.usage.context_estimated = false;
                self.usage.context_tokens = tokens;
                self.set_context_percent(percent, cx);
            }
        }
        if question {
            // Mention the question once its grace ran out, unless the app
            // shows it as a dialog by then.
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(desktop::sections::QUESTION_GRACE)
                    .await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        cx.notify();
    }

    /// Send what the review slice owes once the channel is free.
    pub(super) fn sync_review(&mut self, cx: &mut Context<Self>) {
        let applied = self.desktop.review_pump();
        if !applied.sends.is_empty() || applied.report.is_some() {
            self.apply_review(applied, cx);
        }
    }

    /// Ask Gentle Shell to turn reviews on or off for `scope`. Nothing is
    /// written yet: it answers the confirmation the human sees first.
    pub(super) fn request_review_mode(
        &mut self,
        enabled: bool,
        scope: ReviewScope,
        cx: &mut Context<Self>,
    ) {
        if self.desktop.review_request(enabled, scope) {
            self.sync_review(cx);
        }
        cx.notify();
    }

    /// The human approved the confirmation: resend with its token.
    pub(super) fn confirm_review_mode(&mut self, cx: &mut Context<Self>) {
        if self.desktop.review_confirm() {
            self.sync_review(cx);
        }
        cx.notify();
    }

    /// The human declined the confirmation: nothing is resent.
    pub(super) fn dismiss_review_mode(&mut self, cx: &mut Context<Self>) {
        self.desktop.review_dismiss();
        cx.notify();
    }

    /// Read the switch again: a change made in the TUI emits no event.
    pub(super) fn refresh_review_mode(&mut self, cx: &mut Context<Self>) {
        self.desktop.review_refresh();
        self.sync_review(cx);
    }
    /// Whether the Profiles screen edits this tab's config over Desktop RPC,
    /// waits for the probe, or keeps its direct-file path. No backend to ask
    /// (it failed or exited) is the direct-file path too: there is no writer
    /// to race.
    pub(crate) fn config_support(&self) -> ConfigSupport {
        match self.backend.status {
            BackendStatus::Failed(_) | BackendStatus::Exited(_) => ConfigSupport::Legacy,
            BackendStatus::Starting | BackendStatus::Ready(_) => self.desktop.config_support(),
        }
    }

    /// Read one config store over the channel; the answer arrives as an
    /// outcome for the returned ticket.
    pub(crate) fn config_get(
        &mut self,
        store: ConfigStore,
        cx: &mut Context<Self>,
    ) -> Result<ConfigTicket, ConfigFailure> {
        let ticket = self.desktop.config_get(store)?;
        self.sync_config(cx);
        Ok(ticket)
    }

    /// Replace one config store (or pin layer) guarded by the `writeRevision`
    /// last read; the answer arrives as an outcome for the returned ticket.
    pub(crate) fn config_set(
        &mut self,
        write: ConfigWrite,
        expected_revision: String,
        cx: &mut Context<Self>,
    ) -> Result<ConfigTicket, ConfigFailure> {
        let ticket = self.desktop.config_set(write, expected_revision)?;
        self.sync_config(cx);
        Ok(ticket)
    }

    /// Whether this tab's Gentle Shell applies saved profiles itself
    /// (`apply_profile`); otherwise the Profiles screen applies them on its
    /// own, as before.
    pub(crate) fn profile_apply_supported(&self) -> bool {
        matches!(self.backend.status, BackendStatus::Ready(_))
            && self.desktop.profile_apply_supported()
    }

    /// Ask Gentle Shell to apply a saved profile; the answer arrives as an
    /// outcome for the returned ticket.
    pub(crate) fn config_apply_profile(
        &mut self,
        request: ProfileApply,
        cx: &mut Context<Self>,
    ) -> Result<ConfigTicket, ConfigFailure> {
        let ticket = self.desktop.config_apply_profile(request)?;
        self.sync_config(cx);
        Ok(ticket)
    }

    /// The profile this session is bound to: Gentle Shell's own
    /// `get_profile_binding` answer (an explicit binding) where it offers
    /// that read; otherwise what its `apply_profile` reported, or what the
    /// Profiles screen applied on an older Shell. Forgotten with the session
    /// (new, resumed, or a replaced backend).
    pub(crate) fn session_profile(&self) -> Option<&str> {
        self.desktop.session_profile()
    }

    /// Record a session binding made or cleared outside `apply_profile`.
    pub(crate) fn set_session_profile(&mut self, profile: Option<String>, cx: &mut Context<Self>) {
        self.desktop.set_session_profile(profile);
        cx.notify();
    }

    /// Whether this tab's Gentle Shell clears the session binding itself
    /// (`clear_profile_binding`); otherwise the Profiles screen sends
    /// `/gentle:profiles clear` as before.
    pub(crate) fn binding_clear_supported(&self) -> bool {
        matches!(self.backend.status, BackendStatus::Ready(_))
            && self.desktop.binding_clear_supported()
    }

    /// Ask Gentle Shell to clear this session's binding; the answer arrives
    /// as a [`ConfigOutcome::Cleared`] for the returned ticket.
    pub(crate) fn clear_profile_binding(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<ConfigTicket, ConfigFailure> {
        let ticket = self.desktop.config_clear_binding()?;
        self.sync_config(cx);
        Ok(ticket)
    }

    /// The answers to config tickets since the last call (the Profiles
    /// screen's to take).
    pub(crate) fn take_config_outcomes(&mut self) -> Vec<(ConfigTicket, ConfigOutcome)> {
        self.desktop.take_config_outcomes()
    }

    /// The stores that changed (or may have) since the last call.
    pub(crate) fn take_config_changes(&mut self) -> Vec<ConfigChange> {
        self.desktop.take_config_changes()
    }

    /// The Usage screen's snapshot of this session's subscription usage. Pure
    /// read: the screen decides how to render, never this session.
    pub(crate) fn subscription_snapshot(&self) -> super::state::desktop::subscription::Snapshot {
        self.desktop.subscription_snapshot()
    }

    /// Ask the server for the whole-account subscription usage once (the Usage
    /// screen opened). Idempotent: a later call is a no-op.
    pub(crate) fn subscription_seed(&mut self, cx: &mut Context<Self>) {
        let applied = self.desktop.subscription_seed();
        self.send_desktop(applied.sends, cx);
    }

    /// Refresh the whole-account subscription usage (the Usage screen's
    /// refresh button).
    pub(crate) fn subscription_refresh(&mut self, cx: &mut Context<Self>) {
        let applied = self.desktop.subscription_refresh();
        self.send_desktop(applied.sends, cx);
    }

    /// Reset the Gentle Desktop channel when the conversation changes on the
    /// same backend (new/open session). It forgets the old session's pending
    /// request and event baseline (without rewinding request ids) and returns
    /// the channel to Idle, so the `refresh_state` that follows re-probes
    /// capabilities and the supervision lists re-seed exactly once behind it.
    /// The permission slice keeps its confirmed mode; its capabilities are
    /// refreshed by the same re-probe. The subscription and design slices are
    /// reset with the channel; the design canvas re-seeds once behind the new
    /// probe.
    pub(super) fn reset_desktop_channel(&mut self) {
        self.desktop.reset();
    }

    /// Cancel one running background job over `cancel_job`. There is no job
    /// start method and no restart: only the running job the user picked is
    /// affected, and the server's answer is the truth.
    pub(super) fn cancel_running_job(&mut self, id: String, cx: &mut Context<Self>) {
        let applied = self.desktop.cancel_job(id);
        self.apply_supervision(applied, cx);
    }

    /// Act on a permission decision: say what failed or warn about a default
    /// that could not be saved, then send whatever the permission slice asked
    /// for and reflect the server-confirmed mode into the picker.
    pub(super) fn apply_permission(&mut self, applied: PermissionApplied, cx: &mut Context<Self>) {
        if let Some(report) = applied.report {
            self.transcript
                .notice(NoticeLevel::Warning, report.message());
            cx.notify();
        }
        self.send_desktop(applied.sends, cx);
        // A gap seen while this request was in flight is due now that it
        // settled or rolled back: ask the channel for a fresh snapshot once.
        let resync = self.desktop.flush_resync();
        self.send_desktop(resync, cx);
        self.sync_permission_ui(cx);
    }

    /// The server's confirmed permission state is truth: move the picker's
    /// live and default modes only from it, never optimistically from the
    /// request the user just picked.
    fn sync_permission_ui(&mut self, cx: &mut Context<Self>) {
        let confirmed = self.permission.confirmed();
        let mut changed = false;
        if let Some(mode) = confirmed.mode.map(super::from_desktop_mode)
            && self.mode != Some(mode)
        {
            self.mode = Some(mode);
            changed = true;
        }
        if let Some(default_mode) = confirmed.default_mode.map(super::from_desktop_mode)
            && self.default_mode != Some(default_mode)
        {
            self.default_mode = Some(default_mode);
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// Hand Gentle Desktop requests to the backend. Each one is a `prompt` the
    /// transport answers with a disposition, and the domain answer arrives
    /// later as the Desktop widget; a guarded timer drops one that never comes.
    pub(super) fn send_desktop(&mut self, sends: Vec<DesktopSend>, cx: &mut Context<Self>) {
        for send in sends {
            self.send_one_desktop(send, cx);
        }
    }

    fn send_one_desktop(&mut self, send: DesktopSend, cx: &mut Context<Self>) {
        let generation = send.generation;
        let method = send.request.method();
        if self.process().is_none() {
            // No backend to answer: drop it like a timeout, silently, in the
            // slice that owns the request.
            self.expire_desktop(generation, method, cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(desktop_timeout(method))
                .await;
            let _ = this.update(cx, |this, cx| {
                // Only its own request: a newer one has another generation.
                this.expire_desktop(generation, method, cx);
            });
        })
        .detach();
        self.request(
            send.request.prompt_command(),
            cx,
            move |this, result, cx| {
                // `handled` only means the prompt was accepted; nothing but that
                // keeps the request waiting for its widget record.
                let handled = result.is_ok_and(|data| data["disposition"] == "handled");
                this.outer_desktop(generation, method, handled, cx);
            },
        );
    }

    /// A pending timeout fired: hand it to the slice that issued the request,
    /// so a probe timer never abandons a permission request and vice versa.
    fn expire_desktop(&mut self, generation: u64, method: DesktopMethod, cx: &mut Context<Self>) {
        match method {
            // The core channel, and `get_changes`/`get_change_diff`, which
            // the app no longer sends: a timer for either is the channel's.
            DesktopMethod::Capabilities
            | DesktopMethod::GetSnapshot
            | DesktopMethod::GetChanges
            | DesktopMethod::GetChangeDiff => {
                let sends = self.desktop.expire(generation);
                self.send_desktop(sends, cx);
            }
            DesktopMethod::GetPermissionMode | DesktopMethod::SetPermissionMode => {
                let applied = self.permission.expire(self.desktop.session(), generation);
                self.apply_permission(applied, cx);
            }
            DesktopMethod::ListSubagents
            | DesktopMethod::ListJobs
            | DesktopMethod::CancelSubagent
            | DesktopMethod::CancelJob => {
                let applied = self.desktop.supervision_expire(generation);
                self.apply_supervision(applied, cx);
            }
            DesktopMethod::GetSubscriptionUsage | DesktopMethod::RefreshSubscriptionUsage => {
                let applied = self.desktop.subscription_expire(generation);
                self.apply_subscription(applied, cx);
            }
            DesktopMethod::GetDesignCanvas => {
                let applied = self.desktop.design_expire(generation);
                self.apply_design(applied, cx);
            }
            DesktopMethod::GetConfig
            | DesktopMethod::SetConfig
            | DesktopMethod::ApplyProfile
            | DesktopMethod::GetProfileBinding
            | DesktopMethod::ClearProfileBinding => {
                let applied = self.desktop.config_expire(generation);
                self.apply_config(applied, cx);
            }
            DesktopMethod::GetReviewMode | DesktopMethod::SetReviewMode => {
                let applied = self.desktop.review_expire(generation);
                self.apply_review(applied, cx);
            }
            DesktopMethod::GetJobOutput | DesktopMethod::GetSubagentThread => {
                let applied = self.desktop.output_expire(generation);
                self.apply_output(applied, cx);
            }
            DesktopMethod::ListSessions
            | DesktopMethod::SwitchSession
            | DesktopMethod::GetUsageStats
            | DesktopMethod::GetUpdateStatus
            | DesktopMethod::ListAgentDefinitions
            | DesktopMethod::GetStatus
            | DesktopMethod::GetBackgroundSubagents
            | DesktopMethod::SetBackgroundSubagents
            | DesktopMethod::GetAuthStatus
            | DesktopMethod::ReloadCredentials => {
                let applied = self.desktop.reads_expire(generation);
                self.apply_reads(applied, cx);
            }
        }
        // Whichever request was dropped, the shared slot may be free for the
        // next queued config request or screen read, review change or owed
        // output read.
        self.sync_config(cx);
        self.sync_review(cx);
        self.sync_output(cx);
    }

    /// Pi answered the outer prompt: hand the disposition to the slice that
    /// issued the request.
    fn outer_desktop(
        &mut self,
        generation: u64,
        method: DesktopMethod,
        handled: bool,
        cx: &mut Context<Self>,
    ) {
        match method {
            // As in `expire_desktop`: never sent, so the channel's.
            DesktopMethod::Capabilities
            | DesktopMethod::GetSnapshot
            | DesktopMethod::GetChanges
            | DesktopMethod::GetChangeDiff => {
                let applied = self.desktop.outer(generation, handled);
                self.apply_desktop(applied, cx);
            }
            DesktopMethod::GetPermissionMode | DesktopMethod::SetPermissionMode => {
                let applied = self
                    .permission
                    .outer(self.desktop.session(), generation, handled);
                self.apply_permission(applied, cx);
            }
            DesktopMethod::ListSubagents
            | DesktopMethod::ListJobs
            | DesktopMethod::CancelSubagent
            | DesktopMethod::CancelJob => {
                let applied = self.desktop.supervision_outer(generation, handled);
                self.apply_supervision(applied, cx);
            }
            DesktopMethod::GetSubscriptionUsage | DesktopMethod::RefreshSubscriptionUsage => {
                let applied = self.desktop.subscription_outer(generation, handled);
                self.apply_subscription(applied, cx);
            }
            DesktopMethod::GetDesignCanvas => {
                let applied = self.desktop.design_outer(generation, handled);
                self.apply_design(applied, cx);
            }
            DesktopMethod::GetConfig
            | DesktopMethod::SetConfig
            | DesktopMethod::ApplyProfile
            | DesktopMethod::GetProfileBinding
            | DesktopMethod::ClearProfileBinding => {
                let applied = self.desktop.config_outer(generation, handled);
                self.apply_config(applied, cx);
            }
            DesktopMethod::GetReviewMode | DesktopMethod::SetReviewMode => {
                let applied = self.desktop.review_outer(generation, handled);
                self.apply_review(applied, cx);
            }
            DesktopMethod::GetJobOutput | DesktopMethod::GetSubagentThread => {
                let applied = self.desktop.output_outer(generation, handled);
                self.apply_output(applied, cx);
            }
            DesktopMethod::ListSessions
            | DesktopMethod::SwitchSession
            | DesktopMethod::GetUsageStats
            | DesktopMethod::GetUpdateStatus
            | DesktopMethod::ListAgentDefinitions
            | DesktopMethod::GetStatus
            | DesktopMethod::GetBackgroundSubagents
            | DesktopMethod::SetBackgroundSubagents
            | DesktopMethod::GetAuthStatus
            | DesktopMethod::ReloadCredentials => {
                let applied = self.desktop.reads_outer(generation, handled);
                self.apply_reads(applied, cx);
            }
        }
        self.sync_config(cx);
        self.sync_review(cx);
        self.sync_output(cx);
    }

    /// Pi's command list: what the `/` menu offers, and the discovery that
    /// decides whether Gentle Desktop's extension is there at all. The prompt
    /// carrying a Desktop record must never be sent on a guess, because a Shell
    /// without the command would run that JSON as a conversation prompt.
    fn refresh_commands(&mut self, cx: &mut Context<Self>) {
        self.request(json!({"type": "get_commands"}), cx, |this, result, cx| {
            // Read before the result is consumed: a failed listing leaves the
            // channel closed, and `report` puts that error in the conversation.
            let registered = discovers_desktop(&result);
            if let Some(data) = this.report(result) {
                this.commands = parse_commands(&data);
                // Where each skill lives: `SKILL.md`, or its folder.
                this.skill_dirs = data["commands"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["source"] == "skill")
                    .filter_map(|c| {
                        let name = c["name"].as_str()?.strip_prefix("skill:")?;
                        let path = PathBuf::from(c["sourceInfo"]["path"].as_str()?);
                        let dir = if path.ends_with("SKILL.md") {
                            path.parent()?.to_path_buf()
                        } else {
                            path
                        };
                        Some((name.to_string(), dir))
                    })
                    .collect();
                // The window shows which skills an agent has (Architecture docs).
                this.sync_chrome(cx);
                let applied = this.desktop.ready(registered);
                if matches!(applied.report, Some(desktop::Report::MissingExtension)) {
                    // A Shell without Desktop RPC: the picker keeps the
                    // `permission-mode.json` default and the legacy switch.
                    this.permission.without_desktop();
                }
                this.apply_desktop(applied, cx);
            }
        });
    }

    /// Pi's models with usable credentials (`get_available_models`).
    pub(super) fn refresh_models(&mut self, cx: &mut Context<Self>) {
        self.request(
            json!({"type": "get_available_models"}),
            cx,
            |this, result, _| {
                this.models.loaded = true;
                this.models.rev += 1;
                if let Some(data) = this.report(result) {
                    this.models.available = data["models"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(ModelInfo::parse)
                        .collect();
                }
            },
        );
    }

    pub(super) fn refresh_state(&mut self, cx: &mut Context<Self>) {
        self.set_model_default(None, cx);
        self.request(json!({"type": "get_state"}), cx, |this, result, _| {
            if let Some(state) = this.report(result) {
                this.models.current = ModelInfo::parse(&state["model"]);
                this.models.thinking = state["thinkingLevel"].as_str().unwrap_or("off").to_string();
                this.session_name = state["sessionName"].as_str().map(str::to_string);
                this.session_file = state["sessionFile"].as_str().map(PathBuf::from);
                this.usage.auto_compaction = state["autoCompactionEnabled"].as_bool();
            }
        });
        self.refresh_models(cx);
        self.refresh_thinking_levels(cx);
        self.refresh_commands(cx);
        self.request(json!({"type": "get_messages"}), cx, |this, result, _| {
            if let Some(data) = this.report(result) {
                let messages = data["messages"].as_array().cloned().unwrap_or_default();
                if !messages.is_empty() {
                    this.transcript.load_messages(&messages);
                    this.prompt_history
                        .seed(this.transcript.items.iter().filter_map(|item| match item {
                            gentle_core::transcript::Item::User { text, .. } => Some(text.as_str()),
                            _ => None,
                        }));
                    this.markdown.clear();
                    this.sync_todo_from_history();
                }
            }
        });
        self.refresh_stats(cx);
        self.refresh_model_default(cx);
    }

    /// Re-read the model, thinking level and default after a profile apply.
    fn refresh_model(&mut self, cx: &mut Context<Self>) {
        self.request(json!({"type": "get_state"}), cx, |this, result, _| {
            if let Some(state) = this.report(result) {
                this.models.current = ModelInfo::parse(&state["model"]);
                this.models.thinking = state["thinkingLevel"].as_str().unwrap_or("off").to_string();
            }
        });
        self.refresh_thinking_levels(cx);
        self.refresh_stats(cx);
        self.refresh_model_default(cx);
    }

    fn refresh_model_default(&mut self, cx: &mut Context<Self>) {
        self.desktop.request_model_default();
        self.sync_config(cx);
    }

    pub(super) fn set_model_default(&mut self, default: Option<String>, cx: &mut Context<Self>) {
        if self.models.default != default {
            self.models.default = default;
            self.models.rev += 1;
            self.sync_chrome(cx);
            cx.notify();
        }
    }

    /// Pi's `get_session_stats`: the cost (its authority, also with the
    /// session feed) and the context use, which the feed's `context` section
    /// (the same `getContextUsage()` read) keeps current between reads.
    pub(super) fn refresh_stats(&mut self, cx: &mut Context<Self>) {
        self.usage.context_revision = self.usage.context_revision.wrapping_add(1);
        let revision = self.usage.context_revision;
        self.request(
            json!({"type": "get_session_stats"}),
            cx,
            move |this, result, cx| {
                this.apply_stats_at(revision, result, cx);
            },
        );
    }

    #[cfg(test)]
    pub(super) fn apply_stats(&mut self, result: Result<Value>, cx: &mut Context<Self>) {
        self.apply_stats_at(self.usage.context_revision, result, cx);
    }

    pub(super) fn apply_stats_at(
        &mut self,
        revision: u64,
        result: Result<Value>,
        cx: &mut Context<Self>,
    ) {
        self.usage.context_stats_received = true;
        if let Ok(stats) = result {
            self.usage.cost = stats["cost"].as_f64().unwrap_or(0.0);
            let percent = stats["contextUsage"]["percent"].as_f64();
            if revision == self.usage.context_revision
                && (!self.usage.context_estimated || percent.is_some())
            {
                if self.usage.context_estimated {
                    self.usage.context_tokens = None;
                    self.usage.context_estimated = false;
                }
                self.set_context_percent(percent, cx);
            }
        }
    }

    /// The context meter's number, and the alert when it crosses the
    /// threshold.
    fn set_context_percent(&mut self, percent: Option<f64>, cx: &mut Context<Self>) {
        self.usage.context_percent = percent;
        let threshold = crate::features::alerts::context_threshold(cx);
        if self.usage.context_latch.observe(percent, threshold) {
            let active = self.window_active(cx);
            crate::features::alerts::fire(
                gentle_core::notifications::Alert::ContextHigh,
                active,
                cx,
            );
        }
    }

    /// The levels the current model accepts; until they arrive (or if the
    /// backend can't say) the slider keeps the last list.
    fn refresh_thinking_levels(&mut self, cx: &mut Context<Self>) {
        self.request(
            json!({"type": "get_available_thinking_levels"}),
            cx,
            |this, result, _| {
                if let Ok(data) = result {
                    let levels: Vec<String> = data["levels"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|l| l.as_str().map(str::to_string))
                        .collect();
                    if !levels.is_empty() {
                        this.models.thinking_levels = levels;
                    }
                }
            },
        );
    }

    /// The task list as the conversation's last `todo` call left it, unless
    /// the session feed reports the list itself.
    fn sync_todo_from_history(&mut self) {
        if self.desktop.sections().todo().is_some() {
            return;
        }
        for item in self.transcript.items.iter().rev() {
            if let gentle_core::transcript::Item::Tool { name, details, .. } = item
                && name == "todo"
                && let Some(tasks) = gentle_core::tools::todo_tasks(details)
            {
                self.todo.apply(tasks);
                return;
            }
        }
    }

    pub(super) fn set_model(&mut self, model: ModelInfo, cx: &mut Context<Self>) {
        let command = json!({"type": "set_model", "provider": model.provider, "modelId": model.id});
        self.request(command, cx, move |this, result, cx| {
            if this.report(result).is_some() {
                this.models.current = Some(model);
                this.refresh_stats(cx);
                this.refresh_thinking_levels(cx);
                this.request(json!({"type": "get_state"}), cx, |this, result, _| {
                    if let Ok(state) = result {
                        this.models.thinking =
                            state["thinkingLevel"].as_str().unwrap_or("off").to_string();
                    }
                });
            }
        });
    }

    /// Stop what is running: a `!command` with `abort_bash` (its `bash`
    /// response then settles the card), the turn with `abort` (the activity
    /// line reads "stopping…" until it ends).
    pub(super) fn stop(&mut self, cx: &mut Context<Self>) {
        if self.transcript.shells_running() {
            self.request(json!({"type": "abort_bash"}), cx, |this, result, _| {
                this.report(result);
            });
        }
        if self.busy() {
            self.live.stopping = self.turn_since();
            cx.notify();
            // Pi's Esc: drop queued messages first so abort does not resume them.
            self.request(json!({"type": "clear_queue"}), cx, |_, _, _| {});
            self.request(json!({"type": "abort"}), cx, |this, result, _| {
                this.report(result);
            });
        }
    }

    /// RPC `new_session`: a fresh conversation on the same backend. A fresh
    /// session also re-reads defaults such as the permission mode.
    pub fn new_session(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.replace_session(json!({"type": "new_session"}), cx);
    }

    /// `new_session` or `clone` (`/new`, `/clone` from the session feed's
    /// command list): Pi replaces the session on the same backend, so the
    /// conversation and the channel start over on the one it reports.
    pub(super) fn replace_session(&mut self, command: Value, cx: &mut Context<Self>) {
        self.request(command, cx, |this, result, cx| {
            if let Some(data) = this.report(result)
                && data["cancelled"] != true
            {
                this.reset_conversation();
                this.reset_desktop_channel();
                this.refresh_state(cx);
                this.refresh_git(cx);
            }
        });
    }

    /// RPC `switch_session` to a saved conversation.
    pub fn open_session(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let command = json!({"type": "switch_session", "sessionPath": path});
        self.request(command, cx, move |this, result, cx| {
            let switched = this
                .report(result)
                .is_some_and(|data| data["cancelled"] != true);
            if this.backend.pending_session.as_ref() == Some(&path) {
                this.backend.pending_session = None;
            }
            if switched {
                this.reset_conversation();
                this.session_file = Some(path);
                this.reset_desktop_channel();
                this.refresh_state(cx);
            }
        });
    }

    /// Send any RPC command for a window-level screen; failures surface in the transcript.
    pub fn send_command(&mut self, command: Value, cx: &mut Context<Self>) {
        self.request(command, cx, |this, result, cx| {
            this.report(result);
            this.refresh_state(cx);
        });
    }

    /// RPC `set_session_name`; the tab title follows.
    pub fn rename(&mut self, name: String, cx: &mut Context<Self>) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let command = json!({"type": "set_session_name", "name": name});
        self.request(command, cx, move |this, result, _| {
            if this.report(result).is_some() {
                // `sync_chrome` (after this) tells the window.
                this.session_name = Some(name);
            }
        });
    }

    /// `/compact [instructions]`: RPC `compact`. Pi reports progress and the
    /// result (or failure) as compaction events, shown in the conversation.
    pub(super) fn compact(&mut self, instructions: Option<String>, cx: &mut Context<Self>) {
        if self.usage.compacting || self.process().is_none() {
            return;
        }
        self.usage.compacting = true;
        self.usage.context_details_open = false;
        self.usage.context_ring_hovered = false;
        cx.notify();
        let mut command = json!({"type": "compact"});
        if let Some(instructions) = instructions {
            command["customInstructions"] = instructions.into();
        }
        self.request(command, cx, |this, _, cx| {
            // Success and failure both arrive as `compaction_end`.
            this.usage.compacting = false;
            this.refresh_stats(cx);
        });
    }

    pub(super) fn set_auto_compaction(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let command = json!({"type": "set_auto_compaction", "enabled": enabled});
        self.request(command, cx, move |this, result, _| {
            if this.report(result).is_some() {
                this.usage.auto_compaction = Some(enabled);
            }
        });
    }

    /// `/copy`: RPC `get_last_assistant_text` to the clipboard.
    pub(super) fn copy_last_answer(&mut self, cx: &mut Context<Self>) {
        self.request(
            json!({"type": "get_last_assistant_text"}),
            cx,
            |this, result, cx| {
                let Some(data) = this.report(result) else {
                    return;
                };
                match data["text"].as_str().filter(|t| !t.trim().is_empty()) {
                    Some(text) => this.copy_text(text.to_string(), cx),
                    None => {
                        this.toast(Notification::warning("There is no answer to copy yet."), cx)
                    }
                }
            },
        );
    }

    /// `/export [path]`: RPC `export_html` to `path`, or where the save
    /// dialog says (`~/Downloads/<session>.html` suggested), then reveal it.
    pub(super) fn export_html(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        if let Some(path) = path {
            self.export_to(expand_home(&path), cx);
            return;
        }
        let directory = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| self.cwd.clone());
        let name = format!("{}.html", export_file_stem(&self.title()));
        let chosen = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.export_to(path, cx));
        })
        .detach();
    }

    fn export_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let command = json!({"type": "export_html", "outputPath": path});
        self.request(command, cx, move |this, result, cx| {
            if let Some(data) = this.report(result) {
                let written = data["path"].as_str().map_or(path, PathBuf::from);
                cx.reveal_path(&written);
                this.note(
                    format!("Exported the conversation to {}", written.display()),
                    cx,
                );
            }
        });
    }

    pub(super) fn send_prompt(&mut self, text: String, cx: &mut Context<Self>) {
        self.send_prompt_with(text, Vec::new(), cx);
    }

    /// RPC `prompt`, or `steer` mid-turn, with `images`.
    pub(super) fn send_prompt_with(
        &mut self,
        text: String,
        images: Vec<attachments::Attachment>,
        cx: &mut Context<Self>,
    ) {
        let steer = self.transcript.running;
        let command = attachments::prompt_command(&text, &images, steer);
        for image in &images {
            self.image_cache.insert(image);
        }
        if !steer {
            let shown = images.iter().map(|a| a.image.clone()).collect();
            self.live
                .expect_reply(&text, shown, self.transcript.items.len());
        }
        // An extension command may answer with notices: gather them.
        let capture = text.trim_start().starts_with('/').then(|| {
            if let Some(open) = self.capture.as_ref().map(|c| c.id) {
                self.finish_capture(open);
            }
            self.capture_seq += 1;
            self.capture = Some(builtins::Capture::new(self.capture_seq, &text));
            self.capture_seq
        });
        self.request(command, cx, move |this, result, cx| {
            let reply = this.report(result);
            if reply.is_some() {
                this.live.reply_acknowledged();
            } else {
                this.live.reply_failed();
            }
            if let Some(id) = capture {
                let handled = reply.is_some_and(|data| data["disposition"] == "handled");
                this.command_replied(id, handled, cx);
            }
        });
    }

    /// Pi answered extension command `id`: a handled command's output is
    /// final [`builtins::OUTPUT_GRACE`] later; anything else started a turn
    /// (a prompt template or skill) and shows as one.
    fn command_replied(&mut self, id: u64, handled: bool, cx: &mut Context<Self>) {
        let Some(capture) = self.capture.as_mut().filter(|c| c.id == id) else {
            return;
        };
        if !handled {
            self.capture = None;
            return;
        }
        capture.replied(Instant::now());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(builtins::OUTPUT_GRACE).await;
            let _ = this.update(cx, |this, cx| {
                if this.finish_capture(id) {
                    this.sync_view(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Close capture `id`, adding its output to the conversation; whether
    /// anything was added.
    fn finish_capture(&mut self, id: u64) -> bool {
        let Some(capture) = self.capture.take_if(|c| c.id == id) else {
            return false;
        };
        if capture.notices.is_empty() {
            return false;
        }
        self.transcript
            .command_output(capture.command, capture.notices);
        true
    }

    /// Run a Gentle Shell extension command (`/gentle:…`) over RPC without
    /// adding a turn: Pi runs extension commands at once, even mid-stream.
    /// `done` gets `Ok` only when Pi reports the command as `handled`, so a
    /// missing or renamed command surfaces instead of being sent as a prompt.
    pub fn run_extension_command(
        &mut self,
        text: String,
        cx: &mut Context<Self>,
        done: impl FnOnce(Result<(), String>, &mut App) + 'static,
    ) {
        if self.process().is_none() {
            done(
                Err("Gentle Shell is still starting in this tab; try again in a moment.".into()),
                cx,
            );
            return;
        }
        let command = json!({"type": "prompt", "message": text});
        self.request(command, cx, move |_, result, cx| {
            let outcome = match result {
                Ok(data) if data["disposition"] == "handled" => Ok(()),
                Ok(data) => Err(format!(
                    "Gentle Shell did not run {text} (disposition {}). Is gentle-pi up to date?",
                    data["disposition"]
                )),
                Err(error) => Err(format!("{error:#}")),
            };
            done(outcome, cx);
        });
    }

    /// Give the backend the app's Agent UI mode when it differs from the
    /// one it has. A tab still starting gets it once ready; one not started
    /// yet starts in it.
    pub(super) fn sync_genui_mode(&mut self, cx: &mut Context<Self>) {
        let mode = crate::features::genui::mode::mode(cx);
        if mode == self.genui_mode || self.process().is_none() {
            return;
        }
        self.genui_mode = mode;
        self.run_extension_command(mode.command(), cx, |result, _| {
            if let Err(error) = result {
                eprintln!("gentle-shell-gpui: could not apply Agent UI: {error}");
            }
        });
    }
}

/// `~/x` → under the home folder; other paths as given.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

/// A session title as a file name: path separators and other characters
/// file systems reject become `-`.
fn export_file_stem(title: &str) -> String {
    let stem: String = title
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let stem = stem.trim_matches(['.', ' ']);
    if stem.is_empty() {
        "session".into()
    } else {
        stem.to_string()
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use serde_json::{Value, json};

    use super::{discovers_desktop, export_file_stem, parse_commands};

    #[test]
    fn only_a_listed_desktop_command_opens_the_channel() {
        let listed = Ok(json!({"commands": [
            {"name": "gentle:mode", "description": "Permissions"},
            {"name": "gentle:desktop", "description": "Desktop records"},
        ]}));
        assert!(discovers_desktop(&listed));

        // Another Gentle Shell command is not the extension.
        let other = Ok(json!({"commands": [{"name": "gentle:mode"}]}));
        assert!(!discovers_desktop(&other));

        // A failed listing, or data without commands, opens nothing: its prompt
        // would reach Pi as raw JSON.
        let failed: anyhow::Result<Value> = Err(anyhow!("get_commands failed"));
        assert!(!discovers_desktop(&failed));
        assert!(!discovers_desktop(&Ok(json!({"commands": "nope"}))));
    }

    #[test]
    fn command_lists_keep_their_names_and_descriptions() {
        let commands = parse_commands(&json!({"commands": [
            {"name": "a", "description": "d"},
            {"description": "no name"},
            "junk",
        ]}));
        assert_eq!(commands, vec![("a".to_string(), "d".to_string())]);
        assert!(parse_commands(&json!({})).is_empty());
    }

    #[test]
    fn export_names_are_safe_file_names() {
        assert_eq!(export_file_stem("Fix the API"), "Fix the API");
        assert_eq!(export_file_stem("a/b: c?"), "a-b- c-");
        assert_eq!(export_file_stem("  ..  "), "session");
        assert_eq!(export_file_stem(".hidden"), "hidden");
    }
}
