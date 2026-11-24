//! One tab: a `gentle-shell --mode rpc` backend and its conversation UI.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::TextViewState;
use gpui_kit::*;
use pi_rpc::desktop_rpc::DesktopMethod;
use pi_rpc::{UiAnswer, UiMethod};

use crate::shared::ui;
use gentle_core::transcript::{NoticeLevel, Transcript};

mod agents;
mod attachments;
mod builtins;
mod events;
pub mod input_style;
mod lifecycle;
mod reads;
mod rpc;
mod state;
mod submit;
mod view;
#[cfg(test)]
mod view_tests;

pub use agents::{AgentsPanel, AgentsTab};
pub(crate) use reads::READ_DECIDE_WAIT;
pub(crate) use state::desktop::reads::{
    ReadFailure, ReadOutcome, ReadRequest, ReadSupport, ReadTicket,
};

actions!(session, [Abort, Stop, ToggleAgents, NewSession]);

pub const CONTEXT: &str = "GentleSession";

/// After an Esc that closed something, how long further Esc presses only
/// close things and never stop the turn.
const ESCAPE_SETTLE: std::time::Duration = std::time::Duration::from_millis(600);

/// Requests a tab makes of its window.
pub enum SessionEvent {
    OpenProviders,
    /// An extension's `notify`, for the window's notification center.
    /// Warnings and errors are also toasted by the tab itself.
    Notice {
        level: NoticeLevel,
        message: String,
    },
    /// Something the window chrome shows may have changed: [`SessionView::title`],
    /// [`SessionView::is_running`], [`SessionView::session_file`],
    /// [`SessionView::cwd`] or [`SessionView::model_choices`]. Streaming
    /// output does not emit it, so the window need not redraw per token.
    ChromeChanged,
    /// A Gentle Shell command whose UI is terminal-only was run (typed or
    /// from the palette): open the app's own screen for it instead.
    OpenScreen(gentle_core::commands::Screen),
    /// The session's Engram status was clicked: the Memory screen on the
    /// project and memory it names.
    OpenMemory(gentle_core::engram::MemoryTarget),
    /// The thinking level was set (and the backend took it): the window
    /// plays that level's surge.
    Effort(String),
    /// Gentle Design's canvas opened, refreshed or stopped: the window keeps
    /// it in its instance tray (or reloads the card already showing it).
    DesignCanvas(crate::features::browser::CanvasNotice),
    /// A shell tool is starting an emulator or simulator: the window follows
    /// that device in its instance tray, without opening a card.
    Device(gentle_core::devices::LaunchIntent),
    /// A shell tool drove a device (adb, simctl, Maestro…): the tray shows it
    /// as what the agent is doing with that device.
    DeviceCommand(gentle_core::devices::DeviceCommand),
    /// The user asked for a page (`/web`): the window shows it in a card.
    OpenWeb(String),
    /// A tool printed a local web app's URL: the window keeps it in its
    /// instance tray until the user looks.
    AgentWeb(String),
    /// Show this file in the file viewer, at this line (a code reference
    /// in a generative UI surface).
    OpenFile {
        path: std::path::PathBuf,
        line: Option<u32>,
    },
}

/// What the window reads from a tab, to emit [`SessionEvent::ChromeChanged`]
/// only when it differs.
#[derive(PartialEq)]
struct Chrome {
    title: String,
    running: bool,
    needs_you: bool,
    session_file: Option<PathBuf>,
    models: u64,
    skills: usize,
}

pub struct SessionView {
    cwd: PathBuf,
    backend: state::Backend,
    /// Gentle Desktop's RPC channel on this backend (see [`state::desktop`]).
    desktop: state::desktop::Desktop,
    /// Gentle Desktop's permission-mode state on this backend (see
    /// [`state::desktop::permission`]); it borrows `desktop`'s session.
    permission: state::desktop::permission::Permission,
    /// A `reload_credentials` sent after a sign-in, until it answers: a
    /// failure falls back to restarting the tab.
    credentials_reload: Option<ReadTicket>,
    transcript: Transcript,
    /// Rendered Markdown per (item, block): the state, the byte length of
    /// the shown text already pushed and how many Mermaid diagrams it showed
    /// drawn (a new drawing replaces source, so the text is set again).
    markdown: HashMap<(usize, usize), (Entity<TextViewState>, usize, usize)>,
    /// Which code spans in the answers name a file under `cwd` (see
    /// `view::paths`); remade with `cwd`.
    paths: std::sync::Arc<view::PathLookup>,
    /// The conversation's Mermaid diagrams, drawn by the diagram renderer.
    diagrams: std::sync::Arc<crate::features::files::MermaidDiagrams>,
    /// The `data:` images messages you sent show, decoded once.
    bubble_images: std::sync::Arc<crate::features::files::EmbeddedImages>,
    expanded: HashSet<usize>,
    /// Generative UI surfaces the agent showed (`gentle_ui`), rebuilt
    /// from the transcript.
    genui: crate::features::genui::GenUi,
    /// The Agent UI mode this tab's backend was last given (at launch or
    /// by command); a different choice in Configuration gets sent to it.
    genui_mode: gentle_core::genui::mode::Mode,
    /// The canvas this agent was opened beside to read and draw on (the
    /// canvas extension's tools act on it).
    canvas: Option<PathBuf>,
    /// Where this tab's agent runs (`features::backend`), fixed when it
    /// first starts so a restart resumes its conversation in the same place.
    runs_on: Option<gentle_core::backend::Backend>,
    /// This conversation is on screen with nothing over the window (the
    /// workspace says so): its web views may show.
    web_shown: bool,
    composer: Entity<TextareaState>,
    /// The composer has the keyboard: its border takes the accent.
    composer_focused: bool,
    /// When the composer last took the keyboard: its pulse plays for
    /// `PULSE` from then (see `composer_shell`), a frame at a time.
    composer_pulse: Option<std::time::Instant>,
    /// Pixel kisses floating up from the caret as you type: when each was
    /// blown, where, and its number (for its own drift and size).
    kisses: Vec<(std::time::Instant, Point<Pixels>, usize)>,
    kisses_blown: usize,
    /// The composer's text length last time, to tell typing from deleting.
    composer_len: usize,
    /// Images waiting to go with the next prompt; `[Image #N]` is the N-th.
    attachments: Vec<attachments::Attachment>,
    /// Long pastes waiting to go with the next prompt, each sent in place
    /// of its `[Pasted text #N]` token: the N-th is `pasted[N - 1]`.
    pasted: Vec<attachments::PastedText>,
    /// Decoded conversation images, by data hash.
    image_cache: attachments::ImageCache,
    extension: state::ExtensionUi,
    dialog_input: Entity<TextareaState>,
    activity: state::TurnActivity,
    agents_panel: AgentsPanel,
    /// This session's live permission mode (Gentle Shell's `gentle:mode`
    /// status), and the default new sessions start in. Both follow Desktop
    /// RPC's `get_permission_mode` / `set_permission_mode` /
    /// `permission_mode_changed` once that slice answers. `default_mode`
    /// starts as `permission-mode.json`, only a placeholder until then and
    /// the source for an older Shell without Desktop RPC.
    mode: Option<gentle_core::gentle_state::PermissionMode>,
    default_mode: Option<gentle_core::gentle_state::PermissionMode>,
    _agents_ticker: Task<()>,
    /// The background job output on screen and its poll.
    job_output: agents::JobOutputView,
    todo: state::TodoIsland,
    git: state::GitStatus,
    models: state::Models,
    usage: state::Usage,
    commands: Vec<(String, String)>,
    /// The prompts sent from this tab, for ↑/↓ in the composer.
    prompt_history: gentle_core::prompt_history::PromptHistory,
    /// Each skill's folder, by name (without `skill:`).
    skill_dirs: Vec<(String, PathBuf)>,
    session_name: Option<String>,
    session_file: Option<PathBuf>,
    title: Option<String>,
    active_shell: Option<usize>,
    banner: crate::shared::banner::BannerConfig,
    banner_reveal: f32,
    _banner_anim: Task<()>,
    /// The conversation's virtualized list and its per-item display caches.
    list: view::TranscriptList,
    /// Where the live indicators of the conversation and the composer sit,
    /// and the layer that draws them moving (see [`crate::shared::overlay`]).
    live_slots: crate::shared::overlay::LiveSlots,
    live_layer: Entity<crate::shared::overlay::LiveLayer>,
    /// What the window was last told about: see [`SessionEvent::ChromeChanged`].
    chrome: Option<Chrome>,
    /// The pane's own width, measured each frame; narrow layouts adapt to it.
    pane_width: std::rc::Rc<std::cell::Cell<f32>>,
    focus_handle: FocusHandle,
    _io: Task<()>,
    _subscriptions: Vec<Subscription>,
    /// View-only state: live timers, the prompt in flight, picker highlights.
    live: view::Live,
    model_search: Entity<InputState>,
    /// The `notify` output of the extension command last sent.
    capture: Option<builtins::Capture>,
    /// Numbers [`builtins::Capture`]s, to match Pi's replies.
    capture_seq: u64,
    /// This tab's window, for work that finishes outside a window update.
    window_handle: AnyWindowHandle,
    /// The local UTC offset in seconds, read once per tab: the plan-limits
    /// chip's reset clock uses it (never a `date` call per frame).
    local_offset: std::cell::OnceCell<i64>,
    /// When the plan-limits chip last asked for the subscription usage while
    /// it had nothing to show: the chip is hidden without data, so it can't
    /// wait for its card to open (at most once per `PLAN_SEED_EVERY`).
    plan_seed_asked: std::cell::Cell<Option<std::time::Instant>>,
}

impl EventEmitter<SessionEvent> for SessionView {}

impl SessionView {
    /// `start_now: false` defers the backend until [`Self::ensure_started`]
    /// (tabs that are not shown yet); `prompt` and a resumed session still
    /// run once it starts.
    pub fn new(
        cwd: PathBuf,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
        start_now: bool,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .submit_on_enter(true)
                .placeholder("What are we cooking?")
        });
        let dialog_input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 8));
        let model_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models…"));
        // GPUI keeps decoded images until told to drop them.
        cx.on_release(|this, cx| {
            this.image_cache.release_all(cx);
            for attachment in this.attachments.drain(..) {
                attachment.image.remove_asset(cx);
            }
        })
        .detach();
        let weak = cx.entity().downgrade();
        crate::features::diagnostics::watch_tab(weak.clone());
        let subscriptions = vec![
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { shift: false, .. } => this.submit(window, cx),
                    InputEvent::Change => {
                        this.sync_pasted(window, cx);
                        this.blow_kiss(cx);
                        cx.notify();
                    }
                    InputEvent::Focus | InputEvent::Blur => {
                        this.composer_focused = matches!(event, InputEvent::Focus);
                        if this.composer_focused {
                            this.start_composer_pulse(cx);
                        }
                        cx.notify();
                    }
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &dialog_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter {
                        secondary,
                        shift: false,
                    } = event
                    {
                        let submits = match this.extension.dialogs.front().map(|d| &d.method) {
                            Some(UiMethod::Input { .. }) => true,
                            Some(UiMethod::Editor { .. }) => *secondary,
                            _ => false,
                        };
                        if submits {
                            let value = this.dialog_input.read(cx).value().to_string();
                            this.answer_dialog(UiAnswer::Value(value), window, cx);
                        }
                    }
                },
            ),
            cx.subscribe(&model_search, |this, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.model_query_changed(cx);
                }
            }),
            // The model picker's and the `/` menu's ↑/↓/Enter must beat the
            // focused search field and composer.
            cx.intercept_keystrokes(move |event, window, cx| {
                let consumed = weak
                    .update(cx, |this, cx| {
                        this.model_picker_key(&event.keystroke, window, cx)
                            || this.history_key(&event.keystroke, window, cx)
                            || this.slash_menu_key(&event.keystroke, window, cx)
                    })
                    .unwrap_or(false);
                if consumed {
                    cx.stop_propagation();
                }
            }),
            // Configuration › Agent UI changed: tell this tab's extension.
            crate::features::genui::mode::follow(cx, |this, _, cx| this.sync_genui_mode(cx)),
            // The diagram renderer was installed (or failed), or the theme
            // turned light or dark: draw Mermaid blocks already shown in its
            // colors, or show why not.
            cx.observe_global::<crate::features::files::DiagramRendererState>(|this, cx| {
                this.redraw_diagrams(cx)
            }),
            cx.observe_global::<crate::shared::themes::Themes>(|this, cx| this.redraw_diagrams(cx)),
            // Configuration › Appearance › Input style changed: draw in it.
            input_style::follow(cx),
        ];
        let live_slots = crate::shared::overlay::LiveSlots::default();
        let live_layer = cx.new(|_| crate::shared::overlay::LiveLayer::new(live_slots.clone()));
        let paths = std::sync::Arc::new(view::PathLookup::new(cwd.clone()));
        let mut this = Self {
            cwd,
            backend: state::Backend::new(prompt),
            desktop: state::desktop::Desktop::default(),
            permission: state::desktop::permission::Permission::default(),
            transcript: Transcript::default(),
            markdown: HashMap::new(),
            paths,
            diagrams: Default::default(),
            bubble_images: Default::default(),
            expanded: HashSet::new(),
            genui: Default::default(),
            genui_mode: crate::features::genui::mode::mode(cx),
            canvas: None,
            runs_on: None,
            web_shown: false,
            composer,
            composer_focused: false,
            composer_pulse: None,
            kisses: Vec::new(),
            kisses_blown: 0,
            composer_len: 0,
            attachments: Vec::new(),
            pasted: Vec::new(),
            image_cache: attachments::ImageCache::default(),
            extension: state::ExtensionUi::default(),
            dialog_input,
            activity: state::TurnActivity::default(),
            agents_panel: AgentsPanel::default(),
            mode: None,
            default_mode: crate::shared::gentle_state::read_default_mode(
                &pi_rpc::resolve_homes().config,
            ),
            _agents_ticker: Task::ready(()),
            job_output: agents::JobOutputView::default(),
            todo: state::TodoIsland::default(),
            git: state::GitStatus::default(),
            models: state::Models::default(),
            usage: state::Usage::default(),
            pane_width: std::rc::Rc::new(std::cell::Cell::new(10_000.)),
            commands: Vec::new(),
            prompt_history: Default::default(),
            skill_dirs: Vec::new(),
            session_name: None,
            session_file: None,
            title: None,
            active_shell: None,
            credentials_reload: None,
            banner: crate::shared::banner::read_config(&pi_rpc::resolve_homes().config),
            banner_reveal: 0.0,
            _banner_anim: Task::ready(()),
            list: view::TranscriptList::new(),
            live_slots,
            live_layer,
            chrome: None,
            focus_handle: cx.focus_handle(),
            _io: Task::ready(()),
            _subscriptions: subscriptions,
            live: view::Live::default(),
            model_search,
            capture: None,
            capture_seq: 0,
            window_handle: window.window_handle(),
            local_offset: std::cell::OnceCell::new(),
            plan_seed_asked: std::cell::Cell::new(None),
        };
        if start_now {
            this.ensure_started(window, cx);
        }
        this
    }

    /// The layer this view's live indicators move on (pulses, shimmers,
    /// spinners, the composer's orbit; see [`crate::shared::overlay`]).
    /// Whatever draws this view draws the layer right after it, over its
    /// bounds, outside this view: a frame of the layer then redraws the
    /// layer and the views above it, and this view is reused from cache.
    /// Without it the indicators don't show.
    pub fn live_layer(&self) -> Entity<crate::shared::overlay::LiveLayer> {
        self.live_layer.clone()
    }

    pub fn title(&self) -> String {
        self.session_name
            .clone()
            .or_else(|| self.title.clone())
            .unwrap_or_else(|| ui::split_path(&self.cwd).1)
    }

    pub fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    /// Pairs this agent with a canvas; set before its backend starts.
    pub fn set_canvas(&mut self, path: PathBuf) {
        self.canvas = Some(path);
    }

    pub fn is_running(&self) -> bool {
        self.transcript.running
    }

    /// Something was said here, a turn is running or a saved conversation
    /// is about to be resumed: moving this agent to another folder loses it.
    pub fn has_conversation(&self) -> bool {
        self.transcript.running || !self.hero_shown() || self.backend.pending_session.is_some()
    }

    /// A dialog (a question, a confirmation) waits for an answer.
    pub fn needs_you(&self) -> bool {
        !self.extension.dialogs.is_empty()
    }

    /// Whether this conversation is on screen with nothing over the window;
    /// an expanded HTML surface's web view hides otherwise.
    pub fn set_web_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.web_shown != shown {
            self.web_shown = shown;
            self.genui.set_obscured(!shown, cx);
            cx.notify();
        }
    }

    pub fn composer(&self) -> Entity<TextareaState> {
        self.composer.clone()
    }

    /// Show an informational line in the conversation.
    pub fn note(&mut self, text: String, cx: &mut Context<Self>) {
        self.transcript.notice(NoticeLevel::Info, text);
        cx.notify();
    }

    /// Commands for the `/` menu and the palette: Pi's built-ins the app
    /// runs, then the other Pi built-ins Gentle Shell's session feed lists
    /// (terminal-only ones marked), then Gentle Shell's, without the ones
    /// that do nothing here.
    pub fn commands(&self) -> Vec<gentle_core::commands::SlashCommand> {
        gentle_core::commands::menu(
            builtins::BUILTINS,
            &self.desktop.sections().pi_commands(),
            &self.commands,
        )
    }

    /// Where skill `name` lives, if this tab's Gentle Shell has it.
    pub fn skill_dir(&self, name: &str) -> Option<PathBuf> {
        self.skill_dirs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, dir)| dir.clone())
    }

    /// The conversation this tab shows: a resume still in flight (or waiting
    /// for the backend to start) wins over the live session file.
    pub fn session_file(&self) -> Option<PathBuf> {
        self.backend
            .pending_session
            .clone()
            .or_else(|| self.session_file.clone())
    }

    /// Current model, then the reported default, then by provider, keeping catalog order.
    fn ordered_models(&self) -> Vec<state::ModelInfo> {
        let mut models = self.models.available.clone();
        let current = self
            .models
            .current
            .as_ref()
            .map(|m| format!("{}/{}", m.provider, m.id));
        // A backend restart or exit resets the channel before the next refresh.
        let default = if self.read_support(DesktopMethod::ListAgentDefinitions) == ReadSupport::Rpc
        {
            self.models.default.as_deref()
        } else {
            None
        };
        gentle_core::profiles::sort_like_shell(
            &mut models,
            |m| (m.provider.clone(), m.id.clone()),
            current.as_deref(),
            default,
        );
        models
    }

    pub fn model_choices(&self) -> Vec<(String, String, String, bool)> {
        self.ordered_models()
            .iter()
            .map(|m| {
                (
                    m.provider.clone(),
                    m.id.clone(),
                    m.name.clone(),
                    m.reasoning,
                )
            })
            .collect()
    }

    /// Opens the Agents window on `tab`.
    pub fn open_agents_panel(&mut self, tab: AgentsTab, cx: &mut Context<Self>) {
        self.agents_panel.tab = tab;
        self.open_agents(None, cx);
    }

    /// Tell the window when something it shows changed.
    fn sync_chrome(&mut self, cx: &mut Context<Self>) {
        let chrome = Chrome {
            title: self.title(),
            running: self.is_running(),
            needs_you: self.needs_you(),
            session_file: self.session_file(),
            models: self.models.rev,
            skills: self.skill_dirs.len(),
        };
        if self.chrome.as_ref() != Some(&chrome) {
            self.chrome = Some(chrome);
            cx.emit(SessionEvent::ChromeChanged);
        }
    }

    fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        self.composer
            .update(cx, |state, cx| state.focus(window, cx));
    }

    /// A warning toast, as extension warnings show.
    fn warn(&self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        window.push_notification(Notification::warning(message), cx);
    }

    /// Put `text` on the clipboard and say so.
    fn copy_text(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.toast(Notification::success("Copied to the clipboard."), cx);
    }

    /// Run `f` with this tab's window, after the current update.
    fn in_window(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let handle = self.window_handle;
        cx.spawn(async move |this, cx| {
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| f(this, window, cx));
            });
        })
        .detach();
    }

    /// A toast in this tab's window, from code without one at hand.
    fn toast(&self, note: Notification, cx: &mut Context<Self>) {
        self.in_window(cx, move |_, window, cx| window.push_notification(note, cx));
    }

    /// Whether this tab's window is frontmost and focused (false inside an
    /// update of that window: use `Window::is_window_active` there).
    fn window_active(&self, cx: &mut App) -> bool {
        cx.update_window(self.window_handle, |_, window, _| window.is_window_active())
            .unwrap_or(false)
    }

    fn on_new_session(&mut self, _: &NewSession, window: &mut Window, cx: &mut Context<Self>) {
        self.new_session(window, cx);
    }

    fn toggle_agents(&mut self, _: &ToggleAgents, _: &mut Window, cx: &mut Context<Self>) {
        if self.agents_panel.open {
            self.close_agents(cx);
        } else {
            self.open_agents(None, cx);
        }
    }

    /// Esc: close what's open (the Agents panel, then a dialog, then an
    /// expanded generative UI surface that doesn't have the keyboard; the
    /// `/` menu and the model picker close earlier, from the composer), and
    /// with nothing open stop the running turn or `!command`, as in Gentle
    /// Shell's terminal. An Esc right after one that closed something only
    /// propagates: a quick run of presses to close things never stops the
    /// agent by accident.
    fn abort(&mut self, _: &Abort, window: &mut Window, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        let closed = if self.agents_panel.open {
            self.close_agents(cx);
            true
        } else if !self.extension.dialogs.is_empty() {
            self.answer_dialog(UiAnswer::Cancelled, window, cx);
            true
        } else if !self.slash_menu_open(cx) && self.genui.collapse_on_escape(window, cx) {
            // The `/` menu, open over an expanded surface, closes first.
            cx.notify();
            true
        } else {
            false
        };
        if closed {
            self.live.escape_closed = Some(now);
            return;
        }
        let settling = self
            .live
            .escape_closed
            .is_some_and(|at| now.duration_since(at) < ESCAPE_SETTLE);
        if self.stoppable() && !settling {
            self.stop(cx);
        } else {
            cx.propagate();
        }
    }

    /// ⌘. (Ctrl+. elsewhere): stop the running turn or `!command`.
    fn on_stop(&mut self, _: &Stop, _: &mut Window, cx: &mut Context<Self>) {
        if self.stoppable() {
            self.stop(cx);
        } else {
            cx.propagate();
        }
    }
}

/// The wire permission mode for the picker's mode.
pub(in crate::features::session) fn to_desktop_mode(
    mode: gentle_core::gentle_state::PermissionMode,
) -> pi_rpc::desktop_rpc::PermissionMode {
    match mode {
        gentle_core::gentle_state::PermissionMode::Normal => {
            pi_rpc::desktop_rpc::PermissionMode::Normal
        }
        gentle_core::gentle_state::PermissionMode::Auto => {
            pi_rpc::desktop_rpc::PermissionMode::Auto
        }
        gentle_core::gentle_state::PermissionMode::Yolo => {
            pi_rpc::desktop_rpc::PermissionMode::Yolo
        }
        gentle_core::gentle_state::PermissionMode::SuperYolo => {
            pi_rpc::desktop_rpc::PermissionMode::SuperYolo
        }
    }
}

/// The picker's mode for a server-reported wire mode.
pub(in crate::features::session) fn from_desktop_mode(
    mode: pi_rpc::desktop_rpc::PermissionMode,
) -> gentle_core::gentle_state::PermissionMode {
    match mode {
        pi_rpc::desktop_rpc::PermissionMode::Normal => {
            gentle_core::gentle_state::PermissionMode::Normal
        }
        pi_rpc::desktop_rpc::PermissionMode::Auto => {
            gentle_core::gentle_state::PermissionMode::Auto
        }
        pi_rpc::desktop_rpc::PermissionMode::Yolo => {
            gentle_core::gentle_state::PermissionMode::Yolo
        }
        pi_rpc::desktop_rpc::PermissionMode::SuperYolo => {
            gentle_core::gentle_state::PermissionMode::SuperYolo
        }
    }
}

#[cfg(test)]
mod tests {
    use gentle_core::gentle_state::PermissionMode;

    use super::{from_desktop_mode, to_desktop_mode};

    #[test]
    fn desktop_permission_modes_round_trip() {
        for mode in PermissionMode::ALL {
            assert_eq!(from_desktop_mode(to_desktop_mode(mode)), mode);
        }
    }
}
