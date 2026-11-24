//! Right-click context menus and inline workspace rename owned by the
//! workspace, so their open state can suppress tooltips and hide native web
//! views under them.

use super::tab_panel::{MultiTabMenu, TabMenu};
use super::*;

/// A context menu built and positioned by the workspace. Kept here instead
/// of gpui-kit's `ContextMenu` so the workspace knows when any menu is open.
pub(super) struct OpenMenu {
    key: &'static str,
    menu: Entity<PopupMenu>,
    position: Point<Pixels>,
    focus_handle: FocusHandle,
    previous_focus: Option<FocusHandle>,
    _subscription: Subscription,
}

/// Where a workspace menu was opened from: the inline rename field draws in
/// that same place.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkspaceMenuSource {
    Chip,
    Header,
    SidebarRow,
}

/// The inline rename field of a workspace being renamed.
pub(super) struct WorkspaceRename {
    pub index: usize,
    pub source: WorkspaceMenuSource,
    pub input: Entity<InputState>,
    pub _subscription: Subscription,
}

impl Workspace {
    /// Whether any workspace-owned popup (context menu or badge picker) is
    /// open. Tooltips on workspaces and tabs are gated on this being false.
    pub(super) fn popups_open(&self) -> bool {
        self.open_menu.is_some() || self.badge_picker.is_some()
    }

    pub(super) fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.open_menu.take() {
            if Some(menu.focus_handle.clone()) == window.focused(cx) {
                if let Some(prev) = menu.previous_focus {
                    prev.focus(window, cx);
                } else {
                    self.focus_active(window, cx);
                }
            }
            crate::features::browser::popover_open(menu.key, false, window, cx);
            cx.notify();
        }
    }

    /// Close every owned popup (menu or badge picker).
    pub(super) fn close_popups(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        self.close_badge_picker(window, cx);
    }

    fn open_owned_menu<F>(
        &mut self,
        key: &'static str,
        position: Point<Pixels>,
        build: F,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) where
        F: FnOnce(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    {
        self.close_popups(window, cx);
        let previous_focus = window.focused(cx);
        let menu = PopupMenu::build(window, cx, build);
        menu.focus_handle(cx).focus(window, cx);
        let menu_for_dismiss = menu.clone();
        let subscription = cx.subscribe_in(
            &menu,
            window,
            move |this, _, _: &DismissEvent, window, cx| {
                if this
                    .open_menu
                    .as_ref()
                    .is_some_and(|m| m.menu == menu_for_dismiss)
                {
                    this.close_menu(window, cx);
                }
            },
        );
        crate::features::browser::popover_open(key, true, window, cx);
        let focus_handle = menu.focus_handle(cx);
        self.open_menu = Some(OpenMenu {
            key,
            menu,
            position,
            focus_handle,
            previous_focus,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// The plus in a workspace row opens a two-entry menu independent of
    /// the row's selection and drag handlers.
    pub(super) fn open_workspace_add_menu(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(space) = self.spaces.get(index) else {
            return;
        };
        let name = space.name.clone();
        let weak = cx.entity().downgrade();
        self.open_owned_menu(
            "workspace-add-menu",
            position,
            move |menu, window, _| {
                let mut menu = menu.min_w(rems(160. / 16.).to_pixels(window.rem_size()));
                for (kind, label) in [
                    (PaneKind::Agent, "New agent"),
                    (PaneKind::Terminal, "New terminal"),
                ] {
                    let weak = weak.clone();
                    let name = name.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .icon(Icon::new(kind.icon()))
                            .on_click(move |_, window, cx| {
                                let name = name.clone();
                                let _ = weak.update(cx, |this, cx| {
                                    this.close_menu(window, cx);
                                    // Leave the popup's event dispatch before
                                    // constructing the session and its backend.
                                    cx.spawn_in(window, async move |this, cx| {
                                        let _ = this.update_in(cx, |this, window, cx| {
                                            if let Some(index) = this
                                                .spaces
                                                .iter()
                                                .position(|space| space.name == name)
                                            {
                                                this.open_workspace_tab(index, kind, window, cx);
                                            }
                                        });
                                    })
                                    .detach();
                                });
                            }),
                    );
                }
                menu
            },
            window,
            cx,
        );
    }

    /// Right-click on a workspace chip, header or row opens this menu.
    pub(super) fn open_workspace_menu(
        &mut self,
        index: usize,
        source: WorkspaceMenuSource,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let can_delete = self.spaces.len() >= 2;
        let weak = cx.entity().downgrade();
        self.open_owned_menu(
            "workspace-menu",
            position,
            move |menu, window, cx| {
                Self::build_workspace_menu(menu, weak, index, source, can_delete, window, cx)
            },
            window,
            cx,
        );
    }

    fn build_workspace_menu(
        menu: PopupMenu,
        weak: WeakEntity<Self>,
        index: usize,
        source: WorkspaceMenuSource,
        can_delete: bool,
        window: &Window,
        _cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let rename = weak.clone();
        let badge = weak.clone();
        let agent = weak.clone();
        let terminal = weak.clone();
        let canvas = weak.clone();
        let tower = weak.clone();
        let new_space = weak.clone();
        let delete = weak;
        menu.min_w(rems(220. / 16.).to_pixels(window.rem_size()))
            .item(
                PopupMenuItem::new("Rename")
                    .icon(Icon::new(IconName::Pencil))
                    .on_click(move |_, window, cx| {
                        let _ = rename.update(cx, |this, cx| {
                            this.start_workspace_rename(index, source, window, cx)
                        });
                    }),
            )
            .item(
                PopupMenuItem::new("Badge…")
                    .icon(Icon::new(IconName::Palette))
                    .on_click(move |_, window, cx| {
                        let _ = badge.update(cx, |this, cx| {
                            // Open the picker where the menu was.
                            if let Some(position) = this.open_menu.as_ref().map(|m| m.position) {
                                this.open_badge_picker(index, position, window, cx)
                            }
                        });
                    }),
            )
            .separator()
            .item(
                PopupMenuItem::new("New agent tab")
                    .icon(Icon::new(IconName::Bot))
                    .on_click(move |_, window, cx| {
                        let _ = agent.update(cx, |this, cx| {
                            this.switch_workspace(index, window, cx);
                            let root = this.space().root.clone();
                            this.open_tab(PaneKind::Agent, root, None, None, window, cx);
                        });
                    }),
            )
            .item(
                PopupMenuItem::new("New terminal tab")
                    .icon(Icon::new(IconName::SquareTerminal))
                    .on_click(move |_, window, cx| {
                        let _ = terminal.update(cx, |this, cx| {
                            this.switch_workspace(index, window, cx);
                            let root = this.space().root.clone();
                            this.open_tab(PaneKind::Terminal, root, None, None, window, cx);
                        });
                    }),
            )
            .item(
                PopupMenuItem::new("New canvas tab")
                    .icon(Icon::new(IconName::PenTool))
                    .on_click(move |_, window, cx| {
                        let _ = canvas.update(cx, |this, cx| {
                            this.switch_workspace(index, window, cx);
                            let root = this.space().root.clone();
                            this.open_tab(PaneKind::Canvas, root, None, None, window, cx);
                        });
                    }),
            )
            .item(
                PopupMenuItem::new("New tower tab")
                    .icon(Icon::new(IconName::Building2))
                    .on_click(move |_, window, cx| {
                        let _ = tower.update(cx, |this, cx| {
                            // Gentle Tower is one tab per window: an open one
                            // is revealed wherever it is; only a new one opens
                            // in this workspace.
                            let open = this.panes.values().any(|p| p.item.kind == PaneKind::Tower);
                            if !open {
                                this.switch_workspace(index, window, cx);
                            }
                            this.open_tower(window, cx);
                        });
                    }),
            )
            .separator()
            .item(
                PopupMenuItem::new("New workspace")
                    .icon(Icon::new(IconName::FolderPlus))
                    .on_click(move |_, window, cx| {
                        let _ = new_space
                            .update(cx, |this, cx| this.pick_workspace_folder(None, window, cx));
                    }),
            )
            .separator()
            .item(
                PopupMenuItem::new("Delete")
                    .icon(Icon::new(IconName::Trash))
                    .disabled(!can_delete)
                    .on_click(move |_, window, cx| {
                        let _ = delete.update(cx, |this, cx| {
                            this.confirm_delete_workspace(index, window, cx)
                        });
                    }),
            )
    }

    /// Right-click on a tab: on one of a 2+ selection, Pin/Unpin and Close
    /// for the whole selection; otherwise the tab's own menu (vertical tabs
    /// keep Move up/down, horizontal ones are compact).
    pub(super) fn open_tab_menu(
        &mut self,
        tab: TabMenu,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.entity().downgrade();
        let ids = self.selected_tab_ids(tab.space);
        if ids.len() >= 2 && ids.contains(&tab.id) {
            let selection = MultiTabMenu {
                space: tab.space,
                any_unpinned: self.spaces[tab.space]
                    .tabs
                    .iter()
                    .any(|t| !t.pinned && ids.contains(&t.id)),
                count: ids.len(),
                ids,
            };
            self.open_owned_menu(
                "tab-menu",
                position,
                move |menu, _, _cx| Self::multi_tab_actions_menu(menu, weak, selection),
                window,
                cx,
            );
        } else {
            self.open_owned_menu(
                "tab-menu",
                position,
                move |menu, _, _cx| Self::tab_actions_menu(menu, weak, tab),
                window,
                cx,
            );
        }
    }

    /// Right-click on empty space of the tab area opens this menu.
    pub(super) fn open_empty_area_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.entity().downgrade();
        let layout = self.tab_layout;
        self.open_owned_menu(
            "workspace-empty-area",
            position,
            move |menu, _, _cx| Self::empty_area_menu(menu, weak, layout),
            window,
            cx,
        );
    }

    /// Open the inline name field for workspace `index` in the place the menu
    /// was opened.
    pub(super) fn start_workspace_rename(
        &mut self,
        index: usize,
        source: WorkspaceMenuSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self.spaces.get(index).map(|s| s.name.clone()) else {
            return;
        };
        self.finish_workspace_rename(false, window, cx);
        let len = name.len();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    this.finish_workspace_rename(true, window, cx)
                }
                _ => {}
            },
        );
        input.update(cx, |state, cx| {
            state.focus(window, cx);
            state.set_selected_range(0..len, cx);
        });
        self.workspace_rename = Some(WorkspaceRename {
            index,
            source,
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    pub(super) fn cancel_workspace_rename(
        &mut self,
        _: &Escape,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_workspace_rename(false, window, cx);
    }

    /// Close the workspace rename field; with `commit`, a non-empty name is
    /// applied via `rename_workspace` (which keeps it unique).
    pub(super) fn finish_workspace_rename(
        &mut self,
        commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rename) = self.workspace_rename.take() else {
            return;
        };
        let focused = rename.input.focus_handle(cx).is_focused(window);
        let name = rename.input.read(cx).value().trim().to_string();
        cx.notify();
        if focused {
            self.focus_active(window, cx);
        }
        if commit && !name.is_empty() {
            self.rename_workspace(rename.index, name, cx);
        }
    }

    /// Cancel the workspace rename without moving focus (used when the UI
    /// context that held the field is going away, e.g. layout toggle).
    pub(super) fn cancel_workspace_rename_state(&mut self, cx: &mut Context<Self>) {
        if self.workspace_rename.take().is_some() {
            cx.notify();
        }
    }

    /// Whether workspace `index` is being renamed from `source`.
    pub(super) fn renaming_workspace(&self, index: usize, source: WorkspaceMenuSource) -> bool {
        self.workspace_rename
            .as_ref()
            .is_some_and(|r| r.index == index && r.source == source)
    }

    /// Render the owned context menu, if any, deferred and anchored like the
    /// badge picker.
    pub(super) fn render_context_menu(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.open_menu.as_ref()?;
        Some(
            deferred(
                anchored()
                    .position(menu.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(menu.menu.clone()),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// The inline rename field shown in place of a workspace name.
    pub(super) fn workspace_rename_input(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(rename) = self.workspace_rename.as_ref() else {
            return div().into_any_element();
        };
        div()
            .flex_1()
            .min_w_0()
            .h(rems(20. / 16.))
            .px_1()
            .rounded_md()
            .bg(ui::well())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // The field's own Cut/Copy/Paste menu is the only one.
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_action(cx.listener(Self::cancel_workspace_rename))
            .child(Input::new(&rename.input).appearance(false).xsmall())
            .into_any_element()
    }
}
