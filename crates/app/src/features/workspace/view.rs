//! Title bar chrome, tabs, pane tree and pane rendering.

use super::badge::{Mark, workspace_avatar};
use super::menu::WorkspaceMenuSource;
use super::tab_panel::TabMenu;
use super::*;
use gpui_kit::component::Selectable as _;

impl Workspace {
    // ── Rendering ───────────────────────────────────────────────────────────

    // The chrome reads each pane's `title`/`running` snapshot, never the
    // views themselves: see `Pane::title`.
    pub(super) fn tab_running(&self, tab: &Tab) -> bool {
        tab.leaves()
            .iter()
            .any(|id| self.panes.get(id).is_some_and(|p| p.running))
    }

    /// What the agents of `space` do. A tab not built yet has no
    /// panes, so it counts as idle.
    pub(super) fn space_activity(&self, space: &Space) -> Activity {
        let mut activity = Activity::default();
        for id in space.tabs.iter().flat_map(|tab| tab.leaves()) {
            match self.panes.get(&id) {
                Some(pane) if pane.needs_you => activity.needs_you += 1,
                Some(pane) if pane.running => activity.working += 1,
                _ => {}
            }
        }
        activity
    }

    /// What the agents of the other workspaces do, for the workspace chip.
    fn elsewhere_activity(&self) -> Activity {
        let mut total = Activity::default();
        for (index, space) in self.spaces.iter().enumerate() {
            if index != self.current {
                let activity = self.space_activity(space);
                total.working += activity.working;
                total.needs_you += activity.needs_you;
            }
        }
        total
    }

    pub(super) fn pane_title(&self, id: PaneId) -> SharedString {
        self.panes
            .get(&id)
            .map(|p| p.title.clone())
            .unwrap_or_default()
    }

    pub(super) fn render_workspace_chip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.space().name.clone();
        // Agents in the other workspaces: a dot here, details in the switcher.
        let elsewhere = self.elsewhere_activity();
        let (tip, dot) = if elsewhere.needs_you > 0 {
            (
                "Switch workspace · an agent in another one needs you",
                Some(ui::warning()),
            )
        } else if elsewhere.working > 0 {
            (
                "Switch workspace · agents are working in another one",
                Some(ui::accent()),
            )
        } else {
            ("Switch workspace", None)
        };
        h_flex()
            .id("workspace-chip")
            .h(rems(TITLE_ITEM / 16.))
            .pl(rems(5. / 16.))
            .pr(rems(6. / 16.))
            .gap(rems(7. / 16.))
            .flex_none()
            // Same shape and hover as a tab, with a lighter backing than the
            // active tab's: it labels the workspace (as in Tern) without
            // competing with it, and stays readable over a see-through
            // window or a busy backdrop.
            .rounded(rems(8. / 16.))
            .border_1()
            .border_color(ui::border().opacity(0.6))
            .bg(ui::popover_bg().opacity(0.55))
            .text_size(rems(12.5 / 16.))
            .cursor_pointer()
            .hover(|s| s.bg(ui::popover_bg().opacity(0.8)))
            .when(!self.popups_open(), |chip| {
                chip.tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
            })
            // Keep clicks on the chip from turning into window drags: `occlude`
            // hides the title bar's drag area from Windows' hit test, and
            // stopping the mouse-down keeps TitleBar from starting a move.
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_palette(Category::Workspaces, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_workspace_menu(
                        this.current,
                        WorkspaceMenuSource::Chip,
                        event.position,
                        window,
                        cx,
                    );
                }),
            )
            .child(workspace_avatar(&Mark::of(self.space())))
            .map(|chip| {
                if self.renaming_workspace(self.current, WorkspaceMenuSource::Chip) {
                    chip.child(
                        div()
                            .w(rems(160. / 16.))
                            .child(self.workspace_rename_input(cx)),
                    )
                } else {
                    chip.child(
                        div()
                            .max_w(rems(160. / 16.))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(ui::fg())
                            .child(name),
                    )
                }
            })
            .when_some(dot, |d, color| {
                d.child(
                    div()
                        .size(rems(6. / 16.))
                        .flex_none()
                        .rounded_full()
                        .bg(color),
                )
            })
            .child(
                Icon::new(IconName::ChevronDown)
                    .xsmall()
                    .text_color(ui::faint()),
            )
    }

    pub(super) fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let space = self.space();
        let weak = cx.entity().downgrade();
        h_flex()
            .id("horizontal-tab-strip")
            .gap(px(2.))
            .min_w_0()
            .children(space.tabs.iter().enumerate().map(|(i, tab)| {
                let active = i == space.active;
                let running = self.tab_running(tab);
                let focused = tab.focused();
                let saved = tab.state.saved_focused();
                let kind = match (&focused, &saved) {
                    (Some(id), _) => self.panes.get(id).map(|p| p.item.kind),
                    (None, Some(saved)) => Some(saved.kind),
                    _ => None,
                }
                .unwrap_or(PaneKind::Agent);
                let count = tab.state.len();
                let title = match (&focused, &saved) {
                    (Some(id), _) => self.pane_title(*id),
                    (None, Some(saved)) => sidebars::saved_title(saved),
                    _ => SharedString::default(),
                };
                let icon_color = if active { ui::petal() } else { ui::faint() };
                let is_tab_selected = self.is_tab_selected(self.current, tab.id);
                let drag = TabDrag {
                    tab: tab.id,
                    title: title.clone(),
                };
                let tab_menu = TabMenu {
                    space: self.current,
                    id: tab.id,
                    can_rename: kind == PaneKind::Agent && tab.is_built(),
                    pinned: tab.pinned,
                    can_up: false,
                    can_down: false,
                    compact: true,
                };
                let item = h_flex()
                    .min_w_0()
                    .overflow_hidden()
                    .id(("tab", tab.id))
                    .group("tab")
                    .h(rems(TITLE_ITEM / 16.))
                    .pl(rems(10. / 16.))
                    .pr(rems(4. / 16.))
                    .gap(rems(7. / 16.))
                    .rounded(rems(8. / 16.))
                    .text_size(rems(12.5 / 16.))
                    .cursor_pointer()
                    .relative()
                    // The fill marks the active tab, like Tern; the petal icon
                    // carries the accent. The fill is the opaque popover layer
                    // with a hairline, so it reads over a see-through window
                    // and a busy backdrop alike; every tab keeps the border's
                    // width so none shifts when another is picked. A selected
                    // tab, the active one included, is ringed in petal.
                    .border_1()
                    .when(active, |this| {
                        this.bg(ui::popover_bg().opacity(0.92))
                            .border_color(ui::border())
                            .shadow_sm()
                            .text_color(ui::fg())
                    })
                    .when(!active && is_tab_selected, |this| {
                        this.bg(ui::raised()).text_color(ui::fg())
                    })
                    .when(!active && !is_tab_selected, |this| {
                        this.border_color(gpui_kit::transparent_black())
                            .text_color(ui::muted())
                            .hover(|s| s.bg(ui::surface_hover().opacity(0.6)).text_color(ui::fg()))
                    })
                    .when(is_tab_selected, |this| {
                        this.border_color(ui::petal().opacity(0.45))
                    })
                    // Keep clicks on tabs from turning into window drags (see the
                    // workspace chip).
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_tab_menu(tab_menu, event.position, window, cx);
                        }),
                    )
                    // A double-click on an agent tab renames its session.
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.on_tab_click(i, event, None, window, cx);
                    }))
                    // Drag a tab onto another to put it there.
                    .on_drag(drag, |drag, _, _, cx| {
                        cx.new(|_| DragChip(drag.title.clone()))
                    })
                    .drag_over::<TabDrag>(|style, _, _, _| style.bg(ui::petal().opacity(0.18)))
                    .on_drop(cx.listener(move |this, drag: &TabDrag, _, cx| {
                        this.move_tab(drag.tab, i, cx)
                    }))
                    .child(
                        div()
                            .size(rems(14. / 16.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(if running {
                                ui::pulse(
                                    div().size(rems(7. / 16.)).rounded_full().bg(ui::accent()),
                                )
                                .into_any_element()
                            } else if kind == PaneKind::Agent {
                                div()
                                    .text_size(rems(13. / 16.))
                                    .line_height(relative(1.))
                                    .text_color(icon_color)
                                    .child("❀")
                                    .into_any_element()
                            } else {
                                Icon::new(kind.icon())
                                    .small()
                                    .text_color(icon_color)
                                    .into_any_element()
                            }),
                    )
                    .child(
                        match self.renaming.as_ref().filter(|r| Some(r.pane) == focused) {
                            Some(rename) => div()
                                .w(rems(160. / 16.))
                                .h(rems(20. / 16.))
                                .px_1()
                                .flex()
                                .items_center()
                                .rounded(rems(5. / 16.))
                                .bg(ui::well())
                                // Clicks in the field edit it rather than the tab.
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_action(cx.listener(Self::cancel_tab_rename))
                                .child(Input::new(&rename.input).appearance(false).xsmall())
                                .into_any_element(),
                            None => {
                                let title = title.clone();
                                div()
                                    .id(("tab-title", tab.id))
                                    .min_w_0()
                                    .max_w(rems(180. / 16.))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .when(active, |d| d.font_weight(FontWeight::MEDIUM))
                                    .when(!self.popups_open(), |d| {
                                        let title = title.clone();
                                        d.tooltip(move |window, cx| {
                                            Tooltip::new(title.clone()).build(window, cx)
                                        })
                                    })
                                    .child(title)
                                    .into_any_element()
                            }
                        },
                    )
                    .when(count > 1, |d| {
                        d.child(
                            div()
                                .h(rems(1.))
                                .min_w(rems(1.))
                                .px(rems(4. / 16.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(rems(5. / 16.))
                                .bg(ui::surface_hover().opacity(0.7))
                                .text_size(rems(10. / 16.))
                                .line_height(relative(1.))
                                .text_color(ui::muted())
                                .child(count.to_string()),
                        )
                    })
                    .child(
                        div()
                            .id(("close-tab", tab.id))
                            .size(rems(18. / 16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(rems(5. / 16.))
                            .text_color(ui::faint())
                            .invisible()
                            .group_hover("tab", |s| s.visible())
                            .hover(|s| s.bg(ui::surface_hover()).text_color(ui::fg()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(i, window, cx);
                            }))
                            .child(Icon::new(IconName::X).xsmall()),
                    );
                // Tabs shrink (titles end in …) so the title bar's buttons
                // keep their place in a narrow window.
                ui::appear(("tab-in", tab.id), div().flex().min_w_0().child(item))
            }))
            .child(
                div()
                    .flex_none()
                    .ml(px(2.))
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .child(
                        title_button("new-tab", IconName::Plus)
                            .dropdown_menu(move |menu, window, _| {
                                new_tab_menu(menu, weak.clone(), window)
                            })
                            .on_open_change(|open, window, cx| {
                                crate::features::browser::popover_open("new-tab", *open, window, cx)
                            }),
                    ),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_empty_area_menu(event.position, window, cx);
                }),
            )
    }

    /// One title-bar button for the focused pane's layout: tile, split and
    /// close, each named with its shortcut (the menu takes arrow keys too).
    pub(super) fn render_layout_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let target = self
            .focused_pane()
            .map(|id| self.pane_title(id))
            .unwrap_or_default()
            .trim_end_matches(" •")
            .to_string();
        div()
            // Clicks here are not title-bar drags.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                title_button("title-layout", IconName::LayoutDashboard)
                    .tooltip("Panes: tile, split or close the focused one")
                    .dropdown_menu(move |menu, window, _| {
                        type Act = fn(&mut Workspace, &mut Window, &mut Context<Workspace>);
                        let entry = |icon: IconName,
                                     label: &'static str,
                                     detail: &'static str,
                                     keys: &'static str,
                                     act: Act| {
                            let weak = weak.clone();
                            PopupMenuItem::element(move |_, _| menu_row(icon, label, detail, keys))
                                .on_click(move |_, window, cx| {
                                    let _ = weak.update(cx, |this, cx| act(this, window, cx));
                                })
                        };
                        let close = {
                            let weak = weak.clone();
                            let target = target.clone();
                            PopupMenuItem::element(move |_, _| {
                                h_flex()
                                    .gap_2p5()
                                    .py_0p5()
                                    .w_full()
                                    .child(Icon::new(IconName::X).small().text_color(ui::danger()))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_sm()
                                            .child(format!("Close \u{201c}{target}\u{201d}")),
                                    )
                                    .child(ui::kbd(ui::platform_shortcut("⌘W", "Ctrl+W")))
                            })
                            .on_click(move |_, window, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.on_close_pane(&ClosePane, window, cx)
                                });
                            })
                        };
                        menu.min_w(rems(280. / 16.).to_pixels(window.rem_size()))
                            .item(entry(
                                IconName::LayoutDashboard,
                                "New pane, tiled",
                                "Same kind, beside the focused one",
                                ui::platform_shortcut("⌘D", "Ctrl+Shift+D"),
                                |this, window, cx| this.on_tile_pane(&TilePane, window, cx),
                            ))
                            .item(entry(
                                IconName::Columns2,
                                "Split right",
                                "Same kind, to the right",
                                "",
                                |this, window, cx| {
                                    this.split(SplitAxis::Horizontal, None, window, cx)
                                },
                            ))
                            .item(entry(
                                IconName::Rows2,
                                "Split down",
                                "Same kind, below",
                                ui::platform_shortcut("⌘⇧D", "Ctrl+Shift+E"),
                                |this, window, cx| this.on_split_down(&SplitDown, window, cx),
                            ))
                            .separator()
                            .item(close)
                    })
                    .on_open_change(|open, window, cx| {
                        crate::features::browser::popover_open("title-layout", *open, window, cx)
                    }),
            )
    }

    /// One title-bar button for the screens (Configuration, Profiles, Memory,
    /// Sessions, Providers): lit while one is open, each entry with what it
    /// is and its shortcut when it has one.
    pub(super) fn render_screens_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let open_screen = matches!(
            self.overlay,
            Some((
                Overlay::Settings(_)
                    | Overlay::Profiles(_)
                    | Overlay::Memory(_)
                    | Overlay::Sessions(_)
                    | Overlay::Providers(_),
                _
            ))
        );
        div()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                title_button("title-screens", IconName::Settings)
                    .selected(open_screen)
                    .tooltip(format!(
                        "Configuration, profiles, memory, sessions, providers ({})",
                        ui::platform_shortcut("⌘,", "Ctrl+,")
                    ))
                    .dropdown_menu(move |menu, window, _| {
                        type Open = fn(&mut Workspace, &mut Window, &mut Context<Workspace>);
                        let entry = |icon: IconName,
                                     label: &'static str,
                                     detail: &'static str,
                                     keys: &'static str,
                                     open: Open| {
                            let weak = weak.clone();
                            PopupMenuItem::element(move |_, _| menu_row(icon, label, detail, keys))
                                .on_click(move |_, window, cx| {
                                    let _ = weak.update(cx, |this, cx| open(this, window, cx));
                                })
                        };
                        menu.min_w(rems(280. / 16.).to_pixels(window.rem_size()))
                            .item(entry(
                                IconName::Settings,
                                "Configuration",
                                "Theme, models, integrations, updates",
                                ui::platform_shortcut("⌘,", "Ctrl+,"),
                                |this, window, cx| this.open_settings(window, cx),
                            ))
                            .item(entry(
                                IconName::Route,
                                "Profiles",
                                "Model and effort per agent",
                                "",
                                |this, window, cx| this.open_profiles(window, cx),
                            ))
                            .item(entry(
                                IconName::Brain,
                                "Memory",
                                "Engram observations",
                                "",
                                |this, window, cx| {
                                    let cwd = this.active_cwd(cx);
                                    this.open_memory(cwd, Default::default(), window, cx)
                                },
                            ))
                            .item(entry(
                                IconName::RotateCcwClock,
                                "Sessions",
                                "Resume a past session",
                                ui::platform_shortcut("⌘⇧O", "Ctrl+Shift+O"),
                                |this, window, cx| this.open_sessions(window, cx),
                            ))
                            .item(entry(
                                IconName::KeyRound,
                                "Providers",
                                "Sign-in and API keys",
                                "",
                                |this, window, cx| this.open_providers(window, cx),
                            ))
                    })
                    .on_open_change(|open, window, cx| {
                        crate::features::browser::popover_open("title-screens", *open, window, cx)
                    }),
            )
    }

    pub(super) fn render_tree(
        &self,
        tab: &Tab,
        node: &PaneTree<PaneId>,
        path: &mut Vec<bool>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            PaneTree::Leaf(id) => self.render_pane(tab, *id, window, cx),
            PaneTree::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let axis = *axis;
                let horizontal = axis == SplitAxis::Horizontal;
                path.push(false);
                let first = self.render_tree(tab, first, path, window, cx);
                path.pop();
                path.push(true);
                let second = self.render_tree(tab, second, path, window, cx);
                path.pop();
                let key = path
                    .iter()
                    .map(|b| if *b { '1' } else { '0' })
                    .collect::<String>();
                let drag = DividerDrag {
                    tab: tab.id,
                    path: path.clone(),
                };
                let (tab_id, here) = (tab.id, path.clone());
                let cell = |child: AnyElement, share: f32| {
                    div()
                        .flex()
                        // Zero basis, grow by share: the space left after the
                        // divider splits exactly at the ratio.
                        .flex_basis(px(0.))
                        .flex_grow(share)
                        .flex_shrink(1.)
                        .min_w_0()
                        .min_h_0()
                        .when(horizontal, |d| d.h_full())
                        .when(!horizontal, |d| d.w_full())
                        .child(child)
                };
                div()
                    .flex()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .when(horizontal, |d| d.flex_row())
                    .when(!horizontal, |d| d.flex_col())
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<DividerDrag>, _, cx| {
                            let drag = event.drag(cx);
                            if drag.tab != tab_id || drag.path != here {
                                return;
                            }
                            let bounds = event.bounds;
                            let position = event.event.position;
                            let share = if horizontal {
                                (position.x - bounds.left()) / bounds.size.width
                            } else {
                                (position.y - bounds.top()) / bounds.size.height
                            };
                            // Keep both sides at least the minimum pane size
                            // (when the split has room for that at all).
                            let (total, min) = if horizontal {
                                (f32::from(bounds.size.width), MIN_PANE_WIDTH)
                            } else {
                                (f32::from(bounds.size.height), MIN_PANE_HEIGHT)
                            };
                            let bound = min / total.max(1.);
                            let share = if bound < 0.5 {
                                share.clamp(bound, 1. - bound)
                            } else {
                                share
                            };
                            let path = here.clone();
                            if let Some(tab) = this.tab_mut()
                                && tab.id == tab_id
                                && tab
                                    .built_tree_mut()
                                    .is_some_and(|tree| tree.set_ratio(&path, share))
                            {
                                this.schedule_save(cx);
                                cx.notify();
                            }
                        },
                    ))
                    .child(cell(first, *ratio))
                    .child(
                        div()
                            .id(SharedString::from(format!("divider-{}-{key}", tab.id)))
                            .group("divider")
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(horizontal, |d| d.w(px(6.)).h_full().cursor_col_resize())
                            .when(!horizontal, |d| d.h(px(6.)).w_full().cursor_row_resize())
                            .on_drag(drag, |_, _, _, cx| cx.new(|_| Invisible))
                            .child(crate::shared::floating::divider_handle(
                                "divider", horizontal,
                            )),
                    )
                    .child(cell(second, 1.0 - *ratio))
                    .into_any_element()
            }
        }
    }

    pub(super) fn render_pane(
        &self,
        tab: &Tab,
        id: PaneId,
        _window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(pane) = self.panes.get(&id) else {
            return div().into_any_element();
        };
        let tiled = tab.state.len() > 1;
        let focused = tiled && tab.focused() == Some(id);
        let title = pane.title.clone();
        let body = div()
            .id(("pane", id))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .rounded_xl()
            .bg(ui::panel())
            // Panes read as glass on their own; only a split's focused pane gets
            // an edge. The width stays so focus changes don't shift content.
            .border_1()
            .border_color(if focused {
                ui::petal().opacity(0.45)
            } else {
                gpui_kit::transparent_black()
            })
            .overflow_hidden()
            .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                if let Some(tab) = this.tab_mut()
                    && tab.focused() != Some(id)
                    && tab.state.contains(&id)
                {
                    tab.state.set_focused(id);
                    cx.notify();
                }
            }))
            // Dragging a pane over this one: track which zone the pointer is
            // in (only while it's over this pane, and never over itself).
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<PaneDrag>, _, cx| {
                    let bounds = event.bounds;
                    let position = event.event.position;
                    let hint =
                        (bounds.contains(&position) && event.drag(cx).pane != id).then(|| {
                            let x = (position.x - bounds.left()) / bounds.size.width;
                            let y = (position.y - bounds.top()) / bounds.size.height;
                            (id, DropZone::at(x, y))
                        });
                    match hint {
                        Some(hint) if this.drop_hint != Some(hint) => {
                            this.drop_hint = Some(hint);
                            cx.notify();
                        }
                        // Only this pane's own hint is its to clear.
                        None if this.drop_hint.is_some_and(|(pane, _)| pane == id) => {
                            this.drop_hint = None;
                            cx.notify();
                        }
                        _ => {}
                    }
                }),
            )
            .on_drop(cx.listener(move |this, drag: &PaneDrag, window, cx| {
                let zone = this
                    .drop_hint
                    .take()
                    .filter(|(pane, _)| *pane == id)
                    .map_or(DropZone::Center, |(_, zone)| zone);
                this.move_pane(drag.pane, id, zone, window, cx);
            }))
            // The pink border alone marks focus: a straight accent line on top
            // can't follow the rounded corners now that panes have no header.
            // Cached: a notify elsewhere (another pane, the chrome) reuses this
            // pane's last frame instead of rebuilding it.
            .child(
                div().flex_1().min_h_0().child(
                    pane.item
                        .view
                        .clone()
                        .cached(StyleRefinement::default().size_full()),
                ),
            )
            // An agent's live indicators, over it: their frames redraw the
            // layer and this window, while the agent's view stays cached.
            .children(
                pane.session
                    .as_ref()
                    .map(|session| session.read(cx).live_layer()),
            )
            .children(
                self.drop_hint
                    .filter(|(pane, _)| *pane == id && cx.has_active_drag())
                    .map(|(_, zone)| drop_overlay(zone)),
            );
        // The grip: a pill straddling the pane's top edge (outside the
        // clipped body); drag it onto another pane's edge to move the pane
        // there, or onto its middle to swap the two.
        let grip = h_flex()
            .absolute()
            // Inside the ~4 px gap under the title bar, not over the tabs.
            .top(rems(-4. / 16.))
            .left_0()
            .right_0()
            .justify_center()
            .child(
                div()
                    .id(("pane-grip", id))
                    .w(rems(34. / 16.))
                    .h(rems(14. / 16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(ui::surface())
                    .border_1()
                    .border_color(if focused {
                        ui::petal().opacity(0.45)
                    } else {
                        ui::border()
                    })
                    .text_color(ui::faint())
                    .cursor_grab()
                    .invisible()
                    .when(focused, |d| d.visible())
                    .group_hover(SharedString::from(format!("pane-wrap-{id}")), |s| {
                        s.visible()
                    })
                    .hover(|s| s.text_color(ui::fg()).border_color(ui::border_strong()))
                    .on_drag(
                        PaneDrag {
                            pane: id,
                            title: title.clone(),
                        },
                        |drag, _, _, cx| cx.new(|_| DragChip(drag.title.clone())),
                    )
                    .child(Icon::new(IconName::GripHorizontal).xsmall()),
            );
        ui::appear(
            ("pane-in", id),
            div()
                .group(SharedString::from(format!("pane-wrap-{id}")))
                .relative()
                .flex()
                .size_full()
                .child(body)
                .when(tiled, |d| d.child(grip)),
        )
        .into_any_element()
    }

    pub(super) fn render_body(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        match self.tab() {
            Some(tab) => match tab.built_tree() {
                Some(tree) => {
                    let mut path = Vec::new();
                    self.render_tree(tab, tree, &mut path, window, cx)
                }
                // `activate` builds the shown tab before it is drawn.
                None => div().into_any_element(),
            },
            None => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .text_sm()
                .text_color(ui::muted())
                .child("No agents in this workspace yet")
                .child(
                    Button::new("empty-new-tab")
                        .primary()
                        .small()
                        .icon(Icon::new(IconName::Plus))
                        .label("Open agent")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.on_new_tab(&NewTab, window, cx)),
                        ),
                )
                .into_any_element(),
        }
    }
}

/// Height of the title bar's pills, tabs and buttons.
pub(super) const TITLE_ITEM: f32 = 28.;

/// A quiet square icon button for the title bar.
pub(super) fn title_button(id: &'static str, icon: IconName) -> Button {
    Button::new(id).ghost().xsmall().icon(Icon::new(icon))
}

/// The new-tab menu (Agent, Terminal, Canvas, Tower), shared by the title
/// bar's + and the vertical navigator's.
pub(super) fn new_tab_menu(
    menu: PopupMenu,
    weak: WeakEntity<Workspace>,
    window: &Window,
) -> PopupMenu {
    let item = |icon: IconName, label: &'static str, detail: &'static str, keys: &'static str| {
        PopupMenuItem::element(move |_, _| menu_row(icon, label, detail, keys))
    };
    let (agent, terminal, canvas, tower) = (weak.clone(), weak.clone(), weak.clone(), weak);
    menu.min_w(rems(260. / 16.).to_pixels(window.rem_size()))
        .item(
            item(
                IconName::Bot,
                "Agent tab",
                "Gentle Shell in this folder",
                ui::platform_shortcut("⌘T", "Ctrl+T"),
            )
            .on_click(move |_, window, cx| {
                let _ = agent.update(cx, |this, cx| this.on_new_tab(&NewTab, window, cx));
            }),
        )
        .item(
            item(
                IconName::SquareTerminal,
                "Terminal tab",
                "A shell in this folder",
                ui::platform_shortcut("⌘⇧T", "Ctrl+Shift+T"),
            )
            .on_click(move |_, window, cx| {
                let _ = terminal.update(cx, |this, cx| {
                    this.on_new_terminal_tab(&NewTerminalTab, window, cx)
                });
            }),
        )
        .item(
            item(
                IconName::PenTool,
                "Canvas tab",
                "A whiteboard: ink, shapes, notes",
                ui::platform_shortcut("⌘⇧E", "Ctrl+Alt+Shift+E"),
            )
            .on_click(move |_, window, cx| {
                let _ = canvas.update(cx, |this, cx| {
                    this.on_new_canvas_tab(&NewCanvasTab, window, cx)
                });
            }),
        )
        .item(
            item(
                IconName::Building2,
                "Tower tab",
                "Gentle Tower: every session as a floor",
                "",
            )
            .on_click(move |_, window, cx| {
                let _ = tower.update(cx, |this, cx| this.open_tower(window, cx));
            }),
        )
}

/// A two-line menu entry: icon, label, muted description, shortcut (none
/// when `keys` is empty).
pub(super) fn menu_row(
    icon: IconName,
    label: &'static str,
    detail: &'static str,
    keys: &'static str,
) -> impl IntoElement {
    h_flex()
        .gap_2p5()
        .py_0p5()
        .w_full()
        .child(
            div()
                .size(rems(26. / 16.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .border_1()
                .border_color(ui::border_strong())
                .bg(ui::well())
                .text_color(ui::petal())
                .child(Icon::new(icon).small()),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().text_color(ui::fg()).child(label))
                .child(div().text_xs().text_color(ui::muted()).child(detail)),
        )
        .when(!keys.is_empty(), |d| d.child(ui::kbd(keys)))
}

/// Where a dragged pane would land on the pane under the pointer: that half
/// for an edge, the whole pane (a swap) in the middle.
fn drop_overlay(zone: DropZone) -> impl IntoElement {
    let area = div()
        .absolute()
        .m(px(6.))
        .rounded_xl()
        .bg(ui::petal().opacity(0.12))
        .border_2()
        .border_color(ui::petal().opacity(0.6));
    let area = match zone {
        DropZone::Left => area.left_0().top_0().bottom_0().w(relative(0.5)),
        DropZone::Right => area.right_0().top_0().bottom_0().w(relative(0.5)),
        DropZone::Top => area.top_0().left_0().right_0().h(relative(0.5)),
        DropZone::Bottom => area.bottom_0().left_0().right_0().h(relative(0.5)),
        DropZone::Center => area.inset_0(),
    };
    ui::popover_in(("pane-drop", zone as usize), area)
}
