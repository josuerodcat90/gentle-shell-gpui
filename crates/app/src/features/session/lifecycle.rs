//! The backend's lifecycle: starting it, restarting it on the same
//! conversation (now or once the turn ends), resuming a saved session and
//! the conversation state a fresh start or switch resets.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use pi_rpc::PiProcess;

use pi_rpc::desktop_rpc::DesktopMethod;

use super::SessionView;
use super::state::BackendStatus;
use super::state::desktop::permission::Report;
use super::view;
use super::{ReadFailure, ReadOutcome, ReadRequest, ReadSupport};
use gentle_core::transcript::{NoticeLevel, Transcript};

/// What a `reload_credentials` answer means for the tab.
#[derive(Debug, PartialEq, Eq)]
enum Settled {
    /// The registry was rebuilt in place: re-read the models.
    Reloaded,
    /// The backend was replaced before it answered; it read the
    /// credentials when it started.
    Restarted,
    /// Not reloaded: restart the tab, as without the method.
    Restart,
}

fn credentials_settled(outcome: &ReadOutcome) -> Settled {
    match outcome {
        ReadOutcome::CredentialsReloaded(result) if result.reloaded && !result.aborted => {
            Settled::Reloaded
        }
        ReadOutcome::Failed(ReadFailure::Dropped) => Settled::Restarted,
        _ => Settled::Restart,
    }
}

impl SessionView {
    /// Start the backend unless it was started before. Idempotent.
    pub fn ensure_started(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.backend.started {
            return;
        }
        self.start(window, cx);
        self.animate_banner(cx);
    }

    /// Bloom the rose and write the wordmark in over
    /// [`crate::shared::banner::WRITE`]; the banner paces it.
    fn animate_banner(&mut self, cx: &mut Context<Self>) {
        self.banner_reveal = 0.0;
        self._banner_anim = cx.spawn(async move |this, cx| {
            const FRAME: Duration = Duration::from_millis(16);
            let frames = (crate::shared::banner::WRITE.as_millis() / FRAME.as_millis()) as u32;
            for frame in 1..=frames {
                cx.background_executor().timer(FRAME).await;
                crate::shared::diagnostics::tick();
                let t = frame as f32 / frames as f32;
                if this
                    .update(cx, |this, cx| {
                        this.banner_reveal = t;
                        // Only the hero shows the banner: repaint just this pane, and only then.
                        if this.hero_shown() {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
    }

    /// Without Gentle Shell's `reload_credentials`, new credentials only
    /// reach a backend through a fresh runtime: restart the tab, resuming its
    /// conversation (mid-turn, once the turn ends). A tab not started yet
    /// picks them up when it starts. Packages and the Claude bridge's plan
    /// always need this restart.
    pub fn reload_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.backend.started {
            return;
        }
        if self.transcript.items.is_empty() {
            self.start(window, cx);
        } else {
            self.restart_when_idle(
                "New sign-in saved; this tab reloads its models when the current turn ends.",
                window,
                cx,
            );
        }
    }

    /// A sign-in was saved (or removed): when this tab's Gentle Shell
    /// advertises `reload_credentials`, it refreshes its model registry in
    /// place (offline, mid-turn too) and the tab keeps running; otherwise,
    /// or if that reload fails, the tab restarts as before.
    pub fn reload_credentials(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.backend.started
            && self.read_support(DesktopMethod::ReloadCredentials) == ReadSupport::Rpc
            && let Ok(ticket) = self.read(ReadRequest::ReloadCredentials, cx)
        {
            self.credentials_reload = Some(ticket);
            return;
        }
        self.reload_models(window, cx);
    }

    /// The `reload_credentials` answer, once it arrived: a reloaded
    /// registry re-reads the models it now offers; a failure restarts the
    /// tab. A replaced backend read the credentials when it started.
    pub(super) fn settle_credentials(&mut self, cx: &mut Context<Self>) {
        let Some(ticket) = self.credentials_reload else {
            return;
        };
        let Some(outcome) = self.take_read(ticket) else {
            return;
        };
        self.credentials_reload = None;
        match credentials_settled(&outcome) {
            Settled::Reloaded => self.refresh_models(cx),
            Settled::Restarted => {}
            Settled::Restart => {
                self.in_window(cx, |this, window, cx| this.reload_models(window, cx))
            }
        }
    }

    /// Start again a tab whose backend never came up (gentle-shell missing
    /// or broken, then fixed by onboarding). Running and unstarted tabs are
    /// left alone; a backend that exited before any conversation counts.
    pub fn retry_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let never_ran = match &self.backend.status {
            BackendStatus::Failed(_) => true,
            BackendStatus::Exited(_) => self.transcript.items.is_empty(),
            BackendStatus::Starting | BackendStatus::Ready(_) => false,
        };
        if self.backend.started && never_ran {
            self.start(window, cx);
        }
    }

    /// Restart the backend in another folder; the conversation starts fresh.
    pub fn change_cwd(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.paths = std::sync::Arc::new(super::view::PathLookup::new(cwd.clone()));
        self.cwd = cwd;
        // A conversation still waiting to be resumed belongs to the old folder.
        self.backend.pending_session = None;
        if self.backend.started {
            self.start(window, cx);
        } else {
            self.sync_chrome(cx);
        }
    }

    /// Gentle Shell was updated on disk: run this tab on the new version,
    /// keeping its conversation (the session file is resumed on the new
    /// backend). A tab in the middle of a turn switches when the turn ends.
    pub fn restart_for_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A tab not started yet starts on the new version anyway.
        if !self.backend.started {
            return;
        }
        self.restart_when_idle(
            "Gentle Shell was updated; this tab switches to it when the current turn ends.",
            window,
            cx,
        );
    }

    /// `/reload`: a fresh backend on the same conversation (picks up new
    /// extensions and settings); mid-turn it waits for the turn to end.
    pub(super) fn reload(&mut self, cx: &mut Context<Self>) {
        if self.transcript.running || !self.extension.dialogs.is_empty() {
            self.backend.restart_pending = true;
            self.note(
                "Gentle Shell reloads in this tab when the current turn ends.".into(),
                cx,
            );
            return;
        }
        self.in_window(cx, |this, window, cx| this.restart_backend(window, cx));
    }

    /// Switch this session to `mode`. When the backend offers
    /// `set_permission_mode`, the picker's selection is the consent and the
    /// request switches the live session over RPC (`persist: true`); the
    /// confirmed state moves only when the server answers, and a successful
    /// switch never writes the default file or restarts. A Shell without the
    /// method keeps the legacy path below.
    pub(super) fn choose_mode(
        &mut self,
        mode: gentle_core::gentle_state::PermissionMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let wire_mode = super::to_desktop_mode(mode);
        let applied =
            self.permission
                .set_permission_mode(self.desktop.session(), wire_mode, Some(true));
        match mode_action(applied.report.as_ref()) {
            ModeAction::Legacy => self.legacy_choose_mode(mode, window, cx),
            ModeAction::Request | ModeAction::Report => self.apply_permission(applied, cx),
        }
    }

    /// The legacy mode switch: save `mode` as the default and restart the
    /// backend on the same conversation (mid-turn, once the turn ends). New
    /// sessions start in it too. Used only when Gentle Desktop cannot switch
    /// the live session over RPC.
    fn legacy_choose_mode(
        &mut self,
        mode: gentle_core::gentle_state::PermissionMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let config = pi_rpc::resolve_homes().config;
        if let Err(error) = crate::shared::gentle_state::save_default_mode(&config, mode) {
            self.transcript.notice(
                NoticeLevel::Error,
                format!("Could not save the permission mode: {error}"),
            );
            cx.notify();
            return;
        }
        self.default_mode = Some(mode);
        if self.mode == Some(mode) {
            cx.notify();
            return;
        }
        if !self.backend.started {
            // A tab not started yet starts in the saved mode anyway.
        } else if self.transcript.items.is_empty() {
            self.start(window, cx);
        } else {
            self.restart_when_idle(
                &format!(
                    "Permissions switch to {} when the current turn ends.",
                    mode.label()
                ),
                window,
                cx,
            );
        }
        window.push_notification(
            Notification::info(format!("Permissions: {}", mode.label())),
            cx,
        );
        cx.notify();
    }

    /// A server that advertised `set_permission_mode` answered
    /// `unsupported_method`. Keep the legacy save+restart behavior once, but
    /// defer the restart through `restart_pending` so the batch that delivered
    /// the response finishes against the backend being replaced.
    pub(super) fn legacy_choose_mode_deferred(
        &mut self,
        mode: gentle_core::gentle_state::PermissionMode,
        cx: &mut Context<Self>,
    ) {
        let config = pi_rpc::resolve_homes().config;
        if let Err(error) = crate::shared::gentle_state::save_default_mode(&config, mode) {
            self.transcript.notice(
                NoticeLevel::Error,
                format!("Could not save the permission mode: {error}"),
            );
            cx.notify();
            return;
        }
        self.default_mode = Some(mode);
        if self.mode == Some(mode) || !self.backend.started {
            cx.notify();
            return;
        }
        self.backend.restart_pending = true;
        self.transcript.notice(
            NoticeLevel::Info,
            format!(
                "Permissions switch to {} when the current turn ends.",
                mode.label()
            ),
        );
        cx.notify();
    }

    /// Restart the backend on the same conversation now, or, mid-turn or with
    /// a dialog open, once that settles (`waiting` tells the user).
    fn restart_when_idle(&mut self, waiting: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.transcript.running || !self.extension.dialogs.is_empty() {
            self.backend.restart_pending = true;
            self.transcript.notice(NoticeLevel::Info, waiting.into());
            cx.notify();
        } else {
            self.restart_backend(window, cx);
        }
    }

    pub(super) fn restart_backend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.backend.restart_pending = false;
        if let Some(path) = self.session_file() {
            self.backend.pending_session = Some(path);
        }
        self.start(window, cx);
    }

    /// Ask the backend to stop, without waiting for it; whether one was
    /// running. The app quits with this, before any tab is released.
    pub fn shutdown_backend(&self) -> bool {
        let BackendStatus::Ready(process) = &self.backend.status else {
            return false;
        };
        process.shutdown(Duration::from_secs(3));
        true
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shutdown_backend();
        self.backend.started = true;
        self.backend.status = BackendStatus::Starting;
        self.models.loaded = false;
        // This tab now has a backend of its own to ask: the previous one's
        // Desktop answers and event numbering are gone.
        self.desktop.reset();
        self.permission.reset(self.desktop.session());
        self.transcript = Transcript::default();
        self.markdown.clear();
        self.expanded.clear();
        self.live = view::Live::default();
        self.extension.dialogs.clear();
        self.extension.statuses.clear();
        self.extension.widgets.clear();
        self.activity.agents = None;
        self.activity.jobs.clear();
        self.activity.job_commands.clear();
        self.activity.phase = None;
        self.todo.apply(Vec::new());
        self.session_name = None;
        self.title = None;
        self.capture = None;
        self.usage.compacting = false;
        self.usage.reset_context();
        self.set_model_default(None, cx);
        self.refresh_git(cx);
        let cwd = self.cwd.clone();
        // A conversation to open goes in at launch (`--session`), not by
        // `switch_session` after: Gentle Shell treats the launch session as
        // fresh, so it starts in the saved permission mode, while a switched
        // one starts in Normal.
        let resume = self.backend.pending_session.take();
        let launched = resume.clone();
        // Agent UI: the extension loads in the app's mode (a change made
        // while it starts is sent once it is ready).
        let genui_mode = crate::features::genui::mode::mode(cx);
        // Where agents' pages go, as chosen now: a change applies to the tabs
        // started (or restarted) after it.
        let env: Vec<(String, String)> = [crate::features::genui::mode::launch_env(genui_mode)]
            .into_iter()
            .chain(gentle_core::web_browsers::design_launch_env(
                crate::features::browser::external::agent_pages(cx),
                gentle_core::ides::Os::CURRENT,
                std::env::var_os("WAYLAND_DISPLAY").as_deref(),
            ))
            .collect();
        let canvas = self.canvas.clone();
        // The backend chosen when the tab first started: a restart resumes
        // its conversation there, wherever new tabs go now.
        let runs_on = self.runs_on.clone();
        self._io = cx.spawn_in(window, async move |this, cx| {
            let spawned = cx
                .background_executor()
                .spawn(async move {
                    let runs_on =
                        runs_on.unwrap_or_else(|| crate::features::backend::for_tab(&cwd));
                    // The canvas this agent works on: its extension's tools.
                    let files: Vec<(String, &std::path::Path)> = canvas
                        .as_deref()
                        .map(|file| (crate::features::canvas::AGENT_ENV.to_string(), file))
                        .into_iter()
                        .collect();
                    let mut spec =
                        crate::features::backend::launch_spec(&runs_on, &cwd, &env, &files)?;
                    if let Some(path) = &resume {
                        // A conversation listed from this computer's view of the
                        // backend's home reads there as the backend's path.
                        let path = path.to_string_lossy();
                        spec.args.push("--session".into());
                        spec.args
                            .push(runs_on.path(&path).unwrap_or_else(|| path.into_owned()));
                    }
                    let version = crate::features::backend::shell_version(&runs_on, &spec);
                    let (process, rx) = PiProcess::spawn(&spec)?;
                    anyhow::Ok((process, rx, version, runs_on))
                })
                .await;
            let rx = match spawned {
                Ok((process, rx, version, runs_on)) => {
                    let ok = this.update(cx, |this, cx| {
                        this.backend.status = BackendStatus::Ready(process);
                        this.backend.version = version;
                        this.runs_on = Some(runs_on);
                        this.genui_mode = genui_mode;
                        this.sync_genui_mode(cx);
                        if let Some(path) = launched {
                            this.session_file = Some(path);
                        }
                        this.refresh_state(cx);
                        // Kept until the switch lands, so the window saves this
                        // path rather than the blank session it replaces.
                        if let Some(path) = this.backend.pending_session.clone() {
                            this.open_session(path, cx);
                        }
                        if let Some(prompt) = this.backend.pending_prompt.take() {
                            this.run_text(prompt, cx);
                        }
                        cx.notify();
                    });
                    if ok.is_err() {
                        return;
                    }
                    rx
                }
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.backend.status = BackendStatus::Failed(format!("{error:#}"));
                        cx.notify();
                    });
                    return;
                }
            };
            while let Ok(first) = rx.recv().await {
                // One update (and one frame) for everything already queued.
                let batch = drain_queued(first, &rx);
                if this
                    .update_in(cx, |this, window, cx| this.handle_batch(batch, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.sync_chrome(cx);
        cx.notify();
    }

    /// Switch to `path` as soon as the backend is ready.
    pub fn resume_when_ready(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.process().is_some() {
            self.open_session(path, cx);
        } else {
            self.backend.pending_session = Some(path);
        }
    }

    /// Forget the conversation shown, for another one on the same backend.
    pub(super) fn reset_conversation(&mut self) {
        self.transcript = Transcript::default();
        self.markdown.clear();
        self.expanded.clear();
        self.todo.apply(Vec::new());
        self.activity.jobs.clear();
        self.activity.phase = None;
        self.session_name = None;
        self.activity.agents = None;
        self.live = view::Live::default();
        self.capture = None;
        self.usage.context_latch.rearm();
    }
}

/// `first` plus every message already queued behind it, in arrival order.
fn drain_queued<T>(first: T, rx: &async_channel::Receiver<T>) -> Vec<T> {
    let mut batch = Vec::with_capacity(rx.len() + 1);
    batch.push(first);
    while let Ok(next) = rx.try_recv() {
        batch.push(next);
    }
    batch
}

/// What one picker selection should do, from the permission slice's answer to
/// `set_permission_mode`.
enum ModeAction {
    /// Send the RPC request and wait for the server's truth: never write the
    /// default file or restart for this.
    Request,
    /// The peer does not offer the live switch: keep the legacy save+restart
    /// path once.
    Legacy,
    /// A failure (or a second in-flight selection): report, never fall back.
    Report,
}

fn mode_action(report: Option<&Report>) -> ModeAction {
    match report {
        None => ModeAction::Request,
        Some(Report::Unsupported) | Some(Report::UnsupportedMethod) => ModeAction::Legacy,
        Some(_) => ModeAction::Report,
    }
}

#[cfg(test)]
mod tests {
    use pi_rpc::desktop_rpc::{AuthStatus, DesktopErrorInfo, DesktopRpcError, ReloadCredentials};

    use super::{ModeAction, drain_queued, mode_action};
    use super::{ReadFailure, ReadOutcome, Report, Settled, credentials_settled};

    fn reloaded(reloaded: bool, aborted: bool) -> ReadOutcome {
        ReadOutcome::CredentialsReloaded(ReloadCredentials {
            reloaded,
            aborted,
            failed_providers: Vec::new(),
            status: AuthStatus {
                providers: Vec::new(),
                truncated: false,
            },
        })
    }

    /// Only a registry Gentle Shell rebuilt keeps the tab running; anything
    /// else restarts it as before, except a backend already replaced.
    #[test]
    fn only_a_reloaded_registry_spares_the_restart() {
        assert_eq!(
            credentials_settled(&reloaded(true, false)),
            Settled::Reloaded
        );
        assert_eq!(credentials_settled(&reloaded(true, true)), Settled::Restart);
        assert_eq!(
            credentials_settled(&reloaded(false, false)),
            Settled::Restart
        );
        assert_eq!(
            credentials_settled(&ReadOutcome::Failed(ReadFailure::Timeout)),
            Settled::Restart
        );
        assert_eq!(
            credentials_settled(&ReadOutcome::Failed(ReadFailure::Dropped)),
            Settled::Restarted
        );
    }

    #[test]
    fn batches_drain_everything_queued_in_order() {
        let (tx, rx) = async_channel::unbounded();
        for n in 1..=4 {
            tx.try_send(n).unwrap();
        }
        let first = rx.try_recv().unwrap();
        assert_eq!(drain_queued(first, &rx), vec![1, 2, 3, 4]);
        assert!(rx.is_empty());

        // Later messages start the next batch.
        tx.try_send(5).unwrap();
        assert_eq!(drain_queued(rx.try_recv().unwrap(), &rx), vec![5]);

        // A closed channel still yields what was queued before closing.
        tx.try_send(6).unwrap();
        tx.try_send(7).unwrap();
        drop(tx);
        assert_eq!(drain_queued(rx.try_recv().unwrap(), &rx), vec![6, 7]);
    }

    #[test]
    fn a_picker_selection_requests_legacy_or_reports() {
        // No report: the RPC request goes out; never a file write or restart.
        assert!(matches!(mode_action(None), ModeAction::Request));
        // Missing capability or a server `unsupported_method` keeps the legacy
        // save+restart path once.
        assert!(matches!(
            mode_action(Some(&Report::Unsupported)),
            ModeAction::Legacy
        ));
        assert!(matches!(
            mode_action(Some(&Report::UnsupportedMethod)),
            ModeAction::Legacy
        ));
        // Every other failure is reported, never silently fallen back to.
        let failure = Report::Failure(DesktopRpcError::RequestInFlight {
            request_id: "desktop-1".into(),
        });
        assert!(matches!(mode_action(Some(&failure)), ModeAction::Report));
        let persist = Report::PersistError(DesktopErrorInfo {
            code: "readonly".into(),
            message: "nope".into(),
        });
        assert!(matches!(mode_action(Some(&persist)), ModeAction::Report));
        // Capabilities still loading, and a timeout, must report instead of
        // writing the default file or restarting through the legacy path.
        assert!(matches!(
            mode_action(Some(&Report::NotReady)),
            ModeAction::Report
        ));
        assert!(matches!(
            mode_action(Some(&Report::Timeout)),
            ModeAction::Report
        ));
    }
}
