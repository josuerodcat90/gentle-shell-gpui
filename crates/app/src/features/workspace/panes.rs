//! Pane creation, preview placement, focus, splits, closing and swapping.

use super::*;

impl Workspace {
    /// Puts a freshly built file, diff or history pane in the current tab: it
    /// replaces the tab's existing pane of the same kind, else splits the
    /// largest pane along its long side, else (no room for two panes) gets
    /// its own tab.
    pub(super) fn place_preview(
        &mut self,
        new: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab().filter(|t| t.is_built()) else {
            self.push_pane_tab(new, window, cx);
            return;
        };
        let kind = self.panes.get(&new).map(|p| p.item.kind);
        let tree = tab.built_tree().unwrap();
        let preview = tree.leaves().into_iter().find(|id| {
            *id != new
                && self
                    .panes
                    .get(id)
                    .is_some_and(|p| Some(p.item.kind) == kind)
        });
        if let Some(old) = preview
            && let Some(pane) = self.panes.remove(&new)
        {
            self.panes.insert(old, pane);
            self.focus_pane(old, window, cx);
            self.schedule_save(cx);
            return;
        }
        let largest = self
            .pane_sizes(window)
            .into_iter()
            .max_by(|a, b| (a.1 * a.2).total_cmp(&(b.1 * b.2)));
        let Some((target, w, h)) = largest else {
            self.push_pane_tab(new, window, cx);
            return;
        };
        let axis = if w >= h * 1.2 {
            SplitAxis::Horizontal
        } else {
            SplitAxis::Vertical
        };
        if !fits_split(axis, w, h) {
            self.push_pane_tab(new, window, cx);
            return;
        }
        if let Some(tree) = self.tab_mut().and_then(|t| t.built_tree_mut()) {
            tree.split(&target, new, axis);
        }
        self.focus_pane(new, window, cx);
        self.schedule_save(cx);
    }

    /// Opens an already built pane as a new tab.
    pub(super) fn push_pane_tab(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.alloc_id();
        let space = self.space_mut();
        space.tabs.push(Tab {
            id,
            pinned: false,
            state: TabState::Built {
                tree: PaneTree::Leaf(pane),
                focused: pane,
            },
        });
        let index = space.tabs.len() - 1;
        self.activate(index, window, cx);
    }

    /// Approximate on-screen size of each pane in the current tab.
    pub(super) fn pane_sizes(&self, window: &Window) -> Vec<(PaneId, f32, f32)> {
        let viewport = window.viewport_size();
        let rem = |n: f32| f32::from(rems(n / 16.).to_pixels(window.rem_size()));
        // Vertical tabs and Files are separate columns. Keep pane hit-testing
        // and split-size calculations aligned with the actual rendered width.
        let window_px =
            f32::from(window.viewport_size().width) / (f32::from(window.rem_size()) / 16.);
        let now = std::time::Instant::now();
        let occupied = |side| {
            let (width, fraction) = self.sidebar_geometry(side, window_px, now);
            rem((width + 6.) * fraction)
        };
        let w =
            (f32::from(viewport.width) - rem(12.) - occupied(Side::Tabs) - occupied(Side::Files))
                .max(0.);
        let h = f32::from(viewport.height) - rem(super::render::TITLE_BAR_HEIGHT + 6.);
        let mut out = Vec::new();
        if let Some(tree) = self.tab().and_then(|t| t.built_tree()) {
            leaf_sizes(tree, w, h, &mut out);
        }
        out
    }

    // ── Workspaces ──────────────────────────────────────────────────────────

    pub(super) fn pane_cwd(&self, pane: &Pane, cx: &App) -> PathBuf {
        match &pane.session {
            Some(session) => session.read(cx).cwd().clone(),
            None => pane.cwd.clone(),
        }
    }

    /// An agent pane; its backend starts now with `start_now` (the pane is in
    /// the tab being shown), else the first time its tab is shown.
    pub(super) fn new_agent_pane(
        &mut self,
        cwd: PathBuf,
        prompt: Option<String>,
        resume: Option<PathBuf>,
        start_now: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PaneId {
        let session = cx.new(|cx| SessionView::new(cwd.clone(), prompt, window, cx, start_now));
        if let Some(path) = resume.clone() {
            session.update(cx, |s, cx| s.resume_when_ready(path, cx));
        }
        let id = self.alloc_id();
        let focus = session.read(cx).composer().read(cx).focus_handle(cx);
        let title = {
            let session = session.clone();
            Rc::new(move |cx: &App| SharedString::from(session.read(cx).title()))
        };
        let subscriptions = vec![cx.subscribe_in(
            &session,
            window,
            move |this, session, event: &SessionEvent, window, cx| {
                this.on_session_event(id, session, event, window, cx)
            },
        )];
        let mut pane = Pane {
            item: PaneItem {
                kind: PaneKind::Agent,
                view: session.clone().into(),
                focus,
                title,
                unsaved: None,
            },
            session: Some(session),
            cwd,
            path: None,
            session_file: resume,
            title: SharedString::default(),
            running: false,
            needs_you: false,
            _subscriptions: subscriptions,
        };
        pane.sync_chrome(cx);
        self.panes.insert(id, pane);
        id
    }

    /// What an agent asks of the window: pane `id` shows it (its own pane, or
    /// the canvas it floats in).
    pub(super) fn on_session_event(
        &mut self,
        id: PaneId,
        session: &Entity<SessionView>,
        event: &SessionEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            // Tab titles, running dots and the saved conversation follow
            // each session; its other changes don't touch the chrome.
            SessionEvent::ChromeChanged => {
                let file = session.read(cx).session_file();
                let Some(pane) = self.panes.get_mut(&id) else {
                    return;
                };
                let changed = pane.sync_chrome(cx);
                if pane.session_file != file {
                    pane.session_file = file;
                    self.schedule_save(cx);
                }
                // An agent that just started may have the docs skill.
                if let Some((Overlay::Docs(view), _)) = &self.overlay {
                    let (view, dir) = (view.clone(), self.docs_skill_dir(cx));
                    view.update(cx, |view, cx| view.set_skill_dir(dir, cx));
                }
                if changed {
                    // An open switcher lists every workspace's activity.
                    if let Some((Overlay::Palette(view), _)) = &self.overlay {
                        let (view, rows) = (view.clone(), self.workspace_rows());
                        view.update(cx, |view, cx| view.set_workspaces(rows, cx));
                    }
                    cx.notify();
                }
            }
            SessionEvent::OpenProviders => self.open_providers(window, cx),
            SessionEvent::OpenScreen(screen) => {
                self.open_screen(*screen, Some(session.clone()), window, cx)
            }
            SessionEvent::OpenMemory(target) => {
                let cwd = session.read(cx).cwd().clone();
                self.open_memory(cwd, target.clone(), window, cx)
            }
            SessionEvent::Effort(level) => {
                if let Some(level) = crate::shared::surge::Level::of(level) {
                    self.play_surge(level, cx)
                }
            }
            SessionEvent::DesignCanvas(canvas) => {
                self.show_design_canvas(canvas.clone(), id, window, cx)
            }
            SessionEvent::Device(intent) => {
                // A device an agent starts itself still shows in the app.
                if crate::features::devices::hub(cx).read(cx).prefs().enabled {
                    let intent = intent.clone();
                    self.devices
                        .update(cx, |panel, cx| panel.follow(intent, cx));
                }
            }
            SessionEvent::DeviceCommand(command) => {
                let command = command.clone();
                self.devices
                    .update(cx, |panel, cx| panel.activity(command, cx));
            }
            SessionEvent::OpenWeb(url) => self.open_web(url.clone(), Some(id), true, window, cx),
            SessionEvent::OpenFile { path, line } => {
                self.open_file_at(path.clone(), *line, window, cx)
            }
            SessionEvent::AgentWeb(url) => {
                self.open_agent_page(url.clone(), Some(id), true, window, cx)
            }
            SessionEvent::Notice { level, message } => {
                let source = self.pane_title(id);
                let (level, message) = (*level, message.clone());
                self.notices.update(cx, |store, cx| {
                    store.push(level, message, source, Some(id), std::time::Instant::now());
                    cx.notify();
                });
                cx.notify();
            }
        }
    }

    /// A terminal or history (`target` = folder), file (`target` = file) or
    /// canvas (`target` = its file, or a folder for a new one) pane from the
    /// registered factory; a notice when there is none.
    pub(super) fn new_factory_pane(
        &mut self,
        kind: PaneKind,
        target: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        let root = self.space().root.clone();
        self.new_factory_pane_at(kind, target, &root, window, cx)
    }

    fn new_factory_pane_at(
        &mut self,
        kind: PaneKind,
        target: PathBuf,
        root: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        let factories = PaneFactories::get(cx);
        let factory = match kind {
            PaneKind::Terminal => factories.terminal,
            PaneKind::File => factories.file,
            PaneKind::History => factories.history,
            PaneKind::Canvas => factories.canvas,
            PaneKind::Tower => factories.tower,
            PaneKind::Agent => None,
        };
        let Some(factory) = factory else {
            window.push_notification(
                Notification::info(format!(
                    "{} panes are not available in this build.",
                    match kind {
                        PaneKind::File => "File",
                        PaneKind::History => "Git",
                        PaneKind::Canvas => "Canvas",
                        PaneKind::Tower => "Tower",
                        _ => "Terminal",
                    }
                )),
                cx,
            );
            return None;
        };
        // A canvas started from a folder gets a new file; the folder stays
        // its cwd, so splitting from it opens the next one there too.
        let (canvas_cwd, target) = match kind {
            PaneKind::Canvas if target.extension().is_none_or(|e| e != "json") => {
                (Some(target), crate::features::canvas::new_canvas_path())
            }
            _ => (None, target),
        };
        let item = factory(target.clone(), window, cx);
        let id = self.alloc_id();
        let (cwd, path) = match kind {
            PaneKind::File => (
                target
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| root.to_path_buf()),
                Some(target),
            ),
            PaneKind::Canvas => (
                canvas_cwd.unwrap_or_else(|| root.to_path_buf()),
                Some(target),
            ),
            _ => (target, None),
        };
        // A terminal whose shell exits (`exit`, Ctrl-D) closes its pane; a
        // double Ctrl+C at its idle prompt closes it as ⌘W does.
        let subscriptions = item
            .view
            .clone()
            .downcast::<crate::features::terminal::TerminalView>()
            .ok()
            .map(|terminal| {
                cx.subscribe_in(
                    &terminal,
                    window,
                    move |this, _, event: &crate::features::terminal::TerminalEvent, window, cx| {
                        use crate::features::terminal::TerminalEvent;
                        match event {
                            TerminalEvent::Exited => this.close_pane_now(id, window, cx),
                            TerminalEvent::CloseRequested => this.close_pane(id, window, cx),
                            TerminalEvent::TitleChanged => {}
                        }
                    },
                )
            })
            .into_iter()
            .collect();
        let mut pane = Pane {
            item,
            session: None,
            cwd,
            path,
            session_file: None,
            title: SharedString::default(),
            running: false,
            needs_you: false,
            _subscriptions: subscriptions,
        };
        pane.sync_chrome(cx);
        if kind == PaneKind::Canvas
            && let Ok(canvas) = pane.item.view.clone().downcast::<CanvasView>()
        {
            canvas.update(cx, |canvas, _| canvas.set_cwd(pane.cwd.clone()));
            pane._subscriptions.push(cx.subscribe_in(
                &canvas,
                window,
                move |this, _, event: &CanvasEvent, window, cx| match event {
                    // Its agent's asks go where an agent pane's do, shown
                    // as this canvas pane.
                    CanvasEvent::AgentStarted(session) => {
                        let subscription = cx.subscribe_in(
                            session,
                            window,
                            move |this, session, event: &SessionEvent, window, cx| {
                                this.on_session_event(id, session, event, window, cx)
                            },
                        );
                        if let Some(pane) = this.panes.get_mut(&id) {
                            pane._subscriptions.push(subscription);
                        }
                    }
                },
            ));
        }
        self.panes.insert(id, pane);
        Some(id)
    }

    /// The canvas pane `id` shows, if it is one.
    pub(super) fn canvas_view(&self, id: PaneId) -> Option<Entity<CanvasView>> {
        self.panes.get(&id)?.item.view.clone().downcast().ok()
    }

    /// A pane for an already built file view of `path`.
    pub(super) fn file_pane(&mut self, item: PaneItem, path: PathBuf, cx: &App) -> PaneId {
        let id = self.alloc_id();
        let cwd = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.space().root.clone());
        let mut pane = Pane {
            item,
            session: None,
            cwd,
            path: Some(path),
            session_file: None,
            title: SharedString::default(),
            running: false,
            needs_you: false,
            _subscriptions: Vec::new(),
        };
        pane.sync_chrome(cx);
        self.panes.insert(id, pane);
        id
    }

    pub(super) fn new_pane(
        &mut self,
        kind: PaneKind,
        target: PathBuf,
        prompt: Option<String>,
        resume: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        match kind {
            // Register the pane before starting its backend. Activation
            // starts shown agents after the new tab has been inserted.
            PaneKind::Agent => Some(self.new_agent_pane(target, prompt, resume, false, window, cx)),
            PaneKind::Terminal
            | PaneKind::File
            | PaneKind::History
            | PaneKind::Canvas
            | PaneKind::Tower => self.new_factory_pane(kind, target, window, cx),
        }
    }

    pub(super) fn restore_pane_in_space(
        &mut self,
        space: usize,
        saved: &SavedPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        let root = self.spaces[space].root.clone();
        let cwd = if saved.cwd.is_dir() {
            saved.cwd.clone()
        } else if root.is_dir() {
            root.clone()
        } else {
            crate::cli::default_cwd()
        };
        match saved.kind {
            PaneKind::Agent => {
                let resume = saved.session.clone().filter(|p| p.exists());
                // Started once its tab is shown (see `start_shown`).
                Some(self.new_agent_pane(cwd, None, resume, false, window, cx))
            }
            PaneKind::Terminal => {
                PaneFactories::get(cx).terminal.as_ref()?;
                self.new_factory_pane_at(PaneKind::Terminal, cwd, &root, window, cx)
            }
            PaneKind::File => {
                let path = saved.path.clone().filter(|p| p.is_file())?;
                PaneFactories::get(cx).file.as_ref()?;
                self.new_factory_pane_at(PaneKind::File, path, &root, window, cx)
            }
            PaneKind::Tower => {
                PaneFactories::get(cx).tower.as_ref()?;
                self.new_factory_pane_at(PaneKind::Tower, cwd, &root, window, cx)
            }
            PaneKind::History => {
                PaneFactories::get(cx).history.as_ref()?;
                self.new_factory_pane_at(PaneKind::History, cwd, &root, window, cx)
            }
            PaneKind::Canvas => {
                let path = saved.path.clone()?;
                PaneFactories::get(cx).canvas.as_ref()?;
                let id = self.new_factory_pane_at(PaneKind::Canvas, path, &root, window, cx)?;
                // Its agent works in the folder it had and picks up its
                // conversation the next time it opens.
                let resume = saved.session.clone().filter(|p| p.exists());
                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.cwd = cwd.clone();
                    pane.session_file = resume.clone();
                }
                if let Some(canvas) = self.canvas_view(id) {
                    canvas.update(cx, |canvas, _| {
                        canvas.set_cwd(cwd);
                        if let Some(resume) = resume {
                            canvas.resume_agent(resume);
                        }
                    });
                }
                Some(id)
            }
        }
    }

    pub(super) fn open_tab(
        &mut self,
        kind: PaneKind,
        target: PathBuf,
        prompt: Option<String>,
        resume: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.new_pane(kind, target, prompt, resume, window, cx) else {
            return;
        };
        self.push_pane_tab(pane, window, cx);
    }

    /// Build tab `tab_index` of workspace `space_index` if it is still an
    /// unbuilt placeholder.
    pub(super) fn ensure_tab_built(
        &mut self,
        space_index: usize,
        tab_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let saved = self
            .spaces
            .get(space_index)
            .and_then(|space| space.tabs.get(tab_index))
            .and_then(|tab| tab.state.unbuilt())
            .cloned();
        let Some(saved) = saved else {
            return;
        };
        let leaves = saved.layout.leaves();
        let mut built = Vec::new();
        let tree = saved.layout.filter_map(&mut |pane: &SavedPane| {
            let id = self.restore_pane_in_space(space_index, pane, window, cx);
            built.push(id);
            id
        });
        let Some(tree) = tree else {
            // Nothing could be restored; keep the workspace empty if this
            // was its last tab, rather than silently launching another agent.
            self.spaces[space_index].tabs.remove(tab_index);
            tab_panel::after_removal(&mut self.spaces[space_index], tab_index);
            self.prune_tab_selection(cx);
            return;
        };
        let focused = built
            .get(saved.focused.min(leaves.len().saturating_sub(1)))
            .copied()
            .flatten()
            .or_else(|| tree.leaves().first().copied())
            .unwrap_or_default();
        if let Some(tab) = self.spaces[space_index].tabs.get_mut(tab_index) {
            tab.state.set_built(tree, focused);
        }
    }

    pub(super) fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let space_index = self.current;
        let space = self.space_mut();
        space.active = index.min(space.tabs.len().saturating_sub(1));
        // A tab that restores nothing (its file is gone) is dropped and the
        // next one takes its place, so the shown tab is always built.
        loop {
            let active = self.spaces[space_index].active;
            if !self.spaces[space_index]
                .tabs
                .get(active)
                .is_some_and(|t| !t.is_built())
            {
                break;
            }
            self.ensure_tab_built(space_index, active, window, cx);
        }
        self.focus_active(window, cx);
        self.start_shown(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Start the backends of the shown tab's agents: restored tabs start the
    /// first time they are shown rather than all at launch.
    pub(super) fn start_shown(&self, window: &mut Window, cx: &mut App) {
        let Some(tab) = self.tab() else { return };
        for id in tab.leaves() {
            if let Some(session) = self.panes.get(&id).and_then(|p| p.session.as_ref()) {
                session.update(cx, |s, cx| s.ensure_started(window, cx));
            }
        }
    }

    /// Focus the active tab's focused pane.
    pub(super) fn focus_active(&self, window: &mut Window, cx: &mut App) {
        if let Some(pane) = self.focused_pane().and_then(|id| self.panes.get(&id)) {
            pane.item.focus.focus(window, cx);
        }
    }

    pub(super) fn focus_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_mut()
            && tab.state.contains(&pane)
        {
            tab.state.set_focused(pane);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Show the pane wherever it lives: its workspace, its tab, then focus.
    pub(super) fn reveal_pane(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found = self.spaces.iter().enumerate().find_map(|(s, space)| {
            space
                .tabs
                .iter()
                .position(|tab| tab.state.contains(&pane))
                .map(|t| (s, t))
        });
        let Some((space, tab)) = found else {
            return;
        };
        if space != self.current {
            // Switch directly to the tab holding the pane, not the old selection.
            self.spaces[space].active = tab;
            self.switch_workspace(space, window, cx);
        } else {
            self.activate(tab, window, cx);
        }
        self.focus_pane(pane, window, cx);
    }

    /// Split the focused pane: with `what`, that kind and target; otherwise
    /// the focused pane's kind in its folder (or on its file).
    pub(super) fn split(
        &mut self,
        axis: SplitAxis,
        what: Option<(PaneKind, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(focused) = self.focused_pane() else {
            let root = self.space().root.clone();
            self.open_tab(PaneKind::Agent, root, None, None, window, cx);
            return;
        };
        let (kind, target) = match what {
            Some(what) => what,
            None => {
                let pane = &self.panes[&focused];
                let kind = pane.item.kind;
                let target = match (kind, &pane.path) {
                    (PaneKind::File, Some(path)) => path.clone(),
                    _ => self.pane_cwd(pane, cx),
                };
                (kind, target)
            }
        };
        let Some(pane) = self.new_pane(kind, target, None, None, window, cx) else {
            return;
        };
        let room = self
            .pane_sizes(window)
            .into_iter()
            .find(|(id, ..)| *id == focused)
            .is_none_or(|(_, w, h)| fits_split(axis, w, h));
        if !room {
            self.push_pane_tab(pane, window, cx);
            return;
        }
        if let Some(tree) = self.tab_mut().and_then(|t| t.built_tree_mut()) {
            tree.split(&focused, pane, axis);
        }
        self.focus_pane(pane, window, cx);
        self.schedule_save(cx);
    }

    /// A new pane tiled like Hyprland's dwindle: the focused pane splits
    /// along its longer side (the other one if only that fits), so panes
    /// spiral in instead of lining up in one row. No room: a new tab.
    pub(super) fn tile(
        &mut self,
        what: Option<(PaneKind, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self.focused_pane();
        let axis = self
            .pane_sizes(window)
            .into_iter()
            .find(|(id, ..)| Some(*id) == focused)
            .and_then(|(_, w, h)| tile_axis(w, h))
            // No size yet, or no room either way: `split` opens a tab.
            .unwrap_or(SplitAxis::Horizontal);
        self.split(axis, what, window, cx);
    }

    pub(super) fn split_with_kind(
        &mut self,
        kind: PaneKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.active_cwd(cx);
        self.tile(Some((kind, cwd)), window, cx);
    }

    /// Drop `moving` on `target` at `zone`: beside it on that side, or swap
    /// the two in the middle. A side whose halves wouldn't fit swaps instead.
    pub(super) fn move_pane(
        &mut self,
        moving: PaneId,
        target: PaneId,
        zone: DropZone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placement = zone.placement().filter(|(axis, _)| {
            self.pane_sizes(window)
                .into_iter()
                .find(|(id, ..)| *id == target)
                .is_none_or(|(_, w, h)| fits_split(*axis, w, h))
        });
        let Some((axis, before)) = placement else {
            self.swap_panes(moving, target, cx);
            return;
        };
        if moving == target {
            return;
        }
        if let Some(tab) = self.tab_mut()
            && let Some(tree) = tab.built_tree_mut()
        {
            let new_tree = std::mem::replace(tree, PaneTree::Leaf(moving));
            *tree = new_tree.moved(&moving, &target, axis, before);
            tab.state.set_focused(moving);
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// Close `pane`, asking first when it holds unsaved edits or its agent
    /// is working or waiting on you.
    pub(super) fn close_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let unsaved = self.unsaved_titles([pane], cx);
        let busy = self.busy_titles([pane]);
        if unsaved.is_empty() && busy.is_empty() {
            self.close_pane_now(pane, window, cx);
        } else {
            self.confirm_close(unsaved, busy, window, cx, move |this, window, cx| {
                this.close_pane_now(pane, window, cx)
            });
        }
    }

    /// Titles of the file panes among `panes` with edits not yet saved.
    pub(super) fn unsaved_titles(
        &self,
        panes: impl IntoIterator<Item = PaneId>,
        cx: &App,
    ) -> Vec<SharedString> {
        panes
            .into_iter()
            .filter_map(|id| self.panes.get(&id))
            .filter(|p| p.item.unsaved.as_ref().is_some_and(|u| u(cx)))
            .map(|p| (p.item.title)(cx))
            .collect()
    }

    /// Titles of the agents among `panes` in the middle of something:
    /// running a turn or waiting on a dialog. Closing one stops it.
    pub(super) fn busy_titles(&self, panes: impl IntoIterator<Item = PaneId>) -> Vec<SharedString> {
        panes
            .into_iter()
            .filter_map(|id| self.panes.get(&id))
            .filter(|p| p.running || p.needs_you)
            .map(|p| p.title.clone())
            .collect()
    }

    /// "Close anyway?" for files with unsaved edits and agents at work;
    /// `then` runs only on confirm.
    pub(super) fn confirm_close(
        &mut self,
        unsaved: Vec<SharedString>,
        busy: Vec<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let names = |titles: &[SharedString]| {
            titles
                .iter()
                .map(|t| format!("\u{201c}{}\u{201d}", t.trim_end_matches(" •")))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let title = match (unsaved.len(), busy.len()) {
            (0, 1) => format!("Stop {} and close it?", names(&busy)),
            (0, n) => format!("Stop {n} agents and close them?"),
            (1, 0) => format!("Close {} without saving?", names(&unsaved)),
            (n, 0) => format!("Close {n} files without saving?"),
            _ => "Close unsaved files and working agents?".to_string(),
        };
        let mut lines = Vec::new();
        if !unsaved.is_empty() {
            lines.push(format!(
                "Unsaved edits in {} are lost. {} saves first.",
                names(&unsaved),
                ui::shortcut("S")
            ));
        }
        if !busy.is_empty() {
            lines.push(format!(
                "{} {} working or waiting for your answer; closing stops {}. The \
                 conversation is kept: {} reopens it.",
                names(&busy),
                if busy.len() == 1 { "is" } else { "are" },
                if busy.len() == 1 { "it" } else { "them" },
                closed::REOPEN_KEY,
            ));
        }
        let description = lines.join(" ");
        let ok = if busy.is_empty() {
            "Close without saving"
        } else {
            "Stop and close"
        };
        let this = cx.entity().downgrade();
        let then = Rc::new(then);
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, then) = (this.clone(), then.clone());
            alert
                .title(title.clone())
                .description(description.clone())
                .ok_text(ok)
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let then = then.clone();
                    let _ = this.update(cx, |this, cx| then(this, window, cx));
                    true
                })
        });
    }

    pub(super) fn close_pane_now(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self
            .space()
            .tabs
            .iter()
            .position(|t| t.state.contains(&pane))
        else {
            return;
        };
        if self.space().tabs[index].state.len() == 1 {
            self.close_tab_now(index, window, cx);
            return;
        }
        let space = self.current;
        self.remember_close(space, index, &[pane], cx);
        let active = index == self.space().active;
        let tab = &mut self.space_mut().tabs[index];
        let tree = tab.built_tree_mut().expect("pane belongs to a built tab");
        let next = tree.focus_after_close(&pane);
        if let Some(new_tree) = tree.clone().without(&pane) {
            *tree = new_tree;
        }
        if tab.state.focused() == Some(pane)
            && let Some(next) = next
        {
            tab.state.set_focused(next);
        }
        // Dropping a session drops its PiProcess, which shuts the backend down.
        self.panes.remove(&pane);
        if active {
            self.focus_active(window, cx);
        }
        self.announce_close(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Close tab `index`, asking first when any of its panes holds unsaved
    /// edits or an agent at work.
    pub(super) fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_in_space(self.current, index, window, cx);
    }

    /// Close tab `index` of workspace `space`. Unbuilt tabs close without
    /// a confirmation and can be reopened from the closed tab stack.
    pub(super) fn close_tab_in_space(
        &mut self,
        space: usize,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.spaces.get(space).and_then(|s| s.tabs.get(index)) else {
            return;
        };
        if !tab.is_built() {
            self.close_unbuilt_tab(space, index, window, cx);
            return;
        }
        let leaves = tab.leaves();
        let unsaved = self.unsaved_titles(leaves.clone(), cx);
        let busy = self.busy_titles(leaves);
        if unsaved.is_empty() && busy.is_empty() {
            self.close_tab_now_in_space(space, index, window, cx);
        } else {
            let id = tab.id;
            self.confirm_close(unsaved, busy, window, cx, move |this, window, cx| {
                // The tab may have moved while the dialog was open.
                let index = this
                    .spaces
                    .get(space)
                    .and_then(|s| s.tabs.iter().position(|t| t.id == id));
                if let Some(index) = index {
                    this.close_tab_now_in_space(space, index, window, cx);
                }
            });
        }
    }

    fn close_unbuilt_tab(
        &mut self,
        space: usize,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (saved, tab_id, pinned) = {
            let tab = self.spaces.get(space).and_then(|s| s.tabs.get(index));
            let saved = tab.and_then(|t| t.state.unbuilt()).cloned();
            let id = tab.map(|t| t.id).unwrap_or(0);
            let pinned = tab.map(|t| t.pinned).unwrap_or(false);
            (saved, id, pinned)
        };
        let Some(saved) = saved else { return };
        let title = saved
            .layout
            .leaves()
            .get(
                saved
                    .focused
                    .min(saved.layout.leaves().len().saturating_sub(1)),
            )
            .map(super::sidebars::saved_title)
            .unwrap_or_default();
        if let Some(closed) = closed::Closed::unbuilt(space, tab_id, index, pinned, saved, title) {
            if self.closed.len() == closed::KEEP {
                self.closed.remove(0);
            }
            self.closed.push(closed);
        }
        self.spaces[space].tabs.remove(index);
        self.finish_tab_close(space, index, window, cx);
    }

    pub(super) fn close_tab_now(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_tab_now_in_space(self.current, index, window, cx);
    }

    fn close_tab_now_in_space(
        &mut self,
        space: usize,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= self.spaces[space].tabs.len() {
            return;
        }
        let leaves = self.spaces[space].tabs[index].leaves();
        self.remember_close(space, index, &leaves, cx);
        let tab = self.spaces[space].tabs.remove(index);
        for pane in tab.leaves() {
            self.panes.remove(&pane);
        }
        self.finish_tab_close(space, index, window, cx);
    }

    fn finish_tab_close(
        &mut self,
        space: usize,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tab_panel::after_removal(&mut self.spaces[space], index);
        self.prune_tab_selection(cx);
        self.announce_close(window, cx);
        if space == self.current && !self.space().tabs.is_empty() {
            self.activate(self.space().active, window, cx);
        } else {
            self.schedule_save(cx);
            cx.notify();
        }
    }

    pub(super) fn swap_panes(&mut self, a: PaneId, b: PaneId, cx: &mut Context<Self>) {
        if a == b {
            return;
        }
        if let Some(tab) = self.tab_mut()
            && let Some(tree) = tab.built_tree_mut()
            && tree.contains(&a)
            && tree.contains(&b)
        {
            tree.swap(&a, &b);
            self.schedule_save(cx);
            cx.notify();
        }
    }
}
