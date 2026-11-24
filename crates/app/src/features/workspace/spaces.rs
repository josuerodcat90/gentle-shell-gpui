//! Named workspaces: create, switch, rename, delete, branch switching.

use super::*;

impl Workspace {
    pub(super) fn space(&self) -> &Space {
        &self.spaces[self.current]
    }

    pub(super) fn space_mut(&mut self) -> &mut Space {
        &mut self.spaces[self.current]
    }

    pub(super) fn push_space(&mut self, name: Option<String>, root: PathBuf) -> usize {
        let base = name.unwrap_or_else(|| ui::split_path(&root).1);
        let name = workspaces::unique_name(self.spaces.iter().map(|s| s.name.as_str()), &base);
        self.spaces.push(Space::new(name, root));
        self.spaces.len() - 1
    }

    pub(super) fn switch_workspace(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= self.spaces.len() {
            return;
        }
        self.close_popups(window, cx);
        // Selection belongs to the shown workspace; it does not survive a switch.
        self.clear_tab_selection(cx);
        // A rename left by switching keeps its name, like a blur.
        self.finish_workspace_rename(true, window, cx);
        let changed = index != self.current;
        self.current = index;
        if !self.space().tabs.is_empty() {
            let active = self.space().active;
            self.activate(active, window, cx);
        }
        self.space_mut().last_active = workspaces::now();
        if changed {
            cx.emit(WorkspaceEvent::RootChanged(self.space().root.clone()));
        }
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn create_workspace(
        &mut self,
        name: Option<String>,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = self.push_space(name.filter(|n| !n.trim().is_empty()), root);
        self.switch_workspace(index, window, cx);
    }

    /// Open a new tab in a specific workspace, using its root rather than
    /// the working directory of a previously focused agent.
    pub(super) fn open_workspace_tab(
        &mut self,
        index: usize,
        kind: PaneKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(space) = self.spaces.get_mut(index) else {
            return;
        };
        space.sidebar_expanded = true;
        let root = space.root.clone();
        if index != self.current {
            self.switch_workspace(index, window, cx);
        }
        self.open_tab(kind, root, None, None, window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn pick_workspace_folder(
        &mut self,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Create workspace".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                this.create_workspace(name, path, window, cx)
            });
        })
        .detach();
    }

    /// Point the shown workspace at another folder: the sidebar re-roots on
    /// [`WorkspaceEvent::RootChanged`] and new tabs open there. Open tabs
    /// keep their own folders.
    pub(super) fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.space().root == root || !root.is_dir() {
            return;
        }
        self.space_mut().root = root.clone();
        cx.emit(WorkspaceEvent::RootChanged(root));
        self.schedule_save(cx);
        cx.notify();
    }

    /// The Files sidebar's search field: the palette's file finder.
    pub fn search_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette(Category::Files, window, cx);
    }

    /// The sidebar header: pick a folder, [`Self::set_root`] to it and move
    /// the focused agent there too, so the files shown are the ones it works
    /// on. An agent with a conversation asks first (it restarts fresh).
    pub fn choose_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            let _ = this.update_in(cx, |this, window, cx| this.move_root(path, window, cx));
        })
        .detach();
    }

    /// [`Self::choose_root`]'s pick: the sidebar and the focused agent follow
    /// `root`, after a confirmation when the agent would lose a conversation.
    fn move_root(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.focused_pane();
        // A canvas's agent floats in it instead of being the pane's.
        let canvas = focused.and_then(|id| self.canvas_view(id).map(|view| (id, view)));
        let agent = focused
            .and_then(|id| self.panes.get(&id))
            .and_then(|pane| pane.session.clone())
            .or_else(|| {
                canvas
                    .as_ref()
                    .and_then(|(_, view)| view.read(cx).agent_session())
            })
            .filter(|session| *session.read(cx).cwd() != root);
        if !agent
            .as_ref()
            .is_some_and(|agent| agent.read(cx).has_conversation())
        {
            self.commit_root(root, agent, canvas, window, cx);
            return;
        }
        let folder = ui::split_path(&root).1;
        let title = format!("Move this agent to \u{201c}{folder}\u{201d}?");
        let description = "Its conversation ends and a new session starts in that folder. \
             The previous conversation stays on disk.";
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, agent, canvas, root) =
                (this.clone(), agent.clone(), canvas.clone(), root.clone());
            alert
                .title(title.clone())
                .description(description)
                .ok_text("Move")
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let (agent, canvas, root) = (agent.clone(), canvas.clone(), root.clone());
                    let _ = this.update(cx, |this, cx| {
                        this.commit_root(root, agent, canvas, window, cx)
                    });
                    true
                })
        });
    }

    /// Move the sidebar, the focused `canvas` pane (where its agent starts)
    /// and `agent` to `root`.
    fn commit_root(
        &mut self,
        root: PathBuf,
        agent: Option<Entity<SessionView>>,
        canvas: Option<(PaneId, Entity<CanvasView>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((id, view)) = canvas {
            view.update(cx, |view, _| view.set_cwd(root.clone()));
            if let Some(pane) = self.panes.get_mut(&id) {
                pane.cwd = root.clone();
            }
            // Saved even when the sidebar already shows `root`.
            self.schedule_save(cx);
        }
        if let Some(agent) = agent {
            agent.update(cx, |agent, cx| agent.change_cwd(root.clone(), window, cx));
        }
        self.set_root(root, cx);
    }

    pub(super) fn rename_workspace(&mut self, index: usize, name: String, cx: &mut Context<Self>) {
        let others = self
            .spaces
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, s)| s.name.as_str());
        let name = workspaces::unique_name(others, &name);
        if let Some(space) = self.spaces.get_mut(index) {
            space.name = name;
        }
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn confirm_delete_workspace(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(space) = self.spaces.get(index) else {
            return;
        };
        if self.spaces.len() < 2 {
            return;
        }
        let title = format!("Delete workspace \u{201c}{}\u{201d}?", space.name);
        let tabs = space.tabs.len();
        let description = format!(
            "Its {tabs} {} close and any running agent in them stops. Files and conversations stay on disk.",
            if tabs == 1 { "tab" } else { "tabs" }
        );
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .title(title.clone())
                .description(description.clone())
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let _ = this.update(cx, |this, cx| this.delete_workspace(index, window, cx));
                    true
                })
        });
    }

    pub(super) fn delete_workspace(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= self.spaces.len() || self.spaces.len() < 2 {
            return;
        }
        self.close_popups(window, cx);
        self.finish_workspace_rename(false, window, cx);
        if index == self.current {
            let next = (0..self.spaces.len())
                .filter(|i| *i != index)
                .max_by_key(|i| self.spaces[*i].last_active)
                .unwrap_or(0);
            self.switch_workspace(next, window, cx);
        }
        let space = self.spaces.remove(index);
        self.forget_closed_in(index);
        for tab in space.tabs {
            for pane in tab.leaves() {
                // Dropping a session drops its PiProcess, which shuts the backend down.
                self.panes.remove(&pane);
            }
        }
        if self.current > index {
            self.current -= 1;
        }
        // Workspace-indexed state follows the shift, or goes with the workspace.
        self.tab_selection = self.tab_selection.take().and_then(|sel| {
            let moved =
                gentle_core::workspaces::adjust_workspace_index_after_delete(sel.space, index);
            super::tab_panel::remap_selection_space(Some(sel), moved)
        });
        self.schedule_save(cx);
        cx.notify();
    }

    /// Drag a workspace row in the vertical sidebar to reorder workspaces.
    /// The shown workspace is tracked by identity and every
    /// workspace-indexed state is remapped (dropped when it has no new
    /// index). Open menus and the badge picker close, since they hold
    /// workspace indexes.
    pub(super) fn move_workspace(
        &mut self,
        from: usize,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if from == to || from >= self.spaces.len() || to >= self.spaces.len() {
            return;
        }
        self.close_popups(window, cx);
        let order = gentle_core::workspaces::reorder_workspaces(self.spaces.len(), from, to);
        let remap = gentle_core::workspaces::workspace_index_remap(&order);

        let mut old: Vec<Option<Space>> = self.spaces.drain(..).map(Some).collect();
        self.spaces = order
            .iter()
            .filter_map(|old_index| old.get_mut(*old_index).and_then(Option::take))
            .collect();

        self.current = remap.get(self.current).copied().unwrap_or(0);
        self.workspace_rename = self.workspace_rename.take().and_then(|mut rename| {
            rename.index = *remap.get(rename.index)?;
            Some(rename)
        });
        self.closed
            .retain_mut(|closed| match remap.get(closed.space) {
                Some(new) => {
                    closed.space = *new;
                    true
                }
                None => false,
            });
        let selection_space = self
            .tab_selection
            .as_ref()
            .and_then(|sel| remap.get(sel.space).copied());
        self.tab_selection =
            super::tab_panel::remap_selection_space(self.tab_selection.take(), selection_space);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn confirm_switch_branch(
        &mut self,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.space().root.clone();
        let title = format!("Switch to \u{201c}{branch}\u{201d}?");
        let description = format!(
            "Runs git switch {branch} in {}. Uncommitted changes that conflict make git refuse.",
            ui::split_path(&root).1
        );
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, root, branch) = (this.clone(), root.clone(), branch.clone());
            alert
                .title(title.clone())
                .description(description.clone())
                .ok_text("Switch")
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let (root, branch) = (root.clone(), branch.clone());
                    let _ = this.update(cx, |_, cx| {
                        cx.spawn_in(window, async move |_, cx| {
                            let label = branch.clone();
                            let result = cx
                                .background_executor()
                                .spawn(async move {
                                    crate::features::palette::switch_branch(&root, &branch)
                                })
                                .await;
                            let _ = cx.update(|window, cx| {
                                let note = match result {
                                    Ok(()) => Notification::success(format!("Switched to {label}")),
                                    Err(message) => Notification::error(message),
                                };
                                window.push_notification(note, cx);
                            });
                        })
                        .detach();
                    });
                    true
                })
        });
    }

    // ── Panes ───────────────────────────────────────────────────────────────
}
