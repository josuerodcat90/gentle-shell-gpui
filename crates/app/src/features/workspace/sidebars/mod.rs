//! The left sidebar for vertical tabs: every workspace a group that opens
//! in place with its tabs (`accordion`), built from the shared pieces in
//! `parts`. It acts through the same operations as the horizontal strip.

use super::badge::Mark;
use super::tab_panel::TabMenu;
use super::view::new_tab_menu;
use super::*;

mod accordion;
mod compact;
mod parts;

/// One tab as the sidebar draws it.
#[derive(Clone)]
pub(super) struct TabEntry {
    /// Its workspace's index.
    pub space: usize,
    /// Its index among its workspace's tabs.
    pub index: usize,
    /// The tab's id.
    pub id: usize,
    /// The focused pane of a built tab; `None` while it is unbuilt.
    pub pane: Option<PaneId>,
    pub title: SharedString,
    pub kind: PaneKind,
    /// Active, in the shown workspace: the tab on screen.
    pub shown: bool,
    pub pinned: bool,
    pub running: bool,
    pub needs_you: bool,
    /// Panes tiled in the tab.
    pub panes: usize,
}

impl TabEntry {
    /// The kind icon carries the tab's state, like the horizontal strip:
    /// amber when an agent needs you, the accent while one works.
    pub fn tone(&self) -> Hsla {
        if self.needs_you {
            ui::warning()
        } else if self.running {
            ui::accent()
        } else if self.shown {
            ui::fg()
        } else {
            ui::muted()
        }
    }

    /// A dot for a tab that works or waits on you.
    pub fn status_dot(&self) -> Option<Div> {
        (self.running || self.needs_you).then(|| {
            let dot = div()
                .size(rems(6. / 16.))
                .flex_none()
                .rounded_full()
                .bg(self.tone());
            if self.running && !self.needs_you {
                ui::pulse(dot)
            } else {
                dot
            }
        })
    }

    pub fn tooltip(&self) -> SharedString {
        if self.panes > 1 {
            format!("{} · {} panes", self.title, self.panes).into()
        } else {
            self.title.clone()
        }
    }
}

/// One workspace as the sidebar draws it.
pub(super) struct SpaceEntry {
    pub index: usize,
    pub name: SharedString,
    pub mark: Mark,
    pub current: bool,
    pub activity: Activity,
    pub tabs: usize,
}

impl SpaceEntry {
    /// The workspace switcher's dot: amber when an agent needs you, the
    /// accent while one works.
    pub fn dot(&self) -> Option<(Hsla, &'static str)> {
        if self.activity.needs_you > 0 {
            Some((ui::warning(), "An agent here needs you"))
        } else if self.activity.working > 0 {
            Some((ui::accent(), "Agents are working here"))
        } else {
            None
        }
    }

    /// Name, tab count and activity, for a tooltip.
    pub fn tooltip(&self) -> SharedString {
        let tabs = match self.tabs {
            1 => "1 tab".to_string(),
            n => format!("{n} tabs"),
        };
        match self.dot() {
            Some((_, what)) => format!("{} · {tabs} · {what}", self.name),
            None => format!("{} · {tabs}", self.name),
        }
        .into()
    }
}

/// What a tab not built yet shows: its file, or its kind and folder.
pub(super) fn saved_title(pane: &SavedPane) -> SharedString {
    let name = |path: &Path| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    };
    if let Some(path) = &pane.path {
        return name(path).into();
    }
    let kind = match pane.kind {
        PaneKind::Agent => "Agent",
        PaneKind::Terminal => "Terminal",
        PaneKind::File => "File",
        PaneKind::History => "Git",
        PaneKind::Canvas => "Canvas",
        PaneKind::Tower => "Tower",
    };
    format!("{kind} · {}", name(&pane.cwd)).into()
}

impl Workspace {
    /// The left sidebar's content.
    pub(super) fn render_tab_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.sidebar_compact {
            self.render_compact_sidebar(cx)
        } else {
            self.render_accordion_sidebar(cx)
        }
    }

    pub(super) fn sidebar_spaces(&self) -> Vec<SpaceEntry> {
        self.spaces
            .iter()
            .enumerate()
            .map(|(index, space)| SpaceEntry {
                index,
                name: space.name.clone().into(),
                mark: Mark::of(space),
                current: index == self.current,
                activity: self.space_activity(space),
                tabs: space.tabs.len(),
            })
            .collect()
    }

    /// The tabs of workspace `index`: built ones with their panes' titles and
    /// state, unbuilt ones from what was saved (idle, no live pane).
    pub(super) fn sidebar_tabs(&self, index: usize) -> Vec<TabEntry> {
        let Some(space) = self.spaces.get(index) else {
            return Vec::new();
        };
        let current = index == self.current;
        space
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let shown = current && i == space.active;
                match &tab.state {
                    TabState::Unbuilt(saved) => {
                        let focused = tab.state.saved_focused();
                        TabEntry {
                            space: index,
                            index: i,
                            id: tab.id,
                            pane: None,
                            title: focused.map(saved_title).unwrap_or_default(),
                            kind: focused.map_or(PaneKind::Agent, |p| p.kind),
                            shown,
                            pinned: tab.pinned,
                            running: false,
                            needs_you: false,
                            panes: saved.layout.len(),
                        }
                    }
                    TabState::Built { tree, focused } => {
                        let leaves = tree.leaves();
                        TabEntry {
                            space: index,
                            index: i,
                            id: tab.id,
                            pane: Some(*focused),
                            title: self.pane_title(*focused),
                            kind: self
                                .panes
                                .get(focused)
                                .map_or(PaneKind::Agent, |p| p.item.kind),
                            shown,
                            pinned: tab.pinned,
                            running: self.tab_running(tab),
                            needs_you: leaves
                                .iter()
                                .any(|pane| self.panes.get(pane).is_some_and(|p| p.needs_you)),
                            panes: leaves.len(),
                        }
                    }
                }
            })
            .collect()
    }

    /// A click on a tab of any workspace: switch there first, then show it;
    /// a double-click on one of the shown workspace renames it. Modifier
    /// clicks in the shown workspace update the multi-selection instead.
    pub(super) fn open_sidebar_tab(
        &mut self,
        space: usize,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if space != self.current {
            // Make the clicked tab the target's active one first, so only it
            // is built when the workspace opens.
            if let Some(target) = self.spaces.get_mut(space)
                && index < target.tabs.len()
            {
                target.active = index;
            }
            self.switch_workspace(space, window, cx);
        } else {
            // Ranges follow the tabs the search leaves visible.
            let visible: Vec<usize> = self.visible_tabs(space, cx).iter().map(|t| t.id).collect();
            self.on_tab_click(index, event, Some(visible), window, cx);
        }
    }

    /// Whether the Accordion shows workspace `index`'s tabs.
    pub(super) fn sidebar_space_open(&self, index: usize) -> bool {
        self.spaces
            .get(index)
            .is_some_and(|space| space.sidebar_expanded)
    }

    pub(super) fn toggle_sidebar_space(&mut self, index: usize, cx: &mut Context<Self>) {
        let open = !self.sidebar_space_open(index);
        if let Some(space) = self.spaces.get_mut(index) {
            space.sidebar_expanded = open;
            self.schedule_save(cx);
            cx.notify();
        }
    }

    /// What a tab row does: a click opens the tab, and a tab of
    /// the shown workspace drags to reorder and has the tab menu on a
    /// right-click. Add it after the row's children.
    pub(super) fn tab_row_behavior(
        &self,
        row: Stateful<Div>,
        tab: &TabEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (space, index, id) = (tab.space, tab.index, tab.id);
        let row = row.cursor_pointer().on_click(cx.listener(
            move |this, event: &ClickEvent, window, cx| {
                this.open_sidebar_tab(space, index, event, window, cx)
            },
        ));
        let tabs = &self.spaces[space].tabs;
        let actions = TabMenu {
            space,
            id,
            can_rename: tab.kind == PaneKind::Agent && tab.pane.is_some() && space == self.current,
            pinned: tab.pinned,
            can_up: index > 0 && tabs[index - 1].pinned == tab.pinned,
            can_down: index + 1 < tabs.len() && tabs[index + 1].pinned == tab.pinned,
            compact: false,
        };
        // The menu works on any tab, built or not, in any workspace: it only
        // reorders, pins or closes. Dragging reorders within the shown one.
        let row = row.on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.open_tab_menu(actions, event.position, window, cx);
            }),
        );
        let row = row.on_drag(
            TabDrag {
                tab: id,
                title: tab.title.clone(),
            },
            |drag, _, _, cx| cx.new(|_| DragChip(drag.title.clone())),
        );
        if space == self.current {
            row.drag_over::<TabDrag>(|style, _, _, _| style.bg(ui::petal().opacity(0.18)))
                .on_drop(cx.listener(move |this, drag: &TabDrag, _, cx| {
                    this.move_tab(drag.tab, index, cx);
                }))
                .into_any_element()
        } else {
            row.into_any_element()
        }
    }

    /// A tab's title, or the rename field while it is being renamed.
    pub(super) fn tab_title(
        &self,
        tab: &TabEntry,
        size: f32,
        color: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(rename) = self
            .renaming
            .as_ref()
            .filter(|r| tab.space == self.current && Some(r.pane) == tab.pane)
        {
            return div()
                .flex_1()
                .min_w_0()
                .h(rems(20. / 16.))
                .px_1()
                .rounded_md()
                .bg(ui::well())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_action(cx.listener(Self::cancel_tab_rename))
                .child(Input::new(&rename.input).appearance(false).xsmall())
                .into_any_element();
        }
        div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .text_size(rems(size / 16.))
            .text_color(color)
            .child(tab.title.clone())
            .into_any_element()
    }

    /// A tab's kind icon in its state's tone.
    pub(super) fn tab_icon(tab: &TabEntry) -> Icon {
        Icon::new(tab.kind.icon()).small().text_color(tab.tone())
    }

    /// The + that opens the new-tab menu (Agent, Terminal, Canvas, Tower),
    /// with `label` beside it when there is room.
    pub(super) fn new_tab_button(
        &self,
        id: &'static str,
        label: Option<&'static str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let weak = cx.entity().downgrade();
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(Icon::new(IconName::Plus))
            .when_some(label, |b, label| b.label(label))
            .tooltip("New tab")
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .dropdown_menu(move |menu, window, _| new_tab_menu(menu, weak.clone(), window))
            .on_open_change(move |open, window, cx| {
                crate::features::browser::popover_open(id, *open, window, cx)
            })
            .into_any_element()
    }

    /// Open the workspace switcher, where workspaces are made and removed.
    pub(super) fn open_workspace_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette(Category::Workspaces, window, cx);
    }

    /// New agent and new terminal, at the foot of the sidebar.
    pub(super) fn new_tab_tiles(
        &self,
        (agent, terminal): (&'static str, &'static str),
        cx: &mut Context<Self>,
    ) -> Div {
        h_flex()
            .flex_none()
            .p_2()
            .gap_1p5()
            .child(Self::new_tab_tile(
                agent,
                IconName::Bot,
                "Agent",
                ui::platform_shortcut("⌘T", "Ctrl+T"),
                "New agent tab",
                cx.listener(|this, _, window, cx| this.on_new_tab(&NewTab, window, cx)),
            ))
            .child(Self::new_tab_tile(
                terminal,
                IconName::SquareTerminal,
                "Terminal",
                ui::platform_shortcut("⌘⇧T", "Ctrl+Shift+T"),
                "New terminal tab",
                cx.listener(|this, _, window, cx| {
                    this.on_new_terminal_tab(&NewTerminalTab, window, cx)
                }),
            ))
    }
}
