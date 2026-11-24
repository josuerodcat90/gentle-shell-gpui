//! `Render` for the workspace window.

use super::*;

/// The title bar's height; overlay screens open below it.
pub(super) const TITLE_BAR_HEIGHT: f32 = 36.;

/// Clip a full-size panel as it slides toward the window edge, so its
/// controls never reflow at intermediate widths.
fn sidebar_clip(side: Side, width: f32, content: impl IntoElement) -> Div {
    div().size_full().relative().overflow_hidden().child(
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .when(matches!(side, Side::Tabs), |d| d.right_0())
            .when(matches!(side, Side::Files), |d| d.left_0())
            .w(rems(width / 16.))
            .rounded_xl()
            .bg(ui::panel())
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .overflow_hidden()
            .child(content),
    )
}

impl Workspace {
    fn sidebar_handle(&self, side: Side, cx: &mut Context<Self>) -> impl IntoElement {
        let (id, revision) = match side {
            Side::Tabs => ("tabs-resize-handle", self.tabs_sidebar_motion.revision),
            Side::Files => ("files-resize-handle", self.files_sidebar_motion.revision),
        };
        ui::resize_handle(id)
            .absolute()
            .top_0()
            .bottom_0()
            .when(matches!(side, Side::Tabs), |d| d.right(rems(-6. / 16.)))
            .when(matches!(side, Side::Files), |d| d.left(rems(-6. / 16.)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_drag(
                SidebarDrag::new(side, cx.entity_id(), revision),
                |_, _, _, cx| cx.new(|_| Invisible),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    let width = match side {
                        Side::Tabs => &mut this.tabs_sidebar_width,
                        Side::Files => &mut this.files_sidebar_width,
                    };
                    *width = gentle_core::workspaces::SIDEBAR_DEFAULT_WIDTH;
                    this.schedule_save(cx);
                    cx.notify();
                }
            }))
            .tooltip(|window, cx| {
                Tooltip::new("Drag to resize · double-click to reset").build(window, cx)
            })
    }

    fn resize_sidebar(
        &mut self,
        event: &DragMoveEvent<SidebarDrag>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        use gentle_core::workspaces::SidebarDrag::{Collapse, Reopen, Width};

        // The row survives the handle unmounting. A missed mouse-up can
        // leave GPUI's active payload in place even when left is no longer held.
        let drag = event.drag(cx);
        if drag.owner != cx.entity_id() {
            return;
        }
        let scale = f32::from(window.rem_size()) / 16.;
        let x = event.event.position.x;
        let (distance, open, width, revision) = match drag.side {
            // The icon rail keeps its fixed width.
            Side::Tabs if self.sidebar_compact => return,
            Side::Tabs => (
                x - event.bounds.left(),
                self.tab_layout == TabLayout::Vertical,
                &mut self.tabs_sidebar_width,
                self.tabs_sidebar_motion.revision,
            ),
            Side::Files if self.sidebar.is_some() => (
                event.bounds.right() - x,
                self.sidebar_visible,
                &mut self.files_sidebar_width,
                self.files_sidebar_motion.revision,
            ),
            Side::Files => return,
        };
        // Manual toggles take precedence over any in-flight gesture.
        if drag.revision.get() != revision {
            return;
        }
        // The row's horizontal padding is 6 px at 100% interface scale.
        let pointer = f32::from(distance) / scale - 6.;
        let window_width = f32::from(window.viewport_size().width) / scale;
        let side = drag.side;
        let action = drag.gesture.borrow_mut().update(
            pointer,
            window_width,
            open,
            event.event.pressed_button == Some(MouseButton::Left),
        );
        if matches!(action, Some(Collapse | Reopen)) {
            drag.revision.set(revision + 1);
        }
        match action {
            Some(Width(w)) if *width != w => {
                *width = w;
                self.schedule_save(cx);
                cx.notify();
            }
            Some(Collapse | Reopen) => match side {
                Side::Tabs => self.set_tab_layout(
                    if open {
                        TabLayout::Horizontal
                    } else {
                        TabLayout::Vertical
                    },
                    cx,
                ),
                Side::Files => self.toggle_sidebar(cx),
            },
            _ => {}
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::shared::diagnostics::render("Workspace");
        // Follow keyboard focus into panes (a terminal focused by its own click).
        if let Some(tab) = self.tab()
            && let Some(inside) = tab.leaves().into_iter().find(|id| {
                self.panes
                    .get(id)
                    .is_some_and(|p| p.item.focus.contains_focused(window, cx))
            })
            && Some(inside) != tab.focused()
            && let Some(tab) = self.tab_mut()
        {
            tab.state.set_focused(inside);
        }
        let has_files_sidebar = self.sidebar.is_some();
        let vertical_tabs = self.tab_layout == TabLayout::Vertical;
        let window_px_at_100 =
            f32::from(window.viewport_size().width) / (f32::from(window.rem_size()) / 16.);
        let now = std::time::Instant::now();
        let tabs_moving = self.tabs_sidebar_motion.advance(now, cx.reduce_motion());
        let files_moving = self.files_sidebar_motion.advance(now, cx.reduce_motion());
        if tabs_moving || (files_moving && has_files_sidebar) {
            ui::request_animation_frame(window);
        }
        let (tabs_width, tabs_fraction) = self.sidebar_geometry(Side::Tabs, window_px_at_100, now);
        let (files_width, files_fraction) =
            self.sidebar_geometry(Side::Files, window_px_at_100, now);
        let files_sidebar = self.sidebar.clone().filter(|_| files_fraction > 0.);
        // Native web views would cover a screen or a dialog opened over
        // them, and swallow the pointer while a pane is dragged over them;
        // device cards stop capturing while covered.
        let covered = self.overlay.is_some()
            || window.has_active_dialog(cx)
            || window.has_active_sheet(cx)
            || cx.has_active_drag()
            || crate::features::browser::popovers_open(cx);
        self.set_stages_covered(covered, cx);
        self.sync_web_views(covered, cx);
        let (devices_docked, devices_floating) = {
            let panel = self.devices.read(cx);
            let docked = panel.mode() == crate::shared::floating::Mode::Docked;
            (panel.visible() && docked, panel.visible() && !docked)
        };
        // Browser cards: floating over the window, or docked in a column at
        // its right, whose divider may be being dragged.
        let floating_stages: Vec<AnyElement> = self
            .stages
            .iter()
            .filter_map(|stage| stage.element(cx))
            .collect();
        let browser_dock = self.render_browser_dock(cx);
        let browser_dock_drag = self.browser_dock_drag_layer(cx);
        // While the surge plays, the window shakes before the rose hatches:
        // the root moves in place and asks for the next frame, so the element
        // tree (and every element's state) stays the same.
        let (shake_x, shake_y) = match self.surge {
            Some((_, started, level)) => {
                ui::request_animation_frame(window);
                crate::shared::surge::shake_at(started.elapsed(), level)
            }
            None => (0., 0.),
        };
        v_flex()
            .size_full()
            .text_color(ui::fg())
            // A click in a web page (a device, the browser card) takes the
            // keyboard away from GPUI; any click that reaches GPUI takes it back.
            .capture_any_mouse_down(|_, window, _| {
                crate::features::browser::reclaim_keyboard(window)
            })
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_show_in_pane))
            .on_action(cx.listener(Self::on_close_viewer))
            .on_action(cx.listener(Self::on_open_commit_diff))
            .on_action(cx.listener(Self::on_ask_agent))
            .on_action(cx.listener(Self::on_open_terminal_at))
            .on_action(cx.listener(Self::on_new_terminal_tab))
            .on_action(cx.listener(Self::on_new_canvas_tab))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_split_right))
            .on_action(cx.listener(Self::on_tile_pane))
            .on_action(cx.listener(Self::on_split_down))
            .on_action(cx.listener(Self::on_new_agent_pane))
            .on_action(cx.listener(Self::on_new_terminal_pane))
            .on_action(cx.listener(Self::on_close_pane))
            .on_action(cx.listener(Self::on_reopen_closed))
            .on_action(cx.listener(Self::on_focus_next_pane))
            .on_action(cx.listener(Self::on_focus_prev_pane))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_open_providers))
            .on_action(cx.listener(Self::on_open_sessions))
            .on_action(cx.listener(Self::on_open_palette))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_open_workspaces))
            .on_action(cx.listener(Self::on_open_explorer))
            .on_action(cx.listener(Self::on_close_overlay))
            .key_context("GentleWorkspace")
            .relative()
            .left(px(shake_x))
            .top(px(shake_y))
            // The theme's backdrop, first so everything draws over it.
            .children(crate::shared::backdrop::layer(window, cx))
            .child(
                TitleBar::new()
                    .h(rems(TITLE_BAR_HEIGHT / 16.))
                    .bg(gpui_kit::transparent_black())
                    .child(
                        h_flex()
                            // Fill gpui-component's bar without counting toward
                            // its width: `w_full` there resolves against the
                            // content and pushed the buttons out of the window.
                            .flex_1()
                            .w_0()
                            .min_w_0()
                            .pr_2()
                            .gap_3()
                            .justify_between()
                            .child(
                                h_flex()
                                    .id("title-bar-chip-strip")
                                    .gap_2()
                                    .flex_1()
                                    // Width 0: the tabs take what the buttons leave.
                                    .w_0()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                            cx.stop_propagation();
                                            this.open_empty_area_menu(event.position, window, cx);
                                        }),
                                    )
                                    // With vertical tabs the sidebar header shows
                                    // the workspace and switches it, so the chip
                                    // would only repeat it.
                                    .when(!vertical_tabs, |row| {
                                        row.child(self.render_workspace_chip(cx))
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .w(px(1.))
                                                    .h(rems(14. / 16.))
                                                    .bg(ui::border()),
                                            )
                                            .child(self.render_tabs(cx))
                                    }),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap_1()
                                    // Without this, Windows answers clicks on these
                                    // buttons as title-bar drags (HTCAPTION).
                                    .occlude()
                                    .on_mouse_down(MouseButton::Right, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    // Tern keeps pane layout in the title bar, so
                                    // panes carry no header: one menu acts on the
                                    // focused pane, each entry with its shortcut.
                                    .when(self.tab().is_some(), |d| {
                                        d.child(self.render_layout_menu(cx)).child(
                                            div()
                                                .w(px(1.))
                                                .h(rems(14. / 16.))
                                                .mx_1()
                                                .bg(ui::border()),
                                        )
                                    })
                                    .when_some(
                                        crate::features::app_update::badge(cx),
                                        |d, (label, tip)| {
                                            d.child(
                                                div()
                                                    .id("app-update-pill")
                                                    .cursor_pointer()
                                                    .hover(|s| s.opacity(0.8))
                                                    .tooltip(move |window, cx| {
                                                        Tooltip::new(tip.clone()).build(window, cx)
                                                    })
                                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                        cx.stop_propagation()
                                                    })
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        this.open_app_updates(window, cx)
                                                    }))
                                                    .child(ui::chip(label, ui::accent())),
                                            )
                                        },
                                    )
                                    .children(crate::features::music::title_button(&self.music, cx))
                                    .children(self.render_devices_button(cx))
                                    .children(self.render_tray(cx))
                                    .child({
                                        let weak = cx.entity().downgrade();
                                        div()
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation()
                                            })
                                            .child(notices::bell(
                                                &self.notices,
                                                Rc::new(move |pane, window, cx| {
                                                    let _ = weak.update(cx, |this, cx| {
                                                        this.reveal_pane(pane, window, cx)
                                                    });
                                                }),
                                                cx,
                                            ))
                                    })
                                    .child(
                                        Button::new("toggle-tab-sidebar")
                                            .ghost()
                                            .xsmall()
                                            .icon(Icon::new(
                                                if vertical_tabs && !self.sidebar_compact {
                                                    IconName::PanelLeftClose
                                                } else {
                                                    IconName::PanelLeftOpen
                                                },
                                            ))
                                            .tooltip(if vertical_tabs && !self.sidebar_compact {
                                                "Collapse workspace sidebar"
                                            } else {
                                                "Expand workspace sidebar"
                                            })
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.toggle_sidebar_compact(cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("open-palette")
                                            .ghost()
                                            .xsmall()
                                            .icon(Icon::new(IconName::Command))
                                            .tooltip(format!("Commands ({})", ui::shortcut("K")))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.on_open_palette(&OpenPalette, window, cx)
                                            })),
                                    )
                                    // The screens behind one menu; it's lit while
                                    // one of them is open.
                                    .child(self.render_screens_menu(cx))
                                    .when(has_files_sidebar, |d| {
                                        d.child(
                                            Button::new("toggle-sidebar")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(if self.sidebar_visible {
                                                    IconName::PanelRightClose
                                                } else {
                                                    IconName::PanelRightOpen
                                                }))
                                                .tooltip(format!(
                                                    "{} Files sidebar ({})",
                                                    if self.sidebar_visible {
                                                        "Hide"
                                                    } else {
                                                        "Show"
                                                    },
                                                    ui::shortcut("B")
                                                ))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.toggle_sidebar(cx)
                                                })),
                                        )
                                    }),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .px_1p5()
                    .pb_1p5()
                    .gap_1p5()
                    .on_drag_move(cx.listener(
                        |this, event: &DragMoveEvent<SidebarDrag>, window, cx| {
                            this.resize_sidebar(event, window, cx);
                        },
                    ))
                    // The vertical tab navigator remains independent of the Files panel.
                    .when(tabs_fraction > 0., |row| {
                        row.child(
                            div()
                                .id("vertical-tab-panel")
                                .w(rems(tabs_width * tabs_fraction / 16.))
                                .mr(rems(-6. * (1. - tabs_fraction) / 16.))
                                .flex_none()
                                .h_full()
                                .relative()
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.open_empty_area_menu(event.position, window, cx);
                                    }),
                                )
                                .child(sidebar_clip(
                                    Side::Tabs,
                                    tabs_width,
                                    self.render_tab_sidebar(cx),
                                ))
                                .when(
                                    vertical_tabs && !self.sidebar_compact && !tabs_moving,
                                    |column| column.child(self.sidebar_handle(Side::Tabs, cx)),
                                ),
                        )
                    })
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_body(window, cx))
                            .child(self.measure_conversation()),
                    )
                    .children(browser_dock)
                    .when(devices_docked, |row| row.child(self.devices.clone()))
                    // Ctrl/Cmd+B toggles only the right Files/Changes/Branches sidebar.
                    .when_some(files_sidebar, |row, view| {
                        row.child(
                            div()
                                .id("files-sidebar-panel")
                                .w(rems(files_width * files_fraction / 16.))
                                .ml(rems(-6. * (1. - files_fraction) / 16.))
                                .flex_none()
                                .h_full()
                                .relative()
                                .child(sidebar_clip(
                                    Side::Files,
                                    files_width,
                                    view.cached(StyleRefinement::default().size_full()),
                                ))
                                .when(self.sidebar_visible && !files_moving, |column| {
                                    column.child(self.sidebar_handle(Side::Files, cx))
                                }),
                        )
                    }),
            )
            .children(floating_stages)
            .children(self.render_badge_picker(window, cx))
            .children(self.render_context_menu(cx))
            .when(devices_floating, |root| root.child(self.devices.clone()))
            .children(browser_dock_drag)
            .children(self.overlay.as_ref().map(|(overlay, _)| {
                let (view, full_window) = match overlay {
                    Overlay::Providers(view) => (view.clone().into_any_element(), false),
                    Overlay::Sessions(view) => (view.clone().into_any_element(), false),
                    Overlay::Palette(view) => (view.clone().into_any_element(), false),
                    Overlay::Explorer(view) => (view.clone().into_any_element(), false),
                    Overlay::Usage(view) => (view.clone().into_any_element(), false),
                    Overlay::Memory(view) => (view.clone().into_any_element(), false),
                    Overlay::Docs(view) => (view.clone().into_any_element(), false),
                    Overlay::Settings(view) => (view.clone().into_any_element(), false),
                    Overlay::Profiles(view) => (view.clone().into_any_element(), false),
                    Overlay::Review(view) => (view.clone().into_any_element(), false),
                    // First-run screens still cover the whole window.
                    Overlay::Onboarding(view) => (view.clone().into_any_element(), true),
                    Overlay::Welcome(view) => (view.clone().into_any_element(), true),
                };
                if full_window {
                    return view;
                }
                // Below the title bar, so its buttons can switch screens.
                div()
                    .absolute()
                    .top(rems(TITLE_BAR_HEIGHT / 16.))
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .child(view)
                    .into_any_element()
            }))
            .when_some(self.surge, |root, (run, _, level)| {
                root.child(crate::shared::surge::overlay(run, level))
            })
            .when_some(crate::features::hydration::playing(cx), |root, run| {
                root.child(crate::features::hydration::overlay(run, cx))
            })
    }
}

impl Workspace {
    /// The title bar's device panel button, while "Use devices" is on.
    fn render_devices_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !crate::features::devices::hub(cx).read(cx).prefs().enabled {
            return None;
        }
        let open = self.devices.read(cx).visible();
        Some(
            div()
                // Clicks here are not title-bar drags.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Button::new("title-devices")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::Smartphone))
                        .when(open, |b| b.text_color(ui::petal()))
                        .tooltip("Devices: simulators and emulators")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.devices.update(cx, |panel, cx| panel.toggle(cx))
                        })),
                )
                .into_any_element(),
        )
    }
}
