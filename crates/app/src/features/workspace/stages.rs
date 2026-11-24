//! Floating web pages and the title bar's instance tray. What an agent opens
//! (a dev server's URL, the design canvas, `gentle-open`) starts hidden in the
//! tray, so the window never rearranges on its own; the user looks at it from
//! the tray's circle. What the user opens (`/web`) shows at once. A hidden
//! page costs nothing: it has no native view. Simulators and emulators have
//! their own panel (`features::devices`).

use super::*;
use crate::features::browser::{
    BrowserEvent, BrowserPanel, CanvasNotice, card_layout, external, set_card_layout,
};
use crate::shared::floating;
use gentle_core::browser::CardLayout;
use gentle_core::web::same_page;
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::{ActiveTheme as _, Selectable};

/// One web page, shown (floating, or docked in the window's right column) or
/// kept in the tray.
pub(super) struct Stage {
    id: u64,
    panel: Entity<BrowserPanel>,
    shown: bool,
    /// The agent pane that opened it, named in the tray.
    origin: Option<PaneId>,
    /// What the tray last drew for it: title and address. The page's own
    /// redraws only redraw the window when this changes.
    summary: (String, String),
    _subscriptions: [Subscription; 2],
}

impl Stage {
    fn docked(&self, cx: &App) -> bool {
        self.panel.read(cx).mode() == floating::Mode::Docked
    }

    /// The floating card, while shown.
    pub(super) fn element(&self, cx: &App) -> Option<AnyElement> {
        (self.shown && !self.docked(cx)).then(|| self.panel.clone().into_any_element())
    }

    /// The card in the docked column, while shown.
    pub(super) fn docked_element(&self, cx: &App) -> Option<AnyElement> {
        (self.shown && self.docked(cx)).then(|| self.panel.clone().into_any_element())
    }

    fn set_shown(&mut self, shown: bool, cx: &mut App) {
        self.shown = shown;
        self.panel.update(cx, |panel, cx| {
            if shown {
                panel.show(cx)
            } else {
                panel.hide(cx)
            }
        });
    }

    /// What the tray shows for it: title and address.
    fn summary(panel: &Entity<BrowserPanel>, cx: &App) -> (String, String) {
        let panel = panel.read(cx);
        (panel.title(), address(panel.url()))
    }
}

/// A URL without its scheme, for status lines.
fn address(url: &str) -> String {
    url.trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_string()
}

impl Workspace {
    // ── Opening ─────────────────────────────────────────────────────────────

    /// The card already showing `url` ([`same_page`]), whoever opened it:
    /// one page never gets two cards.
    fn stage_for(&self, url: &str, cx: &App) -> Option<(u64, Entity<BrowserPanel>)> {
        self.stages
            .iter()
            .find(|stage| same_page(stage.panel.read(cx).url(), url))
            .map(|stage| (stage.id, stage.panel.clone()))
    }

    /// Show a page in its own card. `reveal` is false for pages an agent
    /// opened (a dev server's URL, `gentle-open`): those wait in the tray. A
    /// page that is already open (the design canvas too) reuses its card;
    /// a web page reloads.
    pub(super) fn open_web(
        &mut self,
        url: String,
        origin: Option<PaneId>,
        reveal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((id, entity)) = self.stage_for(&url, cx) {
            if !entity.read(cx).is_design() {
                entity.update(cx, |panel, cx| panel.navigate(url, None, cx));
            }
            if reveal {
                self.set_stage_shown(id, true, window, cx);
            }
            return;
        }
        let panel = cx.new(|cx| BrowserPanel::new(url, None, cx));
        self.add_stage(panel, origin, reveal, window, cx);
    }

    /// A page an agent opened (`gentle-open`, a dev server's URL in a tool's
    /// output): in the tray, or in the browser outside the app when
    /// Configuration › Devices › Browser says so. `once`: a URL picked out
    /// of a tool's output or a notice, which only opens outside the first
    /// time this window sees it.
    pub(super) fn open_agent_page(
        &mut self,
        url: String,
        origin: Option<PaneId>,
        once: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if external::agent_pages(cx).outside() {
            self.open_outside(url, once, cx);
        } else {
            self.open_web(url, origin, false, window, cx);
        }
    }

    fn open_outside(&mut self, url: String, once: bool, cx: &mut Context<Self>) {
        if !self.opened_outside.insert(url.clone()) && once {
            return;
        }
        external::open(&url, cx);
    }

    /// Gentle Design's canvas: kept in the tray when it opens (or opened once
    /// in the browser outside the app, which the canvas then reloads itself),
    /// reloaded in place when it refreshes, removed when it stops.
    pub(super) fn show_design_canvas(
        &mut self,
        canvas: CanvasNotice,
        origin: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let design = |stage: &Stage, cx: &App| {
            stage
                .panel
                .read(cx)
                .is_design()
                .then(|| stage.panel.clone())
        };
        match canvas {
            CanvasNotice::Stopped => {
                let ids: Vec<u64> = self
                    .stages
                    .iter()
                    .filter(|stage| design(stage, cx).is_some())
                    .map(|stage| stage.id)
                    .collect();
                for id in ids {
                    self.remove_stage(id, window, cx);
                }
            }
            CanvasNotice::Open { url, .. } => {
                let label = Some("Design canvas".to_string());
                // The design card, or a card already showing the canvas's
                // page (`gentle-open` of its URL), which becomes the design
                // card.
                let card = self
                    .stages
                    .iter()
                    .find_map(|stage| design(stage, cx))
                    .or_else(|| self.stage_for(&url, cx).map(|(_, panel)| panel));
                match card {
                    Some(entity) => entity.update(cx, |panel, cx| panel.navigate(url, label, cx)),
                    None if external::agent_pages(cx).outside() => self.open_outside(url, true, cx),
                    None => {
                        let panel = cx.new(|cx| BrowserPanel::new(url, label, cx));
                        self.add_stage(panel, Some(origin), false, window, cx);
                    }
                }
            }
        }
    }

    fn add_stage(
        &mut self,
        panel: Entity<BrowserPanel>,
        origin: Option<PaneId>,
        shown: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_stage;
        self.next_stage += 1;
        // The card's own button only hides it; the tray closes it. Docking
        // or floating a card lays the others out again.
        let subscriptions = [
            cx.subscribe_in(
                &panel,
                window,
                move |this, _, event: &BrowserEvent, window, cx| match event {
                    BrowserEvent::Hide => this.set_stage_shown(id, false, window, cx),
                    BrowserEvent::Layout => {
                        this.tile_stages(window, cx);
                        cx.notify();
                    }
                },
            ),
            cx.observe(&panel, move |this, _, cx| this.stage_changed(id, cx)),
        ];
        let summary = Stage::summary(&panel, cx);
        let mut stage = Stage {
            id,
            panel,
            shown: false,
            origin,
            summary,
            _subscriptions: subscriptions,
        };
        stage.set_shown(shown, cx);
        self.stages.push(stage);
        if shown {
            self.tile_stages(window, cx);
        }
        cx.notify();
    }

    // ── Showing, hiding, closing ────────────────────────────────────────────

    /// Redraw the tray only when what it shows for this stage changed.
    fn stage_changed(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(stage) = self.stages.iter_mut().find(|stage| stage.id == id) else {
            return;
        };
        let summary = Stage::summary(&stage.panel, cx);
        if stage.summary != summary {
            stage.summary = summary;
            cx.notify();
        }
    }

    fn set_stage_shown(
        &mut self,
        id: u64,
        shown: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(stage) = self.stages.iter_mut().find(|stage| stage.id == id) else {
            return;
        };
        if stage.shown == shown {
            return;
        }
        stage.set_shown(shown, cx);
        self.tile_stages(window, cx);
        cx.notify();
    }

    fn hide_all_stages(&mut self, cx: &mut Context<Self>) {
        for stage in self.stages.iter_mut().filter(|stage| stage.shown) {
            stage.set_shown(false, cx);
        }
        cx.notify();
    }

    /// Close a page.
    fn remove_stage(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.stages.iter().position(|stage| stage.id == id) else {
            return;
        };
        let stage = self.stages.remove(index);
        if stage.shown {
            self.tile_stages(window, cx);
        }
        cx.notify();
    }

    // ── Agents driving pages (`gentle-browser`) ─────────────────────────────

    /// Answer a `gentle-browser` operation; the answer goes to `reply`, now
    /// or once the page did it.
    pub fn browser_request(
        &mut self,
        op: serde_json::Value,
        reply: std::sync::mpsc::Sender<serde_json::Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gentle_core::browser::{Op, Profile, error, ok};
        use serde_json::json;
        let op = match serde_json::from_value::<Op>(op) {
            Ok(op) => op,
            Err(err) => {
                let _ = reply.send(error("bad_op", err.to_string()));
                return;
            }
        };
        let not_found = |tab: &str| {
            error(
                "not_found",
                format!("no page named \"{tab}\" is open (see `gentle-browser tabs`)"),
            )
        };
        match op {
            Op::Tabs => {
                let _ = reply.send(ok(json!({ "tabs": self.browser_tabs(cx) })));
            }
            Op::Open {
                name,
                url,
                profile,
                show,
            } => {
                if let Some(url) = &url
                    && !(url.starts_with("http://") || url.starts_with("https://"))
                {
                    let _ = reply.send(error("bad_url", "only http(s) pages open here"));
                    return;
                }
                let profile = profile.as_deref().map(Profile::parse);
                let found = name.as_deref().and_then(|name| self.find_stage(name, cx));
                // No page by that name, but one already shows the URL (the
                // design canvas, a `gentle-open` page): the agent takes that
                // card instead of opening a second one on the same page.
                let same = match (&found, &url) {
                    (None, Some(url)) => self.stage_for(url, cx),
                    _ => None,
                };
                let (id, panel) = match (found, same) {
                    (Some((id, panel)), _) => {
                        panel.update(cx, |panel, cx| {
                            if let Some(profile) = profile {
                                panel.set_profile(profile, cx);
                            }
                            panel.set_driven(true, cx);
                            if let Some(url) = url
                                && !same_page(panel.url(), &url)
                            {
                                panel.navigate(url, None, cx);
                            }
                        });
                        (id, panel)
                    }
                    (None, Some((id, panel))) => {
                        panel.update(cx, |panel, cx| {
                            // A card another agent named keeps its name;
                            // the answer's `tab` says which it is.
                            if panel.name().is_none() {
                                panel.set_name(name.clone(), cx);
                            }
                            if let Some(profile) = profile {
                                panel.set_profile(profile, cx);
                            }
                            panel.set_driven(true, cx);
                        });
                        (id, panel)
                    }
                    (None, None) => {
                        let Some(url) = url else {
                            let _ = reply.send(error("bad_op", "open needs a URL for a new page"));
                            return;
                        };
                        let root = self.root().display().to_string();
                        let panel = cx.new(|cx| {
                            let mut panel = BrowserPanel::new(url, None, cx);
                            panel.set_name(name.clone(), cx);
                            panel.set_repository(Some(root));
                            panel.set_profile(profile.unwrap_or(Profile::Agent), cx);
                            panel.set_driven(true, cx);
                            panel
                        });
                        let id = self.next_stage;
                        self.add_stage(panel.clone(), None, show, window, cx);
                        (id, panel)
                    }
                };
                if show {
                    self.set_stage_shown(id, true, window, cx);
                }
                let tab = self.tab_label(id, cx);
                let surface = panel.read(cx).surface().clone();
                cx.spawn_in(window, async move |_, cx| {
                    let mut answer =
                        crate::features::browser::automation::opened(surface, cx).await;
                    if let Some(state) = answer.get_mut("ok") {
                        state["tab"] = json!(tab);
                    }
                    let _ = reply.send(answer);
                })
                .detach();
            }
            other => {
                let tab = other.tab().unwrap_or_default().to_string();
                let Some((id, panel)) = self.find_stage(&tab, cx) else {
                    let _ = reply.send(not_found(&tab));
                    return;
                };
                match other {
                    Op::Close { .. } => {
                        self.remove_stage(id, window, cx);
                        let _ = reply.send(ok(json!({ "closed": tab })));
                    }
                    Op::Release { .. } => {
                        panel.update(cx, |panel, cx| panel.set_driven(false, cx));
                        let _ = reply.send(ok(json!({ "released": tab })));
                    }
                    op => {
                        if op.drives() {
                            panel.update(cx, |panel, cx| panel.set_driven(true, cx));
                        }
                        // The user picks on a page they can see.
                        if matches!(op, Op::Pick { .. }) {
                            self.set_stage_shown(id, true, window, cx);
                        }
                        let surface = panel.read(cx).surface().clone();
                        let weak = panel.downgrade();
                        cx.spawn_in(window, async move |_, cx| {
                            let answer =
                                crate::features::browser::automation::run(op, surface, cx).await;
                            // The header follows where the agent took the page.
                            if let Some(url) = answer["ok"]["url"].as_str() {
                                let url = url.to_string();
                                let _ = weak.update(cx, |panel, cx| panel.set_url(url, cx));
                            }
                            let _ = reply.send(answer);
                        })
                        .detach();
                    }
                }
            }
        }
    }

    /// A page by the name it was opened with, or its number in `tabs`.
    fn find_stage(&self, tab: &str, cx: &App) -> Option<(u64, Entity<BrowserPanel>)> {
        let tab = tab.trim();
        self.stages
            .iter()
            .find(|stage| stage.panel.read(cx).name() == Some(tab))
            .or_else(|| {
                let n: usize = tab.parse().ok()?;
                self.stages.get(n.checked_sub(1)?)
            })
            .map(|stage| (stage.id, stage.panel.clone()))
    }

    /// What `gentle-browser` calls a page: its name, or its number.
    fn tab_label(&self, id: u64, cx: &App) -> String {
        let Some(index) = self.stages.iter().position(|stage| stage.id == id) else {
            return String::new();
        };
        self.stages[index]
            .panel
            .read(cx)
            .name()
            .map_or_else(|| (index + 1).to_string(), str::to_string)
    }

    fn browser_tabs(&self, cx: &App) -> Vec<serde_json::Value> {
        self.stages
            .iter()
            .map(|stage| {
                let panel = stage.panel.read(cx);
                serde_json::json!({
                    "tab": self.tab_label(stage.id, cx),
                    "url": panel.url(),
                    "title": panel.title(),
                    "shown": stage.shown,
                    "driven": panel.is_driven(),
                    "profile": panel.profile(cx).key(),
                })
            })
            .collect()
    }

    /// A screen or dialog opened or closed over the window: web views hide
    /// under it.
    pub(super) fn set_stages_covered(&self, covered: bool, cx: &mut App) {
        for stage in self.stages.iter().filter(|stage| stage.shown) {
            stage
                .panel
                .update(cx, |panel, cx| panel.set_covered(covered, cx));
        }
        self.devices
            .update(cx, |panel, cx| panel.set_covered(covered, cx));
    }

    /// Tell agent panes whether their web views (an expanded HTML surface)
    /// may show: only those in the shown tab, with nothing over them. Only
    /// changes reach the panes, so hidden tabs aren't touched every frame.
    /// Tower panes are told apart: a hidden tab drops their page, while a
    /// screen or popover over them only conceals it, so it keeps its state.
    pub(super) fn sync_web_views(&mut self, covered: bool, cx: &mut App) {
        let in_tab: std::collections::HashSet<PaneId> = self
            .tab()
            .map(|tab| tab.leaves().into_iter().collect())
            .unwrap_or_default();
        for (id, pane) in &self.panes {
            if pane.item.kind == PaneKind::Tower
                && let Ok(tower) = pane.item.view.clone().downcast::<TowerView>()
            {
                let shown = in_tab.contains(id);
                tower.update(cx, |tower, cx| tower.set_shown(shown, covered, cx));
            }
        }
        let shown: std::collections::HashSet<PaneId> = if covered {
            Default::default()
        } else {
            in_tab
                .into_iter()
                .filter(|id| self.panes.get(id).is_some_and(|p| p.session.is_some()))
                .collect()
        };
        if shown == self.web_shown {
            return;
        }
        for (id, pane) in &self.panes {
            let Some(session) = &pane.session else {
                continue;
            };
            let on = shown.contains(id);
            if on != self.web_shown.contains(id) {
                session.update(cx, |session, cx| session.set_web_shown(on, cx));
            }
        }
        self.web_shown = shown;
    }

    /// Shown cards floating over the window (docked ones are in its column).
    pub(super) fn floating_stages(&self, cx: &App) -> usize {
        self.stages
            .iter()
            .filter(|stage| stage.shown && !stage.docked(cx))
            .count()
    }

    /// Dock the shown floating stages at the bottom-right
    /// ([`floating::docked`]): side by side at their natural size (the size
    /// the user last gave a card), shrunk together when they don't fit in the
    /// part of the window they may take, wrapped into rows past that. Cards
    /// stay draggable and resizable afterwards.
    pub(super) fn tile_stages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown: Vec<&Stage> = self
            .stages
            .iter()
            .filter(|stage| stage.shown && !stage.docked(cx))
            .collect();
        let frame = crate::features::browser::opening_frame(cx);
        let sizes: Vec<(f32, f32)> = shown.iter().map(|_| (frame.w, frame.h)).collect();
        let size = window.viewport_size();
        let title_bar = f32::from(rems(floating::TITLE_BAR / 16.).to_pixels(window.rem_size()));
        let frames = floating::docked(
            &sizes,
            (size.width.as_f32(), size.height.as_f32()),
            title_bar,
        );
        for (stage, frame) in shown.iter().zip(frames) {
            stage
                .panel
                .update(cx, |panel, cx| panel.set_frame(frame, cx));
        }
    }

    // ── The docked column ───────────────────────────────────────────────────

    /// The docked cards, stacked in a column at the window's right (left of
    /// the device panel's), with the divider that changes its width. `None`
    /// while no shown card is docked. Its width leaves the conversation room
    /// ([`floating::dock_width`]).
    pub(super) fn render_browser_dock(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let cards: Vec<AnyElement> = self
            .stages
            .iter()
            .filter_map(|stage| stage.docked_element(cx))
            .collect();
        let (conversation, column) = self.browser_dock_room.get();
        if cards.is_empty() {
            self.browser_dock_room.set((conversation, 0.));
            return None;
        }
        let room = conversation + column;
        let width = floating::dock_width(card_layout(cx).dock_width, room);
        self.browser_dock_room.set((conversation, width));
        Some(
            div()
                .id("browser-dock")
                .relative()
                .flex_none()
                .h_full()
                .w(px(width))
                .child(
                    v_flex().size_full().gap_1p5().children(
                        cards
                            .into_iter()
                            .map(|card| div().flex_1().min_h_0().child(card)),
                    ),
                )
                .child(floating::dock_divider(
                    "browser-dock-divider",
                    cx,
                    move |this, at, cx| {
                        this.browser_dock_drag = Some((at.x.as_f32(), width, room));
                        cx.notify();
                    },
                ))
                .into_any_element(),
        )
    }

    /// Measures the conversation's width for the docked column (see
    /// [`Self::render_browser_dock`]); goes inside the conversation's area.
    pub(super) fn measure_conversation(&self) -> impl IntoElement {
        let room = self.browser_dock_room.clone();
        canvas(
            move |bounds, _, _| {
                let (_, column) = room.get();
                room.set((bounds.size.width.as_f32(), column));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
    }

    /// While the docked column's divider is dragged, a layer over the window
    /// follows the pointer; the width is saved when it is let go.
    pub(super) fn browser_dock_drag_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (start_x, start_width, room) = self.browser_dock_drag?;
        Some(floating::pointer_layer(
            CursorStyle::ResizeLeftRight,
            cx,
            "browser-dock-drag",
            move |_, at, cx| {
                // The column grows leftwards from the window's right edge.
                let asked = start_width + start_x - at.x.as_f32();
                let layout = CardLayout {
                    dock_width: floating::dock_width(asked, room),
                    ..card_layout(cx)
                };
                if layout != card_layout(cx) {
                    set_card_layout(layout, false, cx);
                    cx.notify();
                }
            },
            |this, cx| {
                this.browser_dock_drag = None;
                set_card_layout(card_layout(cx), true, cx);
                cx.notify();
            },
        ))
    }

    // ── The tray ────────────────────────────────────────────────────────────

    /// The title bar's circle: how many instances are open, colored by the
    /// one that most needs a look, and the list of them in a popover. Nothing
    /// while there are none.
    pub(super) fn render_tray(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.stages.is_empty() {
            return None;
        }
        let rows: Vec<TrayRow> = self
            .stages
            .iter()
            .map(|stage| {
                let (title, status) = stage.summary.clone();
                TrayRow {
                    id: stage.id,
                    design: stage.panel.read(cx).is_design(),
                    title,
                    status,
                    color: if stage.shown {
                        ui::accent()
                    } else {
                        ui::faint()
                    },
                    source: stage
                        .origin
                        .map(|origin| self.pane_title(origin))
                        .unwrap_or_default(),
                    shown: stage.shown,
                }
            })
            .collect();
        let count = rows.len();
        let weak = cx.entity().downgrade();
        Some(
            div()
                // Clicks here are not title-bar drags.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Popover::new("instances")
                        .anchor(Anchor::TopRight)
                        .trigger(TrayTrigger { count, open: false })
                        .w(rems(380. / 16.))
                        .p_0()
                        .on_open_change(|open, window, cx| {
                            crate::features::browser::popover_open("instances", *open, window, cx)
                        })
                        .content(move |_, _, cx| {
                            ui::popover_in(
                                ElementId::Name("instances-in".into()),
                                tray_panel(rows.clone(), weak.clone(), cx),
                            )
                            .into_any_element()
                        }),
                )
                .into_any_element(),
        )
    }
}

/// What the tray shows for one stage, captured when the window draws.
#[derive(Clone)]
struct TrayRow {
    id: u64,
    design: bool,
    title: String,
    status: String,
    color: Hsla,
    /// The agent tab that opened it.
    source: SharedString,
    shown: bool,
}

fn tray_panel(
    rows: Vec<TrayRow>,
    workspace: WeakEntity<Workspace>,
    cx: &mut Context<PopoverState>,
) -> Div {
    let any_shown = rows.iter().any(|row| row.shown);
    let header = h_flex()
        .px_3()
        .pt_2p5()
        .pb_2()
        .gap_2()
        .child(
            div()
                .text_size(rems(13. / 16.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(ui::fg())
                .child("Pages"),
        )
        .child(div().flex_1())
        .when(any_shown, |d| {
            let workspace = workspace.clone();
            d.child(
                div()
                    .id("instances-hide-all")
                    .px_2()
                    .h(rems(24. / 16.))
                    .flex()
                    .items_center()
                    .rounded(rems(6. / 16.))
                    .text_size(rems(11.5 / 16.))
                    .text_color(ui::muted())
                    .cursor_pointer()
                    .hover(|s| s.bg(ui::surface_hover()).text_color(ui::fg()))
                    .child("Hide all")
                    .on_click(move |_, _, cx| {
                        let _ = workspace.update(cx, |this, cx| this.hide_all_stages(cx));
                    }),
            )
        });
    let rows = rows.into_iter().map(|row| {
        let id = row.id;
        let icon = if row.design {
            IconName::Palette
        } else {
            IconName::Globe
        };
        let mut meta = row.source.to_string();
        if !meta.is_empty() {
            meta.push_str("  ·  ");
        }
        meta.push_str(&row.status);
        let open = workspace.clone();
        let toggle = workspace.clone();
        let close = workspace.clone();
        let shown = row.shown;
        h_flex()
            .id(("instance", id as usize))
            .w_full()
            .items_start()
            .gap_2p5()
            .px_2()
            .py_2()
            .rounded(rems(8. / 16.))
            .hover(|s| s.bg(ui::surface_hover()))
            .child(
                div()
                    .mt(px(1.))
                    .size(rems(22. / 16.))
                    .flex_none()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(rems(6. / 16.))
                    .bg(row.color.opacity(0.12))
                    .child(Icon::new(icon).xsmall().text_color(row.color)),
            )
            .child(
                // The row's text opens the card and closes the list.
                v_flex()
                    .id(("instance-open", id as usize))
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .cursor_pointer()
                    .on_click(cx.listener(move |state: &mut PopoverState, _, window, cx| {
                        state.dismiss(window, cx);
                        let _ =
                            open.update(cx, |this, cx| this.set_stage_shown(id, true, window, cx));
                    }))
                    .child(
                        div()
                            .text_size(rems(12.5 / 16.))
                            .truncate()
                            .text_color(if shown { ui::fg() } else { ui::muted() })
                            .child(row.title),
                    )
                    .child(
                        div()
                            .text_size(rems(11. / 16.))
                            .text_color(ui::faint())
                            .truncate()
                            .child(meta),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_0p5()
                    .child(
                        Button::new(("instance-toggle", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(if shown {
                                IconName::EyeOff
                            } else {
                                IconName::Eye
                            }))
                            .tooltip(if shown { "Hide" } else { "Show" })
                            .on_click(move |_, window, cx| {
                                let _ = toggle.update(cx, |this, cx| {
                                    this.set_stage_shown(id, !shown, window, cx)
                                });
                            }),
                    )
                    .child(
                        Button::new(("instance-close", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::X))
                            .tooltip("Close")
                            .on_click(move |_, window, cx| {
                                let _ =
                                    close.update(cx, |this, cx| this.remove_stage(id, window, cx));
                            }),
                    ),
            )
    });
    v_flex()
        .w_full()
        .font_family(cx.theme().font_family.clone())
        .child(header)
        .child(div().h(px(1.)).bg(ui::border()))
        .child(
            v_flex()
                .id("instances-list")
                .max_h(rems(420. / 16.))
                .overflow_y_scroll()
                .px_1()
                .py_1()
                .children(rows),
        )
}

/// The circle in the title bar, with a badge counting the pages.
#[derive(IntoElement)]
struct TrayTrigger {
    count: usize,
    open: bool,
}

impl Selectable for TrayTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for TrayTrigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .id("instances-tray")
            .relative()
            .size(rems(28. / 16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .text_color(ui::fg())
            .hover(|s| s.bg(ui::surface_hover()))
            .when(self.open, |d| d.bg(ui::surface_hover()))
            .when(!self.open, |d| {
                d.tooltip(|window, cx| Tooltip::new("Pages agents opened").build(window, cx))
            })
            .child(Icon::new(IconName::Globe).small())
            .child(
                div()
                    .absolute()
                    .top(px(2.))
                    .right(px(1.))
                    .h(rems(13. / 16.))
                    .min_w(rems(13. / 16.))
                    .px(rems(3. / 16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(ui::petal())
                    .text_color(ui::well().opacity(1.))
                    .text_size(rems(9. / 16.))
                    .font_weight(FontWeight::BOLD)
                    .line_height(relative(1.))
                    .child(self.count.to_string()),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use gpui_kit::component::input::InputState;
    use gpui_kit::{App, AppContext as _, Entity, Task, TestAppContext, Window, WindowOptions};

    use super::super::{DevicePanel, Invisible, Music, Notices, Space, Workspace};
    use crate::features::browser::CanvasNotice;
    use crate::features::workspaces::TabLayout;

    const CANVAS: &str = "http://127.0.0.1:59156/fGw3aH90fb17vXLE/";

    /// A bare workspace in a test window; pages open with "Open pages" never
    /// chosen (settings.json without `agentPages`).
    fn with_workspace(
        cx: &mut TestAppContext,
        test: impl FnOnce(&Entity<Workspace>, &mut Window, &mut App),
    ) {
        cx.update(gpui_kit::init);
        let (handle, _) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Invisible))
            })
            .unwrap();
        cx.update(|cx| {
            handle.update(cx, |_, window, cx| {
                let root = std::env::temp_dir();
                let workspace = cx.new(|cx| Workspace {
                    spaces: vec![Space::new("Pages".into(), root)],
                    current: 0,
                    panes: HashMap::new(),
                    next_id: 1,
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
                    closed: Vec::new(),
                    music: cx.new(|cx| Music::new(window, cx)),
                    devices: cx.new(DevicePanel::new),
                });
                test(&workspace, window, cx);
            })
        })
        .unwrap();
    }

    fn canvas_opened(workspace: &Entity<Workspace>, window: &mut Window, cx: &mut App) {
        let notice = CanvasNotice::Open {
            url: CANVAS.into(),
            refreshed: false,
        };
        workspace.update(cx, |w, cx| w.show_design_canvas(notice, 1, window, cx));
    }

    fn gentle_open(workspace: &Entity<Workspace>, url: &str, window: &mut Window, cx: &mut App) {
        workspace.update(cx, |w, cx| {
            w.open_agent_page(url.into(), Some(1), false, window, cx)
        });
    }

    fn browser_open(workspace: &Entity<Workspace>, window: &mut Window, cx: &mut App) {
        let (reply, _answer) = std::sync::mpsc::channel();
        let op = serde_json::json!({"op": "open", "url": CANVAS, "name": "gentle-design"});
        workspace.update(cx, |w, cx| w.browser_request(op, reply, window, cx));
    }

    /// What the agent in the bug report did: the design tool opened the
    /// canvas, then it ran `gentle-open` on its URL twice and `gentle-browser
    /// open --name gentle-design` on it. One card, the design one, which the
    /// agent now drives; nothing went outside the app.
    #[gpui::test]
    fn one_card_per_page_whoever_opens_it(cx: &mut TestAppContext) {
        with_workspace(cx, |workspace, window, cx| {
            canvas_opened(workspace, window, cx);
            gentle_open(workspace, CANVAS, window, cx);
            gentle_open(workspace, CANVAS.trim_end_matches('/'), window, cx);
            browser_open(workspace, window, cx);
            let workspace = workspace.read(cx);
            assert_eq!(workspace.stages.len(), 1);
            let panel = workspace.stages[0].panel.read(cx);
            assert!(panel.is_design());
            assert!(panel.is_driven());
            assert_eq!(panel.name(), Some("gentle-design"));
            assert!(workspace.opened_outside.is_empty());
        });
    }

    /// The other order: `gentle-open` first, then the canvas notice. The
    /// card becomes the design card instead of getting a twin.
    #[gpui::test]
    fn the_canvas_takes_the_card_already_on_its_page(cx: &mut TestAppContext) {
        with_workspace(cx, |workspace, window, cx| {
            gentle_open(workspace, CANVAS, window, cx);
            canvas_opened(workspace, window, cx);
            gentle_open(workspace, "http://localhost:3000/", window, cx);
            let workspace = workspace.read(cx);
            assert_eq!(workspace.stages.len(), 2);
            assert!(workspace.stages[0].panel.read(cx).is_design());
            assert!(!workspace.stages[1].panel.read(cx).is_design());
            assert!(workspace.opened_outside.is_empty());
        });
    }
}
