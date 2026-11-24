//! The sidebar's pieces, measured like the Files sidebar: a 40 px header,
//! the compact search field, 26 px rows, section labels, and the Agent /
//! Terminal tiles at the bottom.

use super::super::badge::workspace_avatar;
use super::super::menu::WorkspaceMenuSource;
use super::*;

/// A row, as tall as a Files tree row.
pub(super) const ROW: f32 = 26.;
/// A tab row's indent under its workspace row: the chevron and its gap, so
/// the tab's icon lines up with the workspace's badge.
pub(super) const INDENT: f32 = 22.;

impl Workspace {
    /// The header: the shown workspace (a click switches, a right-click
    /// opens its menu), then `actions`.
    pub(super) fn sidebar_header(&self, actions: Vec<AnyElement>, cx: &mut Context<Self>) -> Div {
        let space = self.space();
        let mark = Mark::of(space);
        let index = self.current;
        h_flex()
            .h(rems(40. / 16.))
            .flex_none()
            .pl_3()
            .pr_1p5()
            .gap_0p5()
            .child(
                h_flex()
                    .id("sidebar-workspace")
                    .flex_1()
                    .min_w_0()
                    .h(rems(ROW / 16.))
                    .ml_neg_1()
                    .px_1()
                    .gap_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(ui::surface_hover().opacity(0.6)))
                    .when(!self.popups_open(), |d| {
                        d.tooltip(|window, cx| {
                            Tooltip::new("Switch workspace · right-click for its menu")
                                .build(window, cx)
                        })
                    })
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_workspace_switcher(window, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_workspace_menu(
                                index,
                                WorkspaceMenuSource::Header,
                                event.position,
                                window,
                                cx,
                            );
                        }),
                    )
                    .child(workspace_avatar(&mark))
                    .map(|d| {
                        if self.renaming_workspace(index, WorkspaceMenuSource::Header) {
                            d.child(self.workspace_rename_input(cx))
                        } else {
                            d.child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(rems(13.5 / 16.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(ui::fg())
                                    .child(space.name.clone()),
                            )
                        }
                    })
                    .child(
                        Icon::new(IconName::ChevronsUpDown)
                            .xsmall()
                            .text_color(ui::faint()),
                    ),
            )
            .children(actions)
    }

    /// New tab, and the control to collapse the workspace navigator.
    pub(super) fn header_actions(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            self.new_tab_button("sidebar-new-tab", None, cx),
            Button::new("sidebar-collapse")
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::PanelLeftClose))
                .tooltip("Collapse workspace sidebar")
                .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar_compact(cx)))
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .into_any_element(),
        ]
    }

    /// The search field; it filters the tabs of every workspace.
    pub(super) fn sidebar_search(&self) -> Div {
        h_flex().flex_none().px_2().pb_1p5().child(
            ui::field()
                .flex_1()
                .min_w_0()
                .h(rems(30. / 16.))
                // The field's own Cut/Copy/Paste menu is the only one.
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(Icon::new(IconName::Search).small().text_color(ui::faint()))
                .child(Input::new(&self.tab_search).appearance(false).small()),
        )
    }

    /// Workspace `index`'s tabs that match the search.
    pub(super) fn visible_tabs(&self, index: usize, cx: &App) -> Vec<TabEntry> {
        let query = self.tab_search.read(cx).value().trim().to_lowercase();
        let mut tabs = self.sidebar_tabs(index);
        if !query.is_empty() {
            let workspace_matches = self.spaces[index].name.to_lowercase().contains(&query);
            tabs.retain(|tab| {
                workspace_matches
                    || tab.title.to_lowercase().contains(&query)
                    || self.spaces[index].tabs[tab.index]
                        .leaves()
                        .iter()
                        .any(|id| self.pane_title(*id).to_lowercase().contains(&query))
                    || self.spaces[index].tabs[tab.index]
                        .state
                        .unbuilt()
                        .is_some_and(|saved| {
                            saved
                                .layout
                                .leaves()
                                .iter()
                                .any(|pane| saved_title(pane).to_lowercase().contains(&query))
                        })
            });
        }
        tabs
    }

    pub(super) fn searching(&self, cx: &App) -> bool {
        !self.tab_search.read(cx).value().trim().is_empty()
    }

    /// A direct workspace creation action, next to the list it affects.
    pub(super) fn workspace_section(&self, cx: &mut Context<Self>) -> Div {
        h_flex()
            .px_2()
            .pt_2()
            .pb_1()
            .justify_between()
            .child(ui::section_label("Workspaces"))
            .child(
                Button::new("sidebar-new-workspace")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::FolderPlus))
                    .tooltip("New workspace")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.pick_workspace_folder(None, window, cx);
                    })),
            )
    }

    pub(super) fn global_activity_row(&self) -> Div {
        let mut total = Activity::default();
        for space in &self.spaces {
            let activity = self.space_activity(space);
            total.working += activity.working;
            total.needs_you += activity.needs_you;
        }
        h_flex()
            .px_3()
            .py_1()
            .gap_2()
            .text_xs()
            .child(
                div()
                    .text_color(ui::accent())
                    .child(format!("{} running", total.working)),
            )
            .child(
                div()
                    .text_color(ui::warning())
                    .child(format!("{} waiting", total.needs_you)),
            )
    }

    /// The row a search with no match leaves, at `indent`.
    pub(super) fn quiet_row(text: &'static str, indent: f32) -> Div {
        div()
            .flex_none()
            .h(rems(ROW / 16.))
            .flex()
            .items_center()
            .pl(rems((6. + indent) / 16.))
            .text_xs()
            .text_color(ui::faint())
            .child(text)
    }

    /// A tab as a 26 px row: kind icon in its state's tone, title, pin,
    /// status dot and pane count; the shown tab raised, with the accent
    /// marker on its left. Selected tabs, the shown one included, are raised
    /// and ringed in petal.
    pub(super) fn tab_row(
        &self,
        tab: &TabEntry,
        indent: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tip = tab.tooltip();
        let selected = self.is_tab_selected(tab.space, tab.id);
        let row = h_flex()
            .id(SharedString::from(format!(
                "sidebar-tab-{}-{}",
                tab.space, tab.index
            )))
            .relative()
            .flex_none()
            .w_full()
            .h(rems(ROW / 16.))
            .pl(rems((6. + indent) / 16.))
            .pr_2()
            .gap_1p5()
            .rounded_md()
            // Every row keeps the ring's width so none shifts when selected.
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .when(tab.shown || selected, |d| d.bg(ui::raised()))
            .when(selected, |d| d.border_color(ui::petal().opacity(0.45)))
            .when(!tab.shown && !selected, |d| {
                d.hover(|s| s.bg(ui::surface_hover()))
            })
            .when(!self.popups_open(), |d| {
                d.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            })
            .when(tab.shown, |d| {
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(rems(6. / 16.))
                        .bottom(rems(6. / 16.))
                        .w(px(2.))
                        .rounded_full()
                        .bg(ui::petal()),
                )
            })
            .child(Self::tab_icon(tab))
            .child(self.tab_title(
                tab,
                12.5,
                if tab.shown { ui::fg() } else { ui::muted() },
                cx,
            ))
            .when(tab.pinned, |d| {
                d.child(Icon::new(IconName::Pin).xsmall().text_color(ui::faint()))
            })
            .children(tab.status_dot())
            .when(tab.panes > 1, |d| {
                d.child(
                    div()
                        .flex_none()
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(10.5))
                        .text_color(ui::faint())
                        .child(tab.panes.to_string()),
                )
            });
        self.tab_row_behavior(row, tab, cx)
    }

    /// Individual panes under a split tab, including restored placeholders.
    pub(super) fn split_sidebar_rows(
        &self,
        entry: &TabEntry,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let tab = &self.spaces[entry.space].tabs[entry.index];
        let space = entry.space;
        let tab_id = entry.id;
        let items: Vec<_> = match &tab.state {
            TabState::Built { tree, .. } => tree
                .leaves()
                .into_iter()
                .filter_map(|id| {
                    let pane = self.panes.get(&id)?;
                    Some((
                        Some(id),
                        pane.title.clone(),
                        pane.item.kind,
                        pane.running,
                        pane.needs_you,
                    ))
                })
                .collect(),
            TabState::Unbuilt(saved) => saved
                .layout
                .leaves()
                .iter()
                .map(|pane| (None, saved_title(pane), pane.kind, false, false))
                .collect(),
        };
        items
            .into_iter()
            .enumerate()
            .map(|(index, (pane, title, kind, running, needs_you))| {
                let selected = entry.shown && pane.is_some() && pane == entry.pane;
                let color = if needs_you {
                    ui::warning()
                } else if running {
                    ui::accent()
                } else {
                    ui::muted()
                };
                h_flex()
                    .id(SharedString::from(format!(
                        "sidebar-split-{tab_id}-{index}"
                    )))
                    .h(rems(ROW / 16.))
                    .pl(rems(30. / 16.))
                    .pr_2()
                    .gap_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(ui::raised()))
                    .hover(|d| d.bg(ui::surface_hover()))
                    .child(Icon::new(kind.icon()).small().text_color(color))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(ui::fg())
                            .child(title),
                    )
                    .when(running || needs_you, |d| {
                        d.child(div().size(px(6.)).rounded_full().bg(color))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(pane) = pane {
                            this.reveal_pane(pane, window, cx);
                        } else if let Some(tab_index) = this
                            .spaces
                            .get(space)
                            .and_then(|s| s.tabs.iter().position(|t| t.id == tab_id))
                        {
                            if let TabState::Unbuilt(saved) =
                                &mut this.spaces[space].tabs[tab_index].state
                            {
                                saved.focused = index;
                            }
                            this.spaces[space].active = tab_index;
                            this.switch_workspace(space, window, cx);
                        }
                    }))
                    .into_any_element()
            })
            .collect()
    }

    /// A workspace as a 26 px row: `chevron` (open or closed) when it
    /// folds, its badge, name, activity dot and tab count. A right-click
    /// opens its menu. The chevron only folds the group; the name selects
    /// the workspace with one click. Dragging a row reorders workspaces.
    pub(super) fn space_row(
        &self,
        space: &SpaceEntry,
        chevron: Option<bool>,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let tip = space.tooltip();
        let index = space.index;
        let name = space.name.clone();
        h_flex()
            .id((id, space.index))
            .relative()
            .flex_none()
            .w_full()
            .h(rems(ROW / 16.))
            .pl(rems(4. / 16.))
            .pr_2()
            .gap_1p5()
            .rounded_md()
            .cursor_pointer()
            .when(space.current, |d| d.bg(ui::raised()))
            .hover(|d| d.bg(ui::surface_hover()))
            .when(!self.popups_open(), |d| {
                d.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_workspace_menu(
                        index,
                        WorkspaceMenuSource::SidebarRow,
                        event.position,
                        window,
                        cx,
                    );
                }),
            )
            .when_some(chevron, |d, open| {
                d.child(
                    h_flex()
                        .id(("workspace-chevron", index))
                        .flex_none()
                        .h(rems(ROW / 16.))
                        .px_1()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_sidebar_space(index, cx);
                        }))
                        .child(
                            Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .xsmall()
                            .text_color(ui::faint()),
                        ),
                )
            })
            .child(
                h_flex()
                    .id(("workspace-select", index))
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .gap_1p5()
                    .when(
                        !self.renaming_workspace(index, WorkspaceMenuSource::SidebarRow),
                        |d| {
                            d.on_click(cx.listener(move |this, _, window, cx| {
                                if this.current != index {
                                    this.switch_workspace(index, window, cx);
                                }
                            }))
                        },
                    )
                    .child(workspace_avatar(&space.mark))
                    .map(|d| {
                        if self.renaming_workspace(index, WorkspaceMenuSource::SidebarRow) {
                            d.child(self.workspace_rename_input(cx))
                        } else {
                            d.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(rems(12.5 / 16.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if space.current { ui::fg() } else { ui::muted() })
                                    .child(space.name.clone()),
                            )
                        }
                    })
                    .when_some(space.dot(), |d, (color, _)| {
                        d.child(
                            div()
                                .size(rems(6. / 16.))
                                .flex_none()
                                .rounded_full()
                                .bg(color),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .font_family(ui::mono())
                            .text_size(ui::code_rems(10.5))
                            .text_color(ui::faint())
                            .child(space.tabs.to_string()),
                    ),
            )
            .child(
                h_flex()
                    .id(("workspace-new-session", index))
                    .flex_none()
                    .size(rems(22. / 16.))
                    .justify_center()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|d| d.bg(ui::surface_hover()))
                    .when(!self.popups_open(), |d| {
                        d.tooltip(|window, cx| {
                            Tooltip::new("New agent or terminal in this workspace")
                                .build(window, cx)
                        })
                    })
                    // Open the workspace-owned menu directly. Do not combine
                    // a stopped mouse-down with Button's dropdown trigger.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_workspace_add_menu(index, event.position, window, cx);
                        }),
                    )
                    .child(Icon::new(IconName::Plus).xsmall().text_color(ui::muted())),
            )
            .on_drag(
                WorkspaceDrag {
                    index,
                    name: name.clone(),
                },
                |drag, _, _, cx| cx.new(|_| DragChip(drag.name.clone())),
            )
            .drag_over::<TabDrag>(|style, _, _, _| style.bg(ui::petal().opacity(0.18)))
            .on_drop(cx.listener(move |this, drag: &TabDrag, window, cx| {
                this.move_tab_to_workspace(drag.tab, index, window, cx);
            }))
            .drag_over::<WorkspaceDrag>(|style, _, _, _| style.bg(ui::petal().opacity(0.18)))
            .on_drop(cx.listener(move |this, drag: &WorkspaceDrag, window, cx| {
                this.move_workspace(drag.index, index, window, cx);
            }))
    }
}
