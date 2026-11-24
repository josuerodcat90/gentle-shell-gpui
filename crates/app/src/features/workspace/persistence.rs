//! Session lookup, layout snapshots and debounced saving.

use super::*;

impl Workspace {
    pub(super) fn active_cwd(&self, cx: &App) -> PathBuf {
        self.focused_pane()
            .and_then(|id| self.panes.get(&id))
            .map(|pane| self.pane_cwd(pane, cx))
            .unwrap_or_else(|| self.space().root.clone())
    }

    /// The agent the screens act on: the focused pane's, else the first agent
    /// in the tab, else any in the workspace.
    pub(super) fn active_session(&self) -> Option<Entity<SessionView>> {
        let session = |id: &PaneId| self.panes.get(id).and_then(|p| p.session.clone());
        let tab = self.tab()?;
        tab.focused()
            .and_then(|id| session(&id))
            .or_else(|| tab.leaves().iter().find_map(session))
            .or_else(|| {
                // Unbuilt tabs have no panes, so they are skipped.
                self.space()
                    .tabs
                    .iter()
                    .flat_map(|t| t.leaves())
                    .find_map(|id| session(&id))
            })
    }

    /// Build the current workspace's first unbuilt tab holding an agent when
    /// no built tab has one, so [`Self::active_session`] finds it.
    pub(super) fn ensure_agent_built(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_session().is_some() {
            return;
        }
        let unbuilt = self.space().tabs.iter().position(|tab| {
            tab.state.unbuilt().is_some_and(|saved| {
                saved
                    .layout
                    .leaves()
                    .iter()
                    .any(|pane| pane.kind == PaneKind::Agent)
            })
        });
        if let Some(index) = unbuilt {
            self.ensure_tab_built(self.current, index, window, cx);
        }
    }

    /// Build and start [`Self::active_session`]'s backend when a screen opens
    /// on it: it can sit in a tab not shown, or not built, yet (when the shown
    /// tab has no agent).
    pub(super) fn start_active_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_agent_built(window, cx);
        if let Some(session) = self.active_session() {
            session.update(cx, |s, cx| s.ensure_started(window, cx));
        }
    }

    pub(super) fn sessions(&self) -> Vec<Entity<SessionView>> {
        self.panes
            .values()
            .filter_map(|p| p.session.clone())
            .collect()
    }

    // ── Persistence ─────────────────────────────────────────────────────────

    pub(super) fn saved_pane(&self, id: PaneId, cx: &App) -> Option<SavedPane> {
        let pane = self.panes.get(&id)?;
        Some(SavedPane {
            kind: pane.item.kind,
            cwd: self.pane_cwd(pane, cx),
            path: pane.path.clone(),
            // A canvas keeps its floating agent's conversation.
            session: match &pane.session {
                Some(session) => session.read(cx).session_file(),
                None if pane.item.kind == PaneKind::Canvas => pane.session_file.clone(),
                None => None,
            },
        })
    }

    pub(super) fn snapshot(&self, cx: &App) -> SavedState {
        let workspaces = self
            .spaces
            .iter()
            .map(|space| {
                let tabs = space
                    .tabs
                    .iter()
                    .filter_map(|tab| saved_tab(tab, |id| self.saved_pane(id, cx)))
                    .collect();
                SavedWorkspace {
                    name: space.name.clone(),
                    root: space.root.clone(),
                    tabs,
                    active_tab: space.active,
                    last_active: space.last_active,
                    badge: space.badge,
                    sidebar_expanded: space.sidebar_expanded,
                }
            })
            .collect();
        SavedState {
            version: workspaces::VERSION,
            current: self.current,
            workspaces,
            sidebar: self.sidebar_visible,
            tab_layout: self.tab_layout,
            sidebar_compact: self.sidebar_compact,
            tabs_sidebar_width: self.tabs_sidebar_width,
            files_sidebar_width: self.files_sidebar_width,
        }
    }

    /// Re-renders when a pane's title or busy state changed.
    pub(super) fn refresh_titles(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        for pane in self.panes.values_mut() {
            changed |= pane.sync_chrome(cx);
        }
        if changed {
            cx.notify();
        }
    }

    pub(super) fn save_now(&mut self, cx: &App) {
        self.save_task = None;
        if let Err(err) = workspaces::save(&self.snapshot(cx)) {
            eprintln!("gentle-shell-gpui: could not save workspaces: {err}");
        }
    }

    /// Save soon, at most once per half second.
    pub(super) fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if self.save_task.is_some() {
            return;
        }
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |this, cx| this.save_now(cx));
        }));
    }

    // ── Actions ─────────────────────────────────────────────────────────────
}

/// One tab as saved. A tab that was never built is written back exactly as
/// it was restored; a built one is described from its live panes (`pane`),
/// dropping those that can't be described.
fn saved_tab(tab: &Tab, pane: impl Fn(PaneId) -> Option<SavedPane>) -> Option<SavedTab> {
    let (tree, focused) = match &tab.state {
        TabState::Unbuilt(saved) => {
            return Some(SavedTab {
                pinned: tab.pinned,
                ..saved.clone()
            });
        }
        TabState::Built { tree, focused } => (tree, *focused),
    };
    let kept: Vec<PaneId> = tree
        .leaves()
        .into_iter()
        .filter(|id| pane(*id).is_some())
        .collect();
    Some(SavedTab {
        pinned: tab.pinned,
        layout: tree.filter_map(&mut |id| pane(*id))?,
        focused: kept.iter().position(|id| *id == focused).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{PaneKind, PaneTree, SavedPane, SavedTab, Tab, TabState, saved_tab};
    use crate::shared::pane::SplitAxis;

    fn pane(kind: PaneKind, folder: &str) -> SavedPane {
        SavedPane {
            kind,
            cwd: PathBuf::from(folder),
            path: None,
            session: None,
        }
    }

    fn unbuilt(id: usize, pinned: bool) -> Tab {
        Tab {
            id,
            pinned,
            state: TabState::Unbuilt(SavedTab {
                layout: PaneTree::Split {
                    axis: SplitAxis::Horizontal,
                    ratio: 0.3,
                    first: Box::new(PaneTree::Leaf(pane(PaneKind::Agent, "/a"))),
                    second: Box::new(PaneTree::Leaf(pane(PaneKind::Terminal, "/b"))),
                },
                pinned,
                focused: 1,
            }),
        }
    }

    /// A tab never shown is written back as restored, even though no live
    /// pane can describe it.
    #[test]
    fn an_unbuilt_tab_is_saved_unchanged() {
        let tab = unbuilt(1, false);
        let expected = tab.state.unbuilt().cloned();
        assert_eq!(saved_tab(&tab, |_| None), expected);
    }

    /// Pinning an unbuilt tab (the only thing that can change it) is kept
    /// and nothing else moves.
    #[test]
    fn pinning_an_unbuilt_tab_changes_only_its_pin() {
        let mut tab = unbuilt(1, false);
        let before = tab.state.unbuilt().cloned().unwrap();
        tab.pinned = true;
        let saved = saved_tab(&tab, |_| None).unwrap();
        assert!(saved.pinned);
        assert_eq!(saved.layout, before.layout);
        assert_eq!(saved.focused, before.focused);
    }

    /// A built tab is described from its live panes, as before: panes that
    /// can't be described are dropped and focus follows the kept ones.
    #[test]
    fn a_built_tab_is_saved_from_its_panes() {
        let tab = Tab {
            id: 2,
            pinned: true,
            state: TabState::Built {
                tree: PaneTree::Split {
                    axis: SplitAxis::Vertical,
                    ratio: 0.5,
                    first: Box::new(PaneTree::Leaf(10)),
                    second: Box::new(PaneTree::Split {
                        axis: SplitAxis::Vertical,
                        ratio: 0.5,
                        first: Box::new(PaneTree::Leaf(11)),
                        second: Box::new(PaneTree::Leaf(12)),
                    }),
                },
                focused: 12,
            },
        };
        let saved = saved_tab(&tab, |id| match id {
            10 => Some(pane(PaneKind::Agent, "/a")),
            11 => None,
            _ => Some(pane(PaneKind::Terminal, "/c")),
        })
        .unwrap();
        assert!(saved.pinned);
        assert_eq!(saved.layout.leaves().len(), 2);
        assert_eq!(saved.focused, 1);
    }
}
