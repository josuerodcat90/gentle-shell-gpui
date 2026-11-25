//! The window, after Tern: named workspaces (one shown at a time), each with
//! its own tabs; every tab tiles panes (agents, terminals, files) in a split
//! tree with draggable dividers. Vertical tabs live on the left, Files / Changes /
//! Branches on the right, and the layout is saved to disk as it changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, TitleBar, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::features::canvas::{CanvasEvent, CanvasView};
use crate::features::docs::{DocsEvent, DocsView};
use crate::features::explorer::{ExplorerEvent, ExplorerView};
use crate::features::memory::{MemoryEvent, MemoryView};
use crate::features::notices::{self, Notices};
use crate::features::onboarding::{OnboardingEvent, OnboardingView, Probe};
use crate::features::palette::{
    Activity, Category, PaletteAction, PaletteEvent, PaletteInput, PaletteView, WorkspaceRow,
};
use crate::features::profiles::{ModelChoice, ProfilesEvent, ProfilesView};
use crate::features::providers::{ProvidersEvent, ProvidersView};
use crate::features::review::{ReviewEvent, ReviewView};
use crate::features::session::{AgentsTab, SessionEvent, SessionView};
use crate::features::sessions::{SessionsEvent, SessionsView};
use crate::features::settings::{SettingsEvent, SettingsView};
use crate::features::tower::TowerView;
use crate::features::usage::{UsageEvent, UsageView};
use crate::features::welcome::{WelcomeEvent, WelcomeView};
use crate::features::workspaces::{
    self, Badge, SavedPane, SavedState, SavedTab, SavedWorkspace, TabLayout,
};
use crate::shared::pane::{
    AskAgent, CloseViewer, OpenCommitDiff, OpenTerminalAt, PaneFactories, PaneId, PaneItem,
    PaneKind, PaneTree, ShowInPane, SplitAxis,
};
use crate::shared::ui;
use gentle_core::commands::Screen;

mod actions;
mod badge;
mod closed;
mod commands;
mod layout;
mod menu;
mod overlays;
mod panes;
mod persistence;
mod render;
mod sidebar_motion;
mod sidebars;
mod spaces;
mod stages;
mod tab_panel;
mod tabs;
mod view;

use menu::{OpenMenu, WorkspaceRename};

use crate::features::devices::{DevicePanel, DevicePanelEvent};
use crate::features::music::{Music, MusicChanged};
use layout::*;
use stages::Stage;

actions!(
    gentle,
    [
        NewTab,
        NewTerminalTab,
        NewCanvasTab,
        CloseTab,
        NextTab,
        PrevTab,
        SplitRight,
        TilePane,
        SplitDown,
        NewAgentPane,
        NewTerminalPane,
        ClosePane,
        ReopenClosed,
        FocusNextPane,
        FocusPrevPane,
        ToggleSidebar,
        OpenProviders,
        OpenSessions,
        OpenPalette,
        OpenWorkspaces,
        OpenSettings,
        OpenExplorer,
        CloseOverlay,
        Quit
    ]
);

/// The vertical tabs sidebar's width (px at 100 % interface size) as an
/// icon rail; it has no resize handle.
const COMPACT_SIDEBAR_WIDTH: f32 = 48.;

/// What the window tells whoever embeds things in it (the sidebar's owner).
pub enum WorkspaceEvent {
    /// The shown workspace changed; its root folder.
    RootChanged(PathBuf),
    /// Whether the right-side Files panel is shown.
    FilesVisibilityChanged(bool),
}

/// A window-level screen drawn over the active tab.
enum Overlay {
    Providers(Entity<ProvidersView>),
    Sessions(Entity<SessionsView>),
    Palette(Entity<PaletteView>),
    Explorer(Entity<ExplorerView>),
    Usage(Entity<UsageView>),
    Memory(Entity<MemoryView>),
    Docs(Entity<DocsView>),
    Settings(Entity<SettingsView>),
    Profiles(Entity<ProfilesView>),
    Review(Entity<ReviewView>),
    Onboarding(Entity<OnboardingView>),
    Welcome(Entity<WelcomeView>),
}

struct Pane {
    item: PaneItem,
    /// Set for agent panes.
    session: Option<Entity<SessionView>>,
    /// Folder of a terminal; parent folder of a file.
    cwd: PathBuf,
    /// The file a file pane shows.
    path: Option<PathBuf>,
    /// Last conversation file seen, to save when it changes.
    session_file: Option<PathBuf>,
    /// What the tab bar shows for this pane, kept here so drawing the chrome
    /// never reads the views of tabs that aren't shown (reading an entity
    /// while drawing makes its every notify redraw the window). Agents
    /// refresh it on `SessionEvent::ChromeChanged`, everything on the title poll.
    title: SharedString,
    running: bool,
    /// An agent waits on a dialog only you can answer.
    needs_you: bool,
    _subscriptions: Vec<Subscription>,
}

impl Pane {
    /// Re-read the title and activity; whether any changed.
    fn sync_chrome(&mut self, cx: &App) -> bool {
        let title = (self.item.title)(cx);
        let (running, needs_you) = self.session.as_ref().map_or((false, false), |s| {
            let s = s.read(cx);
            (s.is_running(), s.needs_you())
        });
        let changed = title != self.title || running != self.running || needs_you != self.needs_you;
        self.title = title;
        self.running = running;
        self.needs_you = needs_you;
        changed
    }
}

/// A tab is either restored but not yet built (it holds its saved layout
/// until first shown), or has live panes.
pub(super) enum TabState {
    Unbuilt(SavedTab),
    Built {
        tree: PaneTree<PaneId>,
        focused: PaneId,
    },
}

impl TabState {
    pub(super) fn is_built(&self) -> bool {
        matches!(self, TabState::Built { .. })
    }

    /// Live panes; none while unbuilt.
    pub(super) fn leaves(&self) -> Vec<PaneId> {
        self.built_tree().map(PaneTree::leaves).unwrap_or_default()
    }

    /// Number of panes the tab tiles, built or not.
    pub(super) fn len(&self) -> usize {
        match self {
            TabState::Built { tree, .. } => tree.len(),
            TabState::Unbuilt(saved) => saved.layout.len(),
        }
    }

    pub(super) fn contains(&self, pane: &PaneId) -> bool {
        self.built_tree().is_some_and(|t| t.contains(pane))
    }

    pub(super) fn focused(&self) -> Option<PaneId> {
        match self {
            TabState::Built { focused, .. } => Some(*focused),
            TabState::Unbuilt(_) => None,
        }
    }

    /// The focused pane of an unbuilt tab, as saved.
    pub(super) fn saved_focused(&self) -> Option<&SavedPane> {
        let TabState::Unbuilt(saved) = self else {
            return None;
        };
        let last = saved.layout.len().saturating_sub(1);
        saved.layout.leaf(saved.focused.min(last))
    }

    pub(super) fn built_tree(&self) -> Option<&PaneTree<PaneId>> {
        match self {
            TabState::Built { tree, .. } => Some(tree),
            TabState::Unbuilt(_) => None,
        }
    }

    pub(super) fn built_tree_mut(&mut self) -> Option<&mut PaneTree<PaneId>> {
        match self {
            TabState::Built { tree, .. } => Some(tree),
            TabState::Unbuilt(_) => None,
        }
    }

    pub(super) fn unbuilt(&self) -> Option<&SavedTab> {
        match self {
            TabState::Unbuilt(saved) => Some(saved),
            TabState::Built { .. } => None,
        }
    }

    pub(super) fn set_built(&mut self, tree: PaneTree<PaneId>, focused: PaneId) {
        *self = TabState::Built { tree, focused };
    }

    pub(super) fn set_focused(&mut self, pane: PaneId) {
        if let TabState::Built { focused, .. } = self {
            *focused = pane;
        }
    }
}

struct Tab {
    id: usize,
    pinned: bool,
    state: TabState,
}

impl Tab {
    fn is_built(&self) -> bool {
        self.state.is_built()
    }

    fn leaves(&self) -> Vec<PaneId> {
        self.state.leaves()
    }

    fn focused(&self) -> Option<PaneId> {
        self.state.focused()
    }

    fn built_tree(&self) -> Option<&PaneTree<PaneId>> {
        self.state.built_tree()
    }

    fn built_tree_mut(&mut self) -> Option<&mut PaneTree<PaneId>> {
        self.state.built_tree_mut()
    }
}

/// One named workspace.
struct Space {
    name: String,
    root: PathBuf,
    last_active: u64,
    tabs: Vec<Tab>,
    active: usize,
    /// The badge's chosen icon and color.
    badge: Badge,
    /// Whether this workspace's sessions are visible in the sidebar.
    sidebar_expanded: bool,
}

impl Space {
    fn new(name: String, root: PathBuf) -> Self {
        Self {
            name,
            root,
            last_active: workspaces::now(),
            tabs: Vec::new(),
            active: 0,
            badge: Badge::default(),
            sidebar_expanded: true,
        }
    }
}

/// Dragged by a divider: which split it resizes.
#[derive(Clone)]
struct DividerDrag {
    tab: usize,
    path: Vec<bool>,
}

/// Which sidebar a drag handle is resizing.
#[derive(Clone, Copy)]
enum Side {
    Tabs,
    Files,
}

/// Dragged by a sidebar's inner edge.
#[derive(Clone)]
struct SidebarDrag {
    side: Side,
    owner: gpui::EntityId,
    revision: std::cell::Cell<u64>,
    gesture: std::cell::RefCell<gentle_core::workspaces::SidebarGesture>,
}

impl SidebarDrag {
    fn new(side: Side, owner: gpui::EntityId, revision: u64) -> Self {
        Self {
            side,
            owner,
            revision: std::cell::Cell::new(revision),
            gesture: Default::default(),
        }
    }
}

/// Dragged by a pane's grip pill: dropped near another pane's edge it moves
/// to that side, in the middle the two swap.
#[derive(Clone)]
struct PaneDrag {
    pane: PaneId,
    title: SharedString,
}

/// Dragged by a tab: dropped on another tab it takes that tab's place.
#[derive(Clone)]
struct TabDrag {
    tab: usize,
    title: SharedString,
}

/// Dragged by a workspace row in the vertical sidebar: dropped on another
/// workspace row it reorders the workspaces.
#[derive(Clone)]
struct WorkspaceDrag {
    index: usize,
    name: SharedString,
}

/// A multi-selection of tabs in the shown workspace. Ids are stable while
/// indices move, so the selection survives pin/reorder/close of non-selected
/// tabs and is cleared when one of its tabs disappears.
#[derive(Clone)]
struct TabSelection {
    space: usize,
    anchor: usize,
    selected: std::collections::HashSet<usize>,
}

/// The inline name field of an agent tab being renamed (double-click).
struct TabRename {
    pane: PaneId,
    input: Entity<InputState>,
    _subscription: Subscription,
}

struct DragChip(SharedString);

impl Render for DragChip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1p5()
            .px_2()
            .py_1()
            .rounded_lg()
            .bg(ui::raised())
            .border_1()
            .border_color(ui::petal().opacity(0.5))
            .shadow_lg()
            .text_xs()
            .text_color(ui::fg())
            .child(
                Icon::new(IconName::GripHorizontal)
                    .xsmall()
                    .text_color(ui::petal()),
            )
            .child(self.0.clone())
    }
}

struct Invisible;

impl Render for Invisible {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub struct Workspace {
    spaces: Vec<Space>,
    current: usize,
    panes: HashMap<PaneId, Pane>,
    next_id: usize,
    overlay: Option<(Overlay, Subscription)>,
    /// Agent panes last told their web views may show (see
    /// `sync_web_views`).
    web_shown: std::collections::HashSet<PaneId>,
    sidebar: Option<AnyView>,
    sidebar_visible: bool,
    tabs_sidebar_motion: sidebar_motion::SidebarMotion,
    files_sidebar_motion: sidebar_motion::SidebarMotion,
    tab_layout: TabLayout,
    /// The vertical tabs sidebar shows as the narrow icon rail.
    sidebar_compact: bool,
    /// Width of the expanded vertical tabs sidebar (px at 100 % interface size).
    tabs_sidebar_width: f32,
    /// Width of the right Files sidebar (px at 100 % interface size).
    files_sidebar_width: f32,
    /// The workspace whose badge picker is open, and where it opened.
    badge_picker: Option<(usize, Point<Pixels>)>,
    /// A right-click context menu owned by the workspace so its open state
    /// can suppress tooltips and hide native web views under it.
    open_menu: Option<OpenMenu>,
    /// Inline rename of a workspace, with the place it started (chip, header
    /// or sidebar row) so the text field draws there.
    workspace_rename: Option<WorkspaceRename>,
    tab_selection: Option<TabSelection>,
    tab_search: Entity<InputState>,
    save_task: Option<Task<()>>,
    /// Re-reads every pane's title and busy state once a second (non-agent
    /// views can't be observed through `AnyView`), redrawing only on change.
    _title_poll: Task<()>,
    _lifecycle: Vec<Subscription>,
    /// The bell's notification center, shared by every tab.
    notices: Entity<Notices>,
    renaming: Option<TabRename>,
    /// The max-effort surge playing over the window (its run number and when
    /// it started), and the timer that clears it.
    surge: Option<(u64, std::time::Instant, crate::shared::surge::Level)>,
    surge_runs: u64,
    _surge_end: Option<Task<()>>,
    /// Web and device stages, in creation order: shown ones float and tile,
    /// the rest wait in the title bar's instance tray.
    stages: Vec<Stage>,
    next_stage: u64,
    /// Pages agents opened in the browser outside the app, so one a tool
    /// keeps printing opens there once.
    opened_outside: std::collections::HashSet<String>,
    /// The design canvas's opens in the browser outside the app: every new
    /// open opens a tab, one open only once.
    canvas_outside: gentle_core::web_browsers::OutsideCanvas,
    /// The docked browser column's divider is being dragged: where the
    /// pointer went down (x), the column's width then, and the room it
    /// shares with the conversation.
    browser_dock_drag: Option<(f32, f32, f32)>,
    /// The conversation's width as last laid out, and the docked browser
    /// column's as last drawn: together, the room the column may take from.
    browser_dock_room: std::rc::Rc<std::cell::Cell<(f32, f32)>>,
    /// The welcome screen, kept while its "Sign in" shows Providers so closing
    /// Providers returns to it.
    resume_welcome: Option<Entity<WelcomeView>>,
    /// While a pane is dragged: the pane under the pointer and where it would
    /// land there.
    drop_hint: Option<(PaneId, DropZone)>,
    /// Panes and tabs closed, most recent last, to reopen.
    closed: Vec<closed::Closed>,
    /// gentle-stream, behind the title bar's ♪ button.
    music: Entity<Music>,
    /// Simulators and emulators, docked at the right or floating.
    devices: Entity<DevicePanel>,
}

impl EventEmitter<WorkspaceEvent> for Workspace {}

impl Workspace {
    /// Restores the saved workspaces. `folder` (the command-line argument)
    /// selects the workspace rooted there, creating it when new; `prompt`
    /// opens in a new agent tab.
    pub fn new(
        folder: Option<PathBuf>,
        prompt: Option<String>,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let saved = workspaces::load();
        let mut restored = Vec::new();
        let spaces: Vec<Space> = saved
            .workspaces
            .into_iter()
            .map(|w| {
                restored.push((w.tabs, w.active_tab));
                Space {
                    name: w.name,
                    root: w.root,
                    last_active: w.last_active,
                    tabs: Vec::new(),
                    active: 0,
                    badge: w.badge,
                    sidebar_expanded: w.sidebar_expanded,
                }
            })
            .collect();
        let tab_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search tabs"));
        let music = cx.new(|cx| Music::new(window, cx));
        let devices = cx.new(DevicePanel::new);
        let mut lifecycle = vec![
            cx.on_app_quit(|this, cx| {
                this.music.read(cx).shutdown();
                this.save_now(cx);
                // The app exits before any tab is released: ask every backend
                // to stop now. A moment's wait lets the launchers pass SIGTERM
                // on to Pi before the exit closes its stdin (GPUI caps it).
                let mut signalled = false;
                for session in this.sessions() {
                    signalled |= session.read(cx).shutdown_backend();
                }
                let settle =
                    signalled.then(|| cx.background_executor().timer(Duration::from_millis(100)));
                async move {
                    if let Some(settle) = settle {
                        settle.await;
                    }
                }
            }),
            cx.on_release(|this, cx| this.save_now(cx)),
            // The title-bar update pill follows this app's own update state.
            cx.observe_global::<crate::features::app_update::AppUpdate>(|_, cx| cx.notify()),
            // The water reminder paints over the window while it shows.
            cx.observe_global::<crate::features::hydration::Hydration>(|_, cx| cx.notify()),
            // Keep a multi-stage grid filling the window as it resizes. A
            // lone card keeps whatever size the user gave it.
            cx.observe_window_bounds(window, |this, window, cx| {
                if this.floating_stages(cx) >= 2 {
                    this.tile_stages(window, cx);
                }
            }),
            // The ♪ button's look follows the player; the player's own panel
            // redraws itself (its spectrum) without redrawing the window.
            cx.subscribe(&music, |_, _, _: &MusicChanged, cx| cx.notify()),
            cx.subscribe_in(
                &devices,
                window,
                |this, _, event: &DevicePanelEvent, window, cx| match event {
                    DevicePanelEvent::Layout => cx.notify(),
                    DevicePanelEvent::OpenSettings => this.open_settings(window, cx),
                },
            ),
        ];
        lifecycle.push(
            cx.subscribe_in(&tab_search, window, |_, _, _: &InputEvent, _, cx| {
                cx.notify()
            }),
        );
        let mut this = Self {
            current: saved.current.min(spaces.len().saturating_sub(1)),
            spaces,
            panes: HashMap::new(),
            next_id: 0,
            overlay: None,
            web_shown: Default::default(),
            sidebar: None,
            sidebar_visible: saved.sidebar,
            tabs_sidebar_motion: Default::default(),
            files_sidebar_motion: Default::default(),
            tab_layout: saved.tab_layout,
            sidebar_compact: saved.sidebar_compact,
            tabs_sidebar_width: gentle_core::workspaces::saved_sidebar_width(
                saved.tabs_sidebar_width,
            ),
            files_sidebar_width: gentle_core::workspaces::saved_sidebar_width(
                saved.files_sidebar_width,
            ),
            badge_picker: None,
            open_menu: None,
            workspace_rename: None,
            tab_selection: None,
            tab_search,
            save_task: None,
            notices: cx.new(|_| Notices::default()),
            renaming: None,
            surge: None,
            surge_runs: 0,
            stages: Vec::new(),
            next_stage: 1,
            opened_outside: Default::default(),
            canvas_outside: Default::default(),
            browser_dock_drag: None,
            browser_dock_room: Default::default(),
            resume_welcome: None,
            drop_hint: None,
            closed: Vec::new(),
            music,
            devices,
            _surge_end: None,
            _title_poll: cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    crate::shared::diagnostics::tick();
                    let Ok(()) = this.update(cx, |this, cx| this.refresh_titles(cx)) else {
                        break;
                    };
                }
            }),
            _lifecycle: lifecycle,
        };
        // Restore every workspace's tab list as cheap unbuilt placeholders now,
        // so the sidebar, counts, search and menus see one model everywhere.
        for (index, (tabs, active)) in restored.into_iter().enumerate() {
            let tabs: Vec<Tab> = tabs
                .into_iter()
                .map(|saved| Tab {
                    id: this.alloc_id(),
                    pinned: saved.pinned,
                    state: TabState::Unbuilt(saved),
                })
                .collect();
            let space = &mut this.spaces[index];
            space.active = active.min(tabs.len().saturating_sub(1));
            space.tabs = tabs;
        }
        let target = match folder {
            Some(folder) => match this.spaces.iter().position(|s| s.root == folder) {
                Some(index) => index,
                None => this.push_space(None, folder),
            },
            None if this.spaces.is_empty() => this.push_space(None, crate::cli::default_cwd()),
            None => this.current,
        };
        this.current = target;
        if let Some(prompt) = prompt {
            let root = this.space().root.clone();
            this.open_tab(PaneKind::Agent, root, Some(prompt), None, window, cx);
        } else {
            let active = this.space().active;
            this.activate(active, window, cx);
        }
        if let Some(url) = url {
            this.open_agent_page(url, None, false, window, cx);
        }
        this.space_mut().last_active = workspaces::now();
        this.schedule_save(cx);
        // First run: the welcome screen, from the very first frame (it looks
        // at the machine itself while its banner writes in), so the chat
        // never flashes underneath. Later, something required missing: the
        // onboarding screen, once the checks (blocking; PATH probes) return.
        if crate::features::setup::first_run() {
            this.open_welcome(None, window, cx);
        } else {
            cx.spawn_in(window, async move |this, cx| {
                let probe = cx.background_executor().spawn(async { Probe::run() }).await;
                if !probe.needs_onboarding() {
                    return;
                }
                let _ = this.update_in(cx, |this, window, cx| {
                    if this.overlay.is_none() {
                        this.open_onboarding(Some(probe), window, cx);
                    }
                });
            })
            .detach();
        }
        this
    }

    // ── Public hooks ────────────────────────────────────────────────────────

    /// A later launch's request (see `crate::platform::instance`): show the
    /// workspace rooted at `folder` (creating it), open `prompt` in a new
    /// agent tab, a page, or what `gentle-device` asked for.
    pub fn open_request(
        &mut self,
        request: crate::platform::instance::OpenRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let crate::platform::instance::OpenRequest {
            folder,
            prompt,
            url,
            device,
        } = request;
        if let Some(folder) = folder {
            self.show_folder(folder, window, cx);
        }
        if let Some(prompt) = prompt {
            let root = self.space().root.clone();
            self.open_tab(PaneKind::Agent, root, Some(prompt), None, window, cx);
        }
        if let Some(url) = url {
            self.open_agent_page(url, None, false, window, cx);
        }
        if let Some(args) = device {
            self.device_request(&args, cx);
        }
        cx.notify();
    }

    /// `gentle-device`'s request: start the hub, open a device, close one.
    fn device_request(&mut self, args: &[String], cx: &mut Context<Self>) {
        use crate::features::devices::cli::{Request, parse};
        let Ok(request) = parse(args) else { return };
        self.devices.update(cx, |panel, cx| match request {
            Request::Ensure => {
                crate::features::devices::hub(cx).update(cx, |hub, cx| hub.ensure(cx))
            }
            Request::Open(query) => panel.open_query(query, cx),
            Request::Close { query, shutdown } => panel.close_query(&query, shutdown, cx),
        });
    }

    /// Put `view` in the right Files sidebar (Cmd/Ctrl+B toggles it).
    pub fn set_sidebar(&mut self, view: AnyView, cx: &mut Context<Self>) {
        self.sidebar = Some(view);
        cx.notify();
    }

    /// The shown workspace's root folder.
    pub fn root(&self) -> &Path {
        &self.space().root
    }

    /// Show `path` in the tab's preview pane, or a new split beside the largest pane.
    pub fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab().is_none() {
            self.open_tab(PaneKind::File, path, None, None, window, cx);
            return;
        }
        if let Some(pane) = self.new_pane(PaneKind::File, path, None, None, window, cx) {
            self.place_preview(pane, window, cx);
        }
    }

    /// [`Self::open_file`], then put its cursor at `line` (1-based).
    pub fn open_file_at(
        &mut self,
        path: PathBuf,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file(path.clone(), window, cx);
        let Some(line) = line else { return };
        let view = self
            .panes
            .iter()
            .filter(|(_, pane)| pane.path.as_ref() == Some(&path))
            .max_by_key(|(id, _)| **id)
            .and_then(|(_, pane)| {
                pane.item
                    .view
                    .clone()
                    .downcast::<crate::features::files::FileView>()
                    .ok()
            });
        if let Some(view) = view {
            view.update(cx, |view, cx| view.go_to_line(line, window, cx));
        }
    }

    /// Show `path`'s changes against HEAD, placed like [`Self::open_file`].
    pub fn open_diff(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(factory), Some(_)) = (PaneFactories::get(cx).diff, self.focused_pane()) else {
            self.open_file(path, window, cx);
            return;
        };
        let item = factory(path.clone(), window, cx);
        let id = self.file_pane(item, path, cx);
        self.place_preview(id, window, cx);
    }

    /// The Git pane of the repository `cwd` is in, placed like a preview: it
    /// takes the place of the tab's preview pane, if any.
    pub fn open_history(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.new_factory_pane(PaneKind::History, cwd, window, cx) {
            self.place_preview(id, window, cx);
        }
    }

    /// Open `path` in a new split right of the focused pane (its own tab when
    /// there is no room), or like [`Self::open_file`] in an empty workspace.
    pub fn open_file_beside(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.focused_pane().is_none() {
            self.open_file(path, window, cx);
            return;
        }
        self.split(
            SplitAxis::Horizontal,
            Some((PaneKind::File, path)),
            window,
            cx,
        );
    }

    /// Open `path` as a tab of its own.
    pub fn open_file_in_tab(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_tab(PaneKind::File, path, None, None, window, cx);
    }

    /// Open an agent tab working in `folder` (a worktree from the sidebar).
    pub fn open_agent_tab(&mut self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_tab(PaneKind::Agent, folder, None, None, window, cx);
    }

    /// Type `text` at the active agent's prompt cursor, spaced from what is
    /// before it, and focus the prompt. `false` when no agent is open.
    pub fn insert_into_composer(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.ensure_agent_built(window, cx);
        let Some(session) = self.active_session() else {
            return false;
        };
        let pane = self
            .panes
            .iter()
            .find(|(_, pane)| pane.session.as_ref() == Some(&session))
            .map(|(id, _)| *id);
        if let Some(pane) = pane {
            self.reveal_pane(pane, window, cx);
        }
        let composer = session.read(cx).composer();
        composer.update(cx, |state, cx| {
            let value = state.value();
            let before = value
                .get(..state.cursor())
                .and_then(|s| s.chars().next_back());
            let lead = if before.is_some_and(|c| !c.is_whitespace()) {
                " "
            } else {
                ""
            };
            state.insert(format!("{lead}{text} "), window, cx);
            state.focus(window, cx);
        });
        true
    }
}
