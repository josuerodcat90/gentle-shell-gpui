//! Closing panes safely: an agent that is working or waiting on you asks
//! first, and every pane or tab closed can be reopened where it was (most
//! recent first). Agents come back with their conversation, terminals in
//! their folder, files at their path.

use super::*;

/// How many closes can be undone.
pub(super) const KEEP: usize = 20;

/// A pane in a closed tab's layout: one still open, or one that closed.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Slot {
    Open(PaneId),
    Closed(SavedPane),
}

/// One close, to undo.
pub(super) struct Closed {
    /// The workspace it happened in, by index.
    pub(super) space: usize,
    /// The tab, by id, and where it sat among the tabs.
    tab: usize,
    pub(super) index: usize,
    /// Pin state to restore when an entire tab is reopened.
    pinned: bool,
    /// The tab's layout just before the close.
    layout: PaneTree<Slot>,
    /// What the notice names.
    title: SharedString,
}

impl Closed {
    /// A tab closed without ever being built: all its panes come back from
    /// what was saved.
    pub(super) fn unbuilt(
        space: usize,
        tab: usize,
        index: usize,
        pinned: bool,
        saved: SavedTab,
        title: SharedString,
    ) -> Option<Self> {
        let layout = saved
            .layout
            .filter_map(&mut |pane: &SavedPane| Some(Slot::Closed(pane.clone())))?;
        Some(Self {
            space,
            tab,
            index,
            pinned,
            layout,
            title,
        })
    }
}

impl Workspace {
    /// Remember that `closing` (all of tab `index`'s panes, or some) are
    /// about to close. Panes that can't be described again aren't kept.
    pub(super) fn remember_close(
        &mut self,
        space: usize,
        index: usize,
        closing: &[PaneId],
        cx: &App,
    ) {
        let Some(tab) = self.spaces.get(space).and_then(|s| s.tabs.get(index)) else {
            return;
        };
        let Some(tree) = tab.built_tree() else {
            return;
        };
        let layout = tree.filter_map(&mut |id: &PaneId| {
            if closing.contains(id) {
                self.saved_pane(*id, cx).map(Slot::Closed)
            } else {
                Some(Slot::Open(*id))
            }
        });
        let Some(layout) = layout else { return };
        if !layout.leaves().iter().any(|s| matches!(s, Slot::Closed(_))) {
            return;
        }
        let title = match closing {
            [one] => self.pane_title(*one),
            _ => SharedString::from(format!("{} panes", closing.len())),
        };
        let closed = Closed {
            space,
            tab: tab.id,
            index,
            pinned: tab.pinned,
            layout,
            title,
        };
        if self.closed.len() == KEEP {
            self.closed.remove(0);
        }
        self.closed.push(closed);
    }

    /// The window just closed something: say so, with a way back.
    pub(super) fn announce_close(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(closed) = self.closed.last() else {
            return;
        };
        let weak = cx.entity().downgrade();
        let note = Notification::info(format!(
            "Closed \u{201c}{}\u{201d}. Click to reopen ({}).",
            closed.title.trim_end_matches(" •"),
            REOPEN_KEY
        ))
        .id::<Closed>()
        .on_click(move |_, window, cx| {
            // The click runs inside the notice's own update; reopening
            // removes that notice, which reads it, so wait for the update to
            // end (reading it now panics and aborts the app).
            let weak = weak.clone();
            window.defer(cx, move |window, cx| {
                let _ = weak.update(cx, |this, cx| this.reopen_closed(window, cx));
            });
        });
        window.push_notification(note, cx);
    }

    /// Bring back the last thing closed, where it was.
    pub(super) fn reopen_closed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(closed) = self.closed.pop() else {
            return;
        };
        // Its notice (if still up) would now point at the close before it.
        window.remove_notification::<Closed>(cx);
        let space = closed.space;
        let Some(target) = self.spaces.get(space) else {
            return;
        };
        let existing = target.tabs.iter().position(|tab| tab.id == closed.tab);
        // Only a built tab can have a close record for part of its layout.
        if existing.is_some_and(|index| !target.tabs[index].is_built()) {
            return;
        }

        // Every successful restore is kept in the layout or attached beside
        // the surviving tab's focus. If none succeeds, there is nothing to show.
        let mut reopened = Vec::new();
        let tree = closed.layout.filter_map(&mut |slot| match slot {
            Slot::Open(id) => self.panes.contains_key(id).then_some(*id),
            Slot::Closed(saved) => {
                let id = self.restore_pane_in_space(space, saved, window, cx)?;
                reopened.push(id);
                Some(id)
            }
        });
        let (Some(tree), Some(&focused)) = (tree, reopened.first()) else {
            return;
        };

        let index = match existing {
            Some(index) => {
                let still_open = closed.layout.filter_map(&mut |slot| match slot {
                    Slot::Open(id) => Some(*id),
                    Slot::Closed(_) => None,
                });
                let tab = &mut self.spaces[space].tabs[index];
                if let TabState::Built {
                    tree: live,
                    focused,
                } = &mut tab.state
                {
                    if still_open.as_ref() == Some(live) {
                        // The tab is as the close left it: rebuild its layout exactly.
                        *live = tree;
                    } else {
                        // The tab changed since: the panes come back beside its focus.
                        let beside = if live.contains(focused) {
                            *focused
                        } else {
                            live.leaves()[0]
                        };
                        for id in reopened {
                            live.split(&beside, id, SplitAxis::Horizontal);
                        }
                    }
                }
                index
            }
            // The whole tab closed: it comes back where it sat.
            None => {
                let id = self.alloc_id();
                let tabs = &mut self.spaces[space].tabs;
                let boundary = tabs.iter().take_while(|tab| tab.pinned).count();
                let index = if closed.pinned {
                    closed.index.min(boundary)
                } else {
                    closed.index.max(boundary).min(tabs.len())
                };
                tabs.insert(
                    index,
                    Tab {
                        id,
                        pinned: closed.pinned,
                        state: TabState::Built { tree, focused },
                    },
                );
                index
            }
        };
        self.spaces[space].tabs[index].state.set_focused(focused);
        if space != self.current {
            // Select the restored tab before switching so ordinary switching
            // neither builds an unrelated tab nor fills an empty workspace.
            self.spaces[space].active = index;
            self.switch_workspace(space, window, cx);
        } else {
            self.activate(index, window, cx);
        }
    }

    /// Workspace `index` is gone: its closes can't come back, and the ones
    /// after it moved down.
    pub(super) fn forget_closed_in(&mut self, index: usize) {
        self.closed.retain(|c| c.space != index);
        for closed in &mut self.closed {
            if closed.space > index {
                closed.space -= 1;
            }
        }
    }
}

/// The shortcut that reopens, as the notice and the palette show it.
pub(super) const REOPEN_KEY: &str = ui::platform_shortcut("⌥⌘T", "Ctrl+Alt+Shift+T");

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc};

    use gpui_kit::component::input::InputState;
    use gpui_kit::{App, AppContext as _, Task, TestAppContext, Window, WindowOptions};

    use super::{
        Closed, DevicePanel, Invisible, Music, Notices, PaneFactories, PaneItem, PaneKind,
        PaneTree, SavedPane, SavedTab, Space, Tab, TabLayout, TabState, Workspace,
    };

    fn exercise_reopen(cx: &mut TestAppContext, missing_file: bool) {
        cx.update(gpui_kit::init);
        let root = std::env::temp_dir().join(format!(
            "gentle-reopen-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let builds = Rc::new(RefCell::new(Vec::new()));
        let recorded = builds.clone();
        let terminal = move |cwd, _window: &mut Window, cx: &mut App| {
            recorded.borrow_mut().push(cwd);
            PaneItem {
                kind: PaneKind::Terminal,
                view: cx.new(|_| Invisible).into(),
                focus: cx.focus_handle(),
                title: Rc::new(|_| "Terminal".into()),
                unsaved: None,
            }
        };
        let saved = |kind, cwd, path| SavedTab {
            layout: PaneTree::Leaf(SavedPane {
                kind,
                cwd,
                path,
                session: None,
            }),
            pinned: false,
            focused: 0,
        };
        let (handle, _) = cx
            .update(|cx| {
                cx.set_global(PaneFactories {
                    terminal: Some(Rc::new(terminal)),
                    ..Default::default()
                });
                gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Invisible))
            })
            .unwrap();
        cx.update(|cx| {
            handle.update(cx, |_, window, cx| {
                let mut source = Space::new("Source".into(), root.join("source"));
                source.tabs.push(Tab {
                    id: 1,
                    pinned: false,
                    state: TabState::Built {
                        tree: PaneTree::Leaf(1),
                        focused: 1,
                    },
                });
                let mut target = Space::new("Target".into(), root.clone());
                if !missing_file {
                    target.tabs.push(Tab {
                        id: 2,
                        pinned: false,
                        state: TabState::Unbuilt(saved(PaneKind::Terminal, root.clone(), None)),
                    });
                }
                let description = if missing_file {
                    saved(PaneKind::File, root.clone(), Some(root.join("deleted.txt")))
                } else {
                    let mut tab = saved(PaneKind::Terminal, root.join("deleted-cwd"), None);
                    tab.layout = PaneTree::Split {
                        axis: super::SplitAxis::Horizontal,
                        ratio: 0.5,
                        first: Box::new(tab.layout),
                        second: Box::new(
                            saved(PaneKind::File, root.clone(), Some(root.join("deleted.txt")))
                                .layout,
                        ),
                    };
                    tab
                };
                let closed = Closed::unbuilt(1, 3, 1, false, description, "Closed".into()).unwrap();
                // Drive the real reopen operation without startup, persistence or a real PTY.
                let workspace = cx.new(|cx| Workspace {
                    spaces: vec![source, target],
                    current: 0,
                    panes: HashMap::new(),
                    next_id: 3,
                    overlay: None,
                    web_shown: Default::default(),
                    sidebar: None,
                    sidebar_visible: false,
                    tab_layout: TabLayout::Horizontal,
                    tabs_sidebar_motion: Default::default(),
                    files_sidebar_motion: Default::default(),
                    sidebar_compact: false,
                    tabs_sidebar_width: gentle_core::workspaces::SIDEBAR_DEFAULT_WIDTH,
                    files_sidebar_width: gentle_core::workspaces::SIDEBAR_DEFAULT_WIDTH,
                    badge_picker: None,
                    open_menu: None,
                    workspace_rename: None,
                    tab_selection: None,
                    tab_search: cx.new(|cx| InputState::new(window, cx)),
                    save_task: Some(Task::ready(())),
                    _title_poll: Task::ready(()),
                    _lifecycle: Vec::new(),
                    notices: cx.new(|_| Notices::default()),
                    renaming: None,
                    surge: None,
                    surge_runs: 0,
                    _surge_end: None,
                    stages: Vec::new(),
                    next_stage: 1,
                    opened_outside: Default::default(),
                    browser_dock_drag: None,
                    browser_dock_room: Default::default(),
                    resume_welcome: None,
                    drop_hint: None,
                    closed: vec![closed],
                    music: cx.new(|cx| Music::new(window, cx)),
                    devices: cx.new(DevicePanel::new),
                });
                workspace.update(cx, |workspace, cx| workspace.reopen_closed(window, cx));
                let workspace = workspace.read(cx);
                assert_eq!(workspace.spaces[0].active, 0);
                if missing_file {
                    assert_eq!(workspace.current, 0);
                    assert_eq!(workspace.tab().unwrap().id, 1);
                    assert!(workspace.spaces[1].tabs.is_empty());
                    assert!(workspace.panes.is_empty());
                    assert!(builds.borrow().is_empty());
                } else {
                    assert_eq!(workspace.current, 1);
                    assert_eq!(workspace.space().active, 1);
                    assert!(!workspace.space().tabs[0].is_built());
                    assert_eq!(workspace.panes.len(), 1);
                    assert_eq!(*builds.borrow(), [root]);
                }
            })
        })
        .unwrap();
    }

    #[gpui::test]
    fn cross_workspace_reopen_builds_only_the_requested_tab(cx: &mut TestAppContext) {
        exercise_reopen(cx, false);
    }

    #[gpui::test]
    fn cross_workspace_reopen_of_deleted_file_leaves_current_tab_shown(cx: &mut TestAppContext) {
        exercise_reopen(cx, true);
    }
}
