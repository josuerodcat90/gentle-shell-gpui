//! Gentle Shell's own updates: the "is available" card, `gentle-shell
//! upgrade`, and how it ended as the app sees it afterwards.

use std::path::Path;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pi_rpc::desktop_rpc::{DesktopMethod, UpdateStatus};

use super::claude::forget_claude_auth;
use super::snapshot::Snapshot;
use super::{SettingsEvent, SettingsView};
use crate::features::session::{ReadFailure, ReadOutcome, ReadRequest, ReadSupport, ReadTicket};
use crate::features::updates::UpdateOffer;
use crate::features::updates::status::offer_from_status;
use crate::shared::ui;

/// How long "Waiting for the agent tab to start…" shows. A tab whose probe
/// never answers stays undecided, so the section then shows what the npm
/// path found, as before the wait (the Versions row and any update it
/// offers); the read is still asked if the tab gets ready later.
const TAB_START_WAIT: Duration = Duration::from_secs(60);

/// Gentle Shell's update status, when the active agent tab's Shell reports
/// it.
#[derive(Default)]
pub(super) struct ShellUpdates {
    pub(super) status: Option<UpdateStatus>,
    /// The read in flight, and whether it refreshes.
    pending: Option<(ReadTicket, bool)>,
    /// The tab's Shell did not answer: the npm path, until Configuration
    /// opens again.
    failed: bool,
    /// A read asked while the tab was still starting (its probe had not
    /// answered), and whether it refreshes: asked if the probe answers. A
    /// refresh asked meanwhile is kept by later plain reads.
    awaiting: Option<bool>,
    /// Hides the waiting notice after `TAB_START_WAIT`.
    wait_notice: Option<Task<()>>,
    /// The waiting notice timed out; the read stays awaited.
    wait_expired: bool,
}

impl SettingsView {
    /// Ask the active agent tab's Gentle Shell for its update status
    /// (`refresh` looks it up, bounded by the Shell); `false` when it cannot
    /// answer (not advertised, its tab still starting, or it failed before),
    /// and then the npm path runs as before.
    pub(super) fn ask_update_status(&mut self, refresh: bool, cx: &mut Context<Self>) -> bool {
        if self.shell_updates.failed {
            return false;
        }
        let Some(session) = self.session.clone() else {
            return false;
        };
        match session
            .read(cx)
            .read_support(DesktopMethod::GetUpdateStatus)
        {
            ReadSupport::Rpc => {}
            ReadSupport::NotReady => {
                let refresh = refresh || self.shell_updates.awaiting == Some(true);
                self.shell_updates.awaiting = Some(refresh);
                if self.shell_updates.wait_notice.is_none() {
                    self.shell_updates.wait_notice = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(TAB_START_WAIT).await;
                        let _ = this.update(cx, |this, cx| {
                            this.shell_updates.wait_expired = true;
                            cx.notify();
                        });
                    }));
                }
                return false;
            }
            ReadSupport::Legacy => return false,
        }
        match session.update(cx, |session, cx| {
            session.read(ReadRequest::UpdateStatus { refresh }, cx)
        }) {
            Ok(ticket) => {
                self.shell_updates.pending = Some((ticket, refresh));
                true
            }
            Err(_) => false,
        }
    }

    /// Configuration reads Gentle Shell's own update status (not the npm
    /// path), and whether a refresh is in flight.
    pub(super) fn shell_reports_updates(&self) -> Option<bool> {
        let checking =
            matches!(self.shell_updates.pending, Some((_, true))) || self.waiting_for_tab();
        (self.shell_updates.status.is_some()
            || self.shell_updates.pending.is_some()
            || self.waiting_for_tab())
        .then_some(checking)
    }

    /// The "Waiting for the agent tab to start…" notice shows: a read waits
    /// for the tab and `TAB_START_WAIT` has not passed.
    pub(super) fn waiting_for_tab(&self) -> bool {
        self.shell_updates.awaiting.is_some() && !self.shell_updates.wait_expired
    }

    /// The tab changed: take Gentle Shell's answer once it arrived. A plain
    /// read that knows nothing yet (no launcher record, no earlier refresh)
    /// is looked up once, as the npm path would on a stale cache. A Shell
    /// that fails falls back to the npm path; one replaced meanwhile (an
    /// upgrade restarts the tabs) keeps the last answer, and a refresh it
    /// dropped is asked again of the new one once its tab is ready.
    pub(super) fn update_status_changed(&mut self, cx: &mut Context<Self>) {
        if self.shell_updates.pending.is_none()
            && let Some(refresh) = self.shell_updates.awaiting.take()
        {
            // Asked again: it waits once more while the probe is still out.
            let asked = self.ask_update_status(refresh, cx);
            if self.shell_updates.awaiting.is_none() {
                // Decided (asked, or the npm path): the notice goes.
                self.shell_updates.wait_notice = None;
                self.shell_updates.wait_expired = false;
            }
            if asked || self.shell_updates.awaiting.is_none() {
                cx.notify();
            }
            return;
        }
        let (Some((ticket, refreshed)), Some(session)) =
            (self.shell_updates.pending, self.session.clone())
        else {
            return;
        };
        let Some(outcome) = session.update(cx, |session, _| session.take_read(ticket)) else {
            return;
        };
        self.shell_updates.pending = None;
        match outcome {
            ReadOutcome::UpdateStatus(status) => {
                let unknown =
                    !refreshed && !status.offline && status.gentle_shell.checked_at.is_none();
                self.shell_updates.status = Some(status);
                if unknown {
                    self.ask_update_status(true, cx);
                }
            }
            ReadOutcome::Failed(ReadFailure::Dropped) => {
                if refreshed {
                    self.ask_update_status(true, cx);
                }
            }
            _ => {
                self.shell_updates.failed = true;
                self.shell_updates.status = None;
                self.reload(cx);
            }
        }
        cx.notify();
    }

    /// Keep the offer the running upgrade started with. Otherwise use Gentle
    /// Shell's reported offer, or the npm path if it did not report status.
    pub(super) fn update_offer(&self, s: &Snapshot) -> Option<UpdateOffer> {
        self.shell_update
            .release_offer(|| match &self.shell_updates.status {
                Some(status) => offer_from_status(
                    &status.gentle_shell,
                    s.managed,
                    s.skipped_release.as_deref(),
                ),
                None => s.update.clone(),
            })
    }
}

/// How `gentle-shell upgrade` ended, as the app sees it afterwards.
#[derive(Clone, Debug, PartialEq)]
enum UpgradeOutcome {
    /// The launcher the app resolves now runs this version.
    Installed(String),
    /// The upgrade ran, but the launcher the app resolves still runs
    /// `running` (another install comes first).
    Stale {
        running: String,
        launcher: String,
    },
    /// The upgrade exited with an error, but the launcher the app resolves
    /// now runs this newer main build: Gentle Shell moved, something after
    /// it did not finish.
    Incomplete(String),
    Failed,
}

#[derive(Clone)]
pub(super) struct UpgradeResult {
    target: String,
    outcome: UpgradeOutcome,
    /// Gentle Shell's last output lines.
    lines: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UpgradeChannel {
    Default,
    Main,
}

impl UpgradeChannel {
    fn args(self) -> &'static [&'static str] {
        match self {
            Self::Default => &[],
            Self::Main => &["--channel", "main"],
        }
    }

    fn matches_version(self, target: &str, running: &str) -> bool {
        running == target
            || (matches!(self, Self::Main)
                && running
                    .split_once("-main.")
                    .is_some_and(|(_, build)| build.rsplit('.').next() == Some(target)))
    }
}

/// `<version>-main.<commit>`, the version a main build reports.
fn is_main_build(version: &str) -> bool {
    version.split_once("-main.").is_some_and(|(_, commit)| {
        !commit.is_empty() && commit.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// Decide the outcome from the upgrade's exit and what the launcher the app
/// now resolves (`launcher`) reports as its version (`running`). On main the
/// upgrade installs main's tip, which can be newer than the commit offered
/// (`target`): any main build other than the one running before (`before`)
/// counts.
fn upgrade_outcome(
    succeeded: bool,
    target: &str,
    channel: UpgradeChannel,
    before: Option<&str>,
    running: Option<&str>,
    launcher: Option<&Path>,
) -> UpgradeOutcome {
    let Some(running) = running else {
        return UpgradeOutcome::Failed;
    };
    let moved_on_main = matches!(channel, UpgradeChannel::Main)
        && is_main_build(running)
        && before != Some(running);
    match succeeded {
        true if channel.matches_version(target, running) || moved_on_main => {
            UpgradeOutcome::Installed(running.to_string())
        }
        false if moved_on_main => UpgradeOutcome::Incomplete(running.to_string()),
        false => UpgradeOutcome::Failed,
        true => UpgradeOutcome::Stale {
            running: running.to_string(),
            launcher: launcher.map_or_else(
                || "gentle-shell".to_string(),
                |path| path.display().to_string(),
            ),
        },
    }
}

/// The offer accepted when `gentle-shell upgrade` started.
enum UpgradeTarget {
    Default(UpdateOffer),
    Main(String),
}

impl UpgradeTarget {
    fn channel(&self) -> UpgradeChannel {
        match self {
            Self::Default(_) => UpgradeChannel::Default,
            Self::Main(_) => UpgradeChannel::Main,
        }
    }

    fn version(&self) -> String {
        match self {
            Self::Default(offer) => offer.latest.clone(),
            Self::Main(commit) => commit.clone(),
        }
    }
}

/// Gentle Shell and Pi update progress and output.
#[derive(Default)]
pub(super) struct ShellUpdateUiState {
    upgrading: Option<UpgradeTarget>,
    pub(super) result: Option<UpgradeResult>,
    skip_error: Option<String>,
    pi_running: bool,
    pi_result: Option<(Vec<String>, bool)>,
    /// When the running upgrade or Pi update started, for its elapsed time.
    started: Option<Instant>,
    /// Redraws once a second while one runs, so the elapsed time moves.
    ticker: Option<Task<()>>,
}

impl ShellUpdateUiState {
    /// Both Shell upgrade channels and Pi share the package task slot.
    pub(super) fn blocks_packages(&self) -> bool {
        self.upgrading.is_some() || self.pi_running
    }

    fn release_offer(&self, live: impl FnOnce() -> Option<UpdateOffer>) -> Option<UpdateOffer> {
        match &self.upgrading {
            Some(UpgradeTarget::Default(offer)) => Some(offer.clone()),
            _ => live(),
        }
    }

    pub(super) fn main_commit<'a>(&'a self, live: Option<&'a str>) -> Option<&'a str> {
        match &self.upgrading {
            Some(UpgradeTarget::Main(commit)) => Some(commit),
            _ => live,
        }
    }
}

impl SettingsView {
    /// Starts the elapsed time of the upgrade or Pi update that just began.
    fn start_clock(&mut self, cx: &mut Context<Self>) {
        self.shell_update.started = Some(Instant::now());
        self.shell_update.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.shell_update.blocks_packages()
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        }));
    }

    fn stop_clock(&mut self) {
        self.shell_update.started = None;
        self.shell_update.ticker = None;
    }

    /// "`label` 1:42": the busy label with the time since it started.
    fn busy_label(&self, label: &str) -> String {
        let seconds = self
            .shell_update
            .started
            .map_or(0, |started| started.elapsed().as_secs());
        format!("{label} {}", gentle_core::text::elapsed_clock(seconds))
    }

    /// The shell upgrade on `channel` runs: its button shows the spinner.
    fn upgrading_on(&self, channel: UpgradeChannel) -> Option<String> {
        (self
            .shell_update
            .upgrading
            .as_ref()
            .map(UpgradeTarget::channel)
            == Some(channel))
        .then(|| self.busy_label("Updating…"))
    }

    fn shell_job_running(&self) -> bool {
        self.shell_update.blocks_packages() || self.package.running.is_some()
    }

    /// Gentle Shell's own `upgrade` toward `target` (it knows how it was
    /// installed; since 4.7 it moves to its self-contained install), then ask
    /// the launcher the app resolves now which version it runs: that's the
    /// result shown.
    fn run_upgrade(&mut self, target: UpgradeTarget, cx: &mut Context<Self>) {
        if self.shell_job_running() {
            return;
        }
        let channel = target.channel();
        let target_version = target.version();
        self.shell_update.upgrading = Some(target);
        self.shell_update.result = None;
        self.start_clock(cx);
        let cwd = self.cwd.clone();
        self._job = cx.spawn(async move |this, cx| {
            let upgrade = cx
                .background_executor()
                .spawn(async move { super::services::updates::upgrade_shell(&cwd, channel.args()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let target = target_version;
                this.shell_update.upgrading = None;
                this.stop_clock();
                forget_claude_auth();
                let outcome = upgrade_outcome(
                    upgrade.succeeded,
                    &target,
                    channel,
                    upgrade.before.as_deref(),
                    upgrade.running.as_deref(),
                    upgrade.launcher.as_deref(),
                );
                // A new build runs, even if the upgrade did not finish after it.
                if matches!(
                    outcome,
                    UpgradeOutcome::Installed(_) | UpgradeOutcome::Incomplete(_)
                ) {
                    cx.emit(SettingsEvent::BackendUpdated);
                }
                this.shell_update.result = Some(UpgradeResult {
                    target,
                    outcome,
                    lines: last_output_lines(&upgrade.text),
                });
                // Look the update status up again: the one cached names the
                // commit offered before the upgrade.
                this.reload_with(true, cx);
                cx.notify();
            });
        });
        cx.notify();
    }

    fn skip_update(&mut self, version: String, cx: &mut Context<Self>) {
        if let Err(error) = crate::features::updates::skip(&version) {
            self.shell_update.skip_error = Some(format!("Could not remember the skip: {error}"));
        }
        self.reload(cx);
    }

    /// "Gentle Shell X is available": update, release notes or skip, as the
    /// terminal's startup prompt offers.
    pub(super) fn render_update(
        &self,
        offer: UpdateOffer,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let target = offer.clone();
        let skipped = offer.latest.clone();
        let upgrading = self.shell_update.upgrading.is_some();
        let busy = self.upgrading_on(UpgradeChannel::Default);
        let running = busy.is_some();
        update_card(
            "update",
            "Gentle Shell",
            "Runs gentle-shell upgrade. Open agent tabs then restart on the new version and keep their conversations; a tab mid-turn switches when the turn ends.",
            &offer,
            update_now_button("update", busy)
                .disabled(!running && self.shell_job_running())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.run_upgrade(UpgradeTarget::Default(target.clone()), cx)
                })),
            upgrading,
            cx.listener(move |this, _, _, cx| this.skip_update(skipped.clone(), cx)),
            self.shell_update.skip_error.clone(),
        )
        .when(running, |card| card.child(upgrade_wait_note()))
    }

    pub(super) fn render_main_update(&self, commit: &str, cx: &mut Context<Self>) -> Div {
        let target = commit.to_string();
        let busy = self.upgrading_on(UpgradeChannel::Main);
        let running = busy.is_some();
        update_card_body(
            format!("Gentle Shell main has a newer commit {commit}"),
            None,
            "Runs gentle-shell upgrade --channel main. Open agent tabs then restart on the new build and keep their conversations; a tab mid-turn switches when the turn ends.",
        )
        .child(
            h_flex().child(
                update_now_button("update-main", busy)
                    .disabled(!running && self.shell_job_running())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_upgrade(UpgradeTarget::Main(target.clone()), cx)
                    })),
            ),
        )
        .when(running, |card| card.child(upgrade_wait_note()))
    }

    fn run_pi_update(&mut self, cx: &mut Context<Self>) {
        if self.shell_job_running() {
            return;
        }
        self.shell_update.pi_running = true;
        self.shell_update.pi_result = None;
        self.start_clock(cx);
        let cwd = self.cwd.clone();
        self._job = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { crate::features::setup::run_gentle_shell(&cwd, &["update"]) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let (text, failed) = match result {
                    Ok(text) => (text, false),
                    Err(text) => (text, true),
                };
                let mut lines = last_output_lines(&text);
                if lines.is_empty() {
                    lines.push(if failed {
                        "The Pi update didn't finish.".into()
                    } else {
                        "The Pi update finished.".into()
                    });
                }
                this.shell_update.pi_running = false;
                this.stop_clock();
                this.shell_update.pi_result = Some((lines, failed));
                this.reload_with(true, cx);
                cx.notify();
            });
        });
        cx.notify();
    }

    pub(super) fn render_pi_update(&self, available: bool, cx: &mut Context<Self>) -> Option<Div> {
        let running = self.shell_update.pi_running;
        let show_button = available || running;
        let output = self.shell_update.pi_result.as_ref();
        if !show_button && output.is_none() {
            return None;
        }
        Some(
            v_flex()
                .when(show_button, |d| {
                    d.child(
                        h_flex().pt_2().child(
                            Button::new("update-pi")
                                .xsmall()
                                .ghost()
                                .icon(Icon::new(IconName::ArrowUp))
                                .label(if running {
                                    self.busy_label("Updating Pi…")
                                } else {
                                    "Update Pi".to_string()
                                })
                                .tooltip("gentle-shell update")
                                .loading(running)
                                .disabled(!running && self.shell_job_running())
                                .on_click(cx.listener(|this, _, _, cx| this.run_pi_update(cx))),
                        ),
                    )
                })
                .when(running, |d| {
                    d.child(
                        div()
                            .pt_1()
                            .text_xs()
                            .text_color(ui::muted())
                            .child("This can take several minutes."),
                    )
                })
                .children(output.map(|(lines, failed)| upgrade_output(lines.clone(), *failed))),
        )
    }

    /// Update Engram the way it was installed, then check what runs.
    fn run_engram_upgrade(&mut self, cx: &mut Context<Self>) {
        if self.engram.upgrading {
            return;
        }
        self.engram.upgrading = true;
        self.engram.result = None;
        self.engram._job = cx.spawn(async move |this, cx| {
            let (result, lines) = cx
                .background_executor()
                .spawn(async move { super::services::updates::upgrade_engram() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.engram.upgrading = false;
                let mut shown = lines[lines.len().saturating_sub(8)..].to_vec();
                let failed = result.is_err();
                shown.push(match result {
                    Ok(version) => format!(
                        "Engram {version} is installed. An Engram server that's already running keeps the old version until it restarts."
                    ),
                    Err(error) => error,
                });
                this.engram.result = Some((shown, failed));
                this.reload(cx);
                cx.notify();
            });
        });
        cx.notify();
    }

    fn skip_engram(&mut self, version: String, cx: &mut Context<Self>) {
        if let Err(error) = crate::features::updates::engram::skip(&version) {
            self.engram.skip_error = Some(format!("Could not remember the skip: {error}"));
        }
        self.reload(cx);
    }

    /// "Engram X is available", with the result of the last update.
    pub(super) fn render_engram_update(
        &self,
        offer: Option<crate::features::updates::UpdateOffer>,
        cx: &mut Context<Self>,
    ) -> Div {
        let card = offer.map(|offer| {
            let skipped = offer.latest.clone();
            update_card(
                "engram-update",
                "Engram",
                "Updates Engram the way it was installed (Homebrew, or the release the app downloaded). Your memories stay.",
                &offer,
                update_now_button(
                    "engram-update",
                    self.engram.upgrading.then(|| "Updating…".to_string()),
                )
                .on_click(cx.listener(|this, _, _, cx| this.run_engram_upgrade(cx))),
                self.engram.upgrading,
                cx.listener(move |this, _, _, cx| this.skip_engram(skipped.clone(), cx)),
                self.engram.skip_error.clone(),
            )
        });
        div().children(card).children(
            self.engram
                .result
                .clone()
                .map(|(lines, failed)| div().mt_2().child(upgrade_output(lines, failed))),
        )
    }
}

/// Engram's update: running, how it ended, a failed skip.
pub(super) struct EngramUpdate {
    upgrading: bool,
    /// The last lines and whether it failed.
    result: Option<(Vec<String>, bool)>,
    skip_error: Option<String>,
    _job: Task<()>,
}

impl Default for EngramUpdate {
    fn default() -> Self {
        Self {
            upgrading: false,
            result: None,
            skip_error: None,
            _job: Task::ready(()),
        }
    }
}

/// "<tool> X is available": update now, release notes, skip this version.
#[allow(clippy::too_many_arguments)]
fn update_card(
    id: &'static str,
    tool: &str,
    what_it_does: &'static str,
    offer: &crate::features::updates::UpdateOffer,
    update_button: Button,
    busy: bool,
    on_skip: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    error: Option<String>,
) -> Div {
    let notes = offer.notes_url.clone();
    update_card_body(
        format!("{tool} {} is available", offer.latest),
        Some(&offer.current),
        what_it_does,
    )
    .child(
        h_flex()
            .gap_2()
            .child(update_button)
            .child(
                Button::new((id, 1usize))
                    .ghost()
                    .small()
                    .icon(Icon::new(IconName::ExternalLink))
                    .label("Release notes")
                    .on_click(move |_, _, cx| cx.open_url(&notes)),
            )
            .child(
                Button::new((id, 2usize))
                    .ghost()
                    .small()
                    .label("Skip this version")
                    .disabled(busy)
                    .on_click(on_skip),
            ),
    )
    .children(error.map(|error| upgrade_output(vec![error], true)))
}

/// "Update now", or `busy` with a spinner while the update runs. A loading
/// button is already inert, so it stays enabled: disabled would dim the
/// spinner.
fn update_now_button(id: &'static str, busy: Option<String>) -> Button {
    let loading = busy.is_some();
    Button::new((id, 0usize))
        .primary()
        .small()
        // `loading` draws its spinner in the icon slot.
        .when(loading, |button| button.icon(Icon::new(IconName::ArrowUp)))
        .label(busy.unwrap_or_else(|| "Update now".to_string()))
        .loading(loading)
}

/// Shown while Gentle Shell upgrades: it prints nothing until it ends.
fn upgrade_wait_note() -> Div {
    div()
        .text_xs()
        .text_color(ui::muted())
        .child("This can take several minutes. Open agent tabs restart when it ends.")
}

fn update_card_body(title: String, current: Option<&str>, what_it_does: &'static str) -> Div {
    v_flex()
        .mt_2()
        .p_3()
        .gap_2()
        .rounded_lg()
        .bg(ui::accent().opacity(0.10))
        .child(
            h_flex()
                .gap_2()
                .child(
                    Icon::new(IconName::ArrowUp)
                        .small()
                        .text_color(ui::accent()),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(ui::fg())
                        .child(title),
                )
                .children(current.map(|version| {
                    div()
                        .text_xs()
                        .text_color(ui::muted())
                        .child(format!("you have {version}"))
                })),
        )
        .child(div().text_xs().text_color(ui::muted()).child(what_it_does))
}

/// The upgrade's result: what happened and what's left to do, then
/// Gentle Shell's own last lines.
pub(super) fn upgrade_result(result: &UpgradeResult) -> Div {
    let (icon, color, title, line) = match &result.outcome {
        UpgradeOutcome::Installed(version) => (
            IconName::CircleCheck,
            ui::success(),
            format!("Gentle Shell {version} is installed"),
            "Open agent tabs restarted on it and kept their conversations. Nothing else to do."
                .to_string(),
        ),
        UpgradeOutcome::Stale { running, launcher } => (
            IconName::TriangleAlert,
            ui::warning(),
            format!(
                "Gentle Shell {} installed, but the app still runs {running}",
                result.target
            ),
            format!(
                "{launcher} comes first. Remove the old package with `npm uninstall -g gentle-pi`, then open Configuration again."
            ),
        ),
        UpgradeOutcome::Incomplete(version) => (
            IconName::TriangleAlert,
            ui::warning(),
            format!("Gentle Shell {version} is installed, but the update didn't finish"),
            "Open agent tabs restarted on it. Gentle Shell's output below says what is left to do."
                .to_string(),
        ),
        UpgradeOutcome::Failed => (
            IconName::CircleX,
            ui::danger(),
            "The update didn't finish".to_string(),
            "Gentle Shell's output is below; the version you had keeps running.".to_string(),
        ),
    };
    let failed = matches!(
        result.outcome,
        UpgradeOutcome::Failed | UpgradeOutcome::Incomplete(_)
    );
    v_flex()
        .mt_2()
        .p_3()
        .gap_1()
        .rounded_lg()
        .bg(color.opacity(0.10))
        .child(
            h_flex()
                .gap_2()
                .child(Icon::new(icon).small().text_color(color))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(ui::fg())
                        .child(title),
                ),
        )
        .child(div().text_xs().text_color(ui::muted()).child(line))
        .when(!result.lines.is_empty(), |d| {
            d.child(upgrade_output(result.lines.clone(), failed))
        })
}

fn last_output_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<_> = text
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(8)
        .map(str::to_string)
        .collect();
    lines.reverse();
    lines
}

/// The update command's last lines, tinted by outcome.
fn upgrade_output(lines: Vec<String>, failed: bool) -> Div {
    v_flex()
        .mt_1()
        .p_2()
        .rounded_md()
        .bg(ui::well())
        .font_family(ui::mono())
        .text_size(ui::code_rems(12.))
        .text_color(if failed { ui::danger() } else { ui::muted() })
        .children(lines.into_iter().map(|line| div().child(line)))
}

#[cfg(test)]
mod tests {
    use super::{
        ShellUpdateUiState, UpgradeChannel, UpgradeOutcome, UpgradeTarget, upgrade_outcome,
    };
    use gentle_core::version::offer_for;

    #[test]
    fn default_upgrade_blocks_packages_while_it_owns_the_job_slot() {
        let started = offer_for("4.6.0", "4.7.0", None).expect("test update offer");
        let mut state = ShellUpdateUiState {
            upgrading: Some(UpgradeTarget::Default(started)),
            ..ShellUpdateUiState::default()
        };
        assert!(
            state.blocks_packages(),
            "a package job would replace the upgrade"
        );

        state.upgrading = None;
        assert!(
            !state.blocks_packages(),
            "the upgrade released the job slot"
        );
    }

    #[test]
    fn running_default_card_keeps_its_offer_until_completion() {
        let started = offer_for("4.6.0", "4.7.0", None).expect("test update offer");
        let newer = || offer_for("4.6.0", "4.7.1", None);
        let mut state = ShellUpdateUiState {
            upgrading: Some(UpgradeTarget::Default(started.clone())),
            ..ShellUpdateUiState::default()
        };
        assert_eq!(state.release_offer(|| None), Some(started.clone()));
        assert_eq!(state.release_offer(newer), Some(started));

        state.upgrading = None;
        assert_eq!(state.release_offer(|| None), None);
        assert_eq!(state.release_offer(newer), newer());
    }

    #[test]
    fn running_main_card_keeps_its_commit_until_completion() {
        let started = "05f20b064770";
        let newer = "2792ce0f011c";
        let mut state = ShellUpdateUiState {
            upgrading: Some(UpgradeTarget::Main(started.into())),
            ..ShellUpdateUiState::default()
        };
        assert_eq!(state.main_commit(None), Some(started));
        assert_eq!(state.main_commit(Some(newer)), Some(started));

        state.upgrading = None;
        assert_eq!(state.main_commit(None), None);
        assert_eq!(state.main_commit(Some(newer)), Some(newer));
    }

    #[test]
    fn update_action_main_upgrade_result_follows_the_main_build_that_runs_afterwards() {
        // Offered 05f20b064770 from the last check; main moved on before the
        // upgrade ran, so it installed a later commit.
        let before = "4.9.0-main.25ed7a9f4927";
        let target = "05f20b064770";
        assert_eq!(UpgradeChannel::Main.args(), &["--channel", "main"]);
        let outcome = |ok, running| {
            upgrade_outcome(
                ok,
                target,
                UpgradeChannel::Main,
                Some(before),
                running,
                None,
            )
        };
        for running in [target, "4.9.0-main.05f20b064770", "4.9.0-main.2792ce0f011c"] {
            assert_eq!(
                outcome(true, Some(running)),
                UpgradeOutcome::Installed(running.into())
            );
        }
        // Nothing changed, or what runs is not a main build.
        for running in [
            before,
            "4.9.0",
            "4.9.0-beta.2792ce0f011c",
            "4.9.0-main.2792ce0f011c.1",
        ] {
            assert_eq!(
                outcome(true, Some(running)),
                UpgradeOutcome::Stale {
                    running: running.into(),
                    launcher: "gentle-shell".into(),
                }
            );
        }
        // Gentle Shell moved, but the upgrade exited with an error after it.
        assert_eq!(
            outcome(false, Some("4.9.0-main.6d47decde29c")),
            UpgradeOutcome::Incomplete("4.9.0-main.6d47decde29c".into())
        );
        assert_eq!(outcome(false, Some(before)), UpgradeOutcome::Failed);
        assert_eq!(outcome(false, Some("4.9.0")), UpgradeOutcome::Failed);
        assert_eq!(outcome(true, None), UpgradeOutcome::Failed);
        assert_eq!(outcome(false, None), UpgradeOutcome::Failed);
        // The version before the upgrade could not be read: any main build counts.
        assert_eq!(
            upgrade_outcome(
                true,
                target,
                UpgradeChannel::Main,
                None,
                Some("4.9.0-main.2792ce0f011c"),
                None
            ),
            UpgradeOutcome::Installed("4.9.0-main.2792ce0f011c".into())
        );
    }

    #[test]
    fn update_action_release_upgrade_result_follows_what_the_app_runs_afterwards() {
        use std::path::Path;
        let old = Path::new("/fnm/bin/gentle-shell");
        assert_eq!(UpgradeChannel::Default.args(), &[] as &[&str]);
        let outcome = |ok, running| {
            upgrade_outcome(
                ok,
                "4.7.0",
                UpgradeChannel::Default,
                Some("4.6.0"),
                running,
                Some(old),
            )
        };
        assert_eq!(
            outcome(true, Some("4.7.0")),
            UpgradeOutcome::Installed("4.7.0".into())
        );
        for running in ["4.6.0", "4.9.0-main.4.7.0"] {
            assert_eq!(
                outcome(true, Some(running)),
                UpgradeOutcome::Stale {
                    running: running.into(),
                    launcher: "/fnm/bin/gentle-shell".into(),
                }
            );
        }
        // A release upgrade that exits with an error stays failed, whatever runs.
        for running in ["4.7.0", "4.9.0-main.2792ce0f011c"] {
            assert_eq!(outcome(false, Some(running)), UpgradeOutcome::Failed);
        }
        assert_eq!(outcome(true, None), UpgradeOutcome::Failed);
    }
}
