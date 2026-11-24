//! The activity line over the composer: turn timer, phase, current tool and
//! the session's status chips (jobs, widgets, usage, helpers, rate limits).

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{TINY, clock};
use crate::features::session::SessionView;
use crate::features::session::agents::{JobRow, JobTail, job_row};
use crate::shared::ui;
use gentle_core::tools;
use gentle_core::transcript::{Item, ToolStatus, format_tokens};

impl SessionView {
    /// Elapsed time, phase and current tool over the composer, with the
    /// session's live chips on the right.
    pub(super) fn render_activity(
        &self,
        rem_size: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let busy = self.busy();
        let waiting = self.live.pending.as_ref().filter(|p| !p.started);
        let since = self.turn_since();
        // A stop was asked for this very turn: say so until it ends.
        let stopping = since.is_some() && self.live.stopping == since;
        // Only shown while busy; with no tool running there is nothing to find.
        let scan = busy && self.transcript.running_tools() > 0;
        let current_tool = self
            .transcript
            .items
            .get(if scan { 0 } else { self.transcript.items.len() }..)
            .unwrap_or_default()
            .iter()
            .rev()
            .find_map(|item| match item {
                Item::Tool {
                    name,
                    args,
                    details,
                    status: ToolStatus::Running,
                    ..
                } => {
                    let s = tools::summarize(name, args, details);
                    Some(if s.subject.is_empty() {
                        s.title
                    } else {
                        format!(
                            "{} {}",
                            s.title,
                            gentle_core::text::first_line(&s.subject, 60)
                        )
                    })
                }
                _ => None,
            });
        let provider = self
            .models
            .current
            .as_ref()
            .map_or_else(|| "the agent".to_string(), |m| m.provider.clone());
        let sections = self.desktop.sections();
        // Gentle Shell's own ODD phase (`implementing…`) when its session feed
        // reports one; the phase inferred from tool calls otherwise.
        let feed_phase = sections.phase_label().map(str::to_string);
        // A question the session waits on that the app shows no dialog for
        // (it only works in the terminal, or no dialog came).
        let question = sections
            .pending_question(std::time::Instant::now())
            .filter(|_| self.extension.dialogs.is_empty())
            .map(|question| {
                let text = gentle_core::text::first_line(&question.question, 80);
                if question.terminal_only {
                    format!("Answer in Gentle Shell's terminal: {text}")
                } else {
                    format!("Waiting for an answer: {text}")
                }
            });

        let left = h_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .overflow_hidden()
            .whitespace_nowrap()
            .when_some(question, |this, question| {
                this.child(
                    h_flex()
                        .min_w_0()
                        .gap_1()
                        .text_color(ui::warning())
                        .child(Icon::new(IconName::MessageSquare).xsmall())
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(question),
                        ),
                )
            })
            .when(busy, |this| {
                // The turn's clock; the composer's orbit shows it's live.
                this.when_some(since, |this, since| {
                    this.child(
                        div()
                            .font_family(ui::mono())
                            .text_size(ui::code_rems(11.))
                            .text_color(ui::muted())
                            .child(clock(since.elapsed())),
                    )
                    .child(ui::each_second())
                })
                .child(div().h(rems(10. / 16.)).w(px(1.)).bg(ui::border_strong()))
                .map(|this| match (waiting, feed_phase, self.activity.phase) {
                    _ if stopping => this.child(div().text_color(ui::muted()).child("✿ stopping…")),
                    (Some(_), _, _) => this.child(
                        div()
                            .text_color(ui::petal())
                            .child(format!("✿ waiting for {provider}…")),
                    ),
                    (None, Some(label), _) => {
                        this.child(div().text_color(ui::petal()).child(format!("✿ {label}")))
                    }
                    (None, None, Some(phase)) => {
                        this.child(div().text_color(ui::petal()).child(format!("✿ {phase}…")))
                    }
                    (None, None, None) => {
                        this.child(div().text_color(ui::petal()).child("✿ working…"))
                    }
                })
                .when_some(current_tool, |this, tool| {
                    this.child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(ui::muted())
                            .child(tool),
                    )
                })
            });

        let width = self.pane_width.get();
        // Only what needs you shows by itself; the rest waits one hover away.
        // Extension statuses (a server's state) fold into one quiet chip, as
        // do the plan limits until one runs high; tiny panes fold every
        // limit. Engram's status is the exception: its brain opens the
        // Memory screen on what it names (folded too in tiny panes).
        let collapse = width < f32::from(rems(TINY / 16.).to_pixels(rem_size));
        let memory = self
            .extension
            .statuses
            .get("engram")
            .filter(|_| !collapse)
            .and_then(|s| Some((s.clone(), gentle_core::engram::status_target(s)?)));
        let statuses: Vec<(String, String)> = self
            .extension
            .statuses
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "gentle:mode" | "gentle:yolo" | "gentle-jobs"))
            .filter(|(k, _)| memory.is_none() || k.as_str() != "engram")
            .map(|(k, s)| (k.clone(), s.clone()))
            .collect();
        let limit_color = |percent: u32| {
            use gentle_core::usage::subscription::LimitTone;
            match gentle_core::usage::subscription::limit_tone(f64::from(percent)) {
                LimitTone::Danger => Some(ui::danger()),
                LimitTone::Warning => Some(ui::warning()),
                LimitTone::Calm => None,
            }
        };
        let folded: Vec<String> = statuses
            .iter()
            .map(|(_, s)| s.clone())
            .chain(
                self.extension
                    .rate_limits
                    .values()
                    .filter(|l| collapse || limit_color(l.percent).is_none())
                    .map(|l| l.text.clone()),
            )
            .collect();
        let right = h_flex()
            .flex_shrink_0()
            // Each control brings its own 4px inset (an xsmall button's box),
            // so a hairline gap reads as one group: ~10px between glyphs.
            .gap_0p5()
            .when(!folded.is_empty(), |this| {
                let tip = folded.join("\n");
                this.child(
                    div()
                        .id("status-overflow")
                        .flex()
                        .items_center()
                        // The box of an xsmall ghost button, as its neighbours.
                        .h_5()
                        .px_1()
                        .gap_1()
                        .text_color(ui::faint())
                        .hover(|s| s.text_color(ui::muted()))
                        .child(Icon::new(IconName::Info).xsmall())
                        .when(folded.len() > 1, |d| d.child(folded.len().to_string()))
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)),
                )
            })
            .when(self.jobs_visible(), |this| {
                this.child(self.render_jobs_chip(cx))
            })
            // The working tree has changes: `±` and the count; a click
            // opens the review.
            .when(!collapse, |this| {
                this.children(self.render_changes_button(cx))
            })
            // Engram's status: a brain (and the memory it just touched); a
            // click opens Memory on that project and memory.
            .when_some(memory, |this, (status, target)| {
                this.child(render_memory_button(status, target, cx))
            })
            .when(self.activity.queued > 0, |this| {
                this.child(ui::chip(
                    format!("{} queued", self.activity.queued),
                    ui::warning(),
                ))
            })
            // Tern-style: jobs and widgets live in chips here instead of a
            // side panel. The task list floats at the top of the
            // conversation instead (see `render_todo_island`).
            .when(!collapse, |this| this.children(self.render_widget_chips()))
            .when(self.helpers_visible(), |this| {
                this.child(self.render_helpers_chip(cx))
            })
            .children(
                self.extension
                    .rate_limits
                    .iter()
                    .filter(|_| !collapse)
                    .filter_map(|(window, limit)| {
                        let color = limit_color(limit.percent)?;
                        let tip = limit.text.clone();
                        Some(
                            div()
                                .id(ElementId::Name(format!("limit-{window}").into()))
                                .child(ui::chip(
                                    format!("{} {}%", limit_label(window), limit.percent),
                                    color,
                                ))
                                .tooltip(move |window, cx| {
                                    Tooltip::new(tip.clone()).build(window, cx)
                                }),
                        )
                    }),
            )
            // The review switch sits with the permission chip: both are this
            // workspace's rules for the agent (Gentle Shell `main`).
            .when(!collapse, |this| this.children(self.render_review_chip(cx)))
            .child(self.render_mode_picker(cx));

        h_flex()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .h(rems(24. / 16.))
            .px_1()
            .gap_3()
            .text_size(rems(11. / 16.))
            .child(left)
            .child(right)
    }

    // ── Status chips (Tern-style: everything that used to sit in a side
    //    panel lives in small chips over the composer, each opening a card) ──

    /// Whether the jobs chip has anything to show (structured or legacy).
    fn jobs_visible(&self) -> bool {
        if self.desktop.jobs_ready() {
            !self.desktop.jobs().is_empty()
        } else {
            !self.activity.jobs.is_empty()
        }
    }

    /// Whether the helpers chip has anything to show (structured or legacy).
    fn helpers_visible(&self) -> bool {
        if self.desktop.subagents_ready() {
            !self.desktop.subagents().is_empty()
        } else {
            self.activity
                .agents
                .as_ref()
                .is_some_and(|a| !a.tasks.is_empty())
        }
    }

    /// Background jobs: a spinner and "2 running" or "Jobs 3", with the
    /// latest 20 on click (the Agents window's Jobs tab lists all), a cancel
    /// action for running jobs once the peer offers `cancel_job`, and each
    /// job's output on click once it offers `get_job_output`.
    fn render_jobs_chip(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // Counts use the full list, never the truncated display rows, so a
        // >20-job session still reports its real running/total.
        let all = self.job_rows();
        let running = all.iter().filter(|row| row.is_running()).count();
        let total = all.len();
        let rows: Vec<(JobRow, JobTail)> = all
            .into_iter()
            .take(20)
            .map(|row| {
                let tail = self.job_tail(row.id());
                (row, tail)
            })
            .collect();
        let cancel = self.desktop.cancel_job_supported();
        let color = if running > 0 {
            ui::accent()
        } else {
            ui::muted()
        };
        let label = if running > 0 {
            format!("{running} running")
        } else {
            format!("Jobs {total}")
        };
        let weak = cx.entity().downgrade();
        let closing = weak.clone();
        chip_popover(
            ChipPopoverOptions::new("jobs", IconName::Activity, color, label)
                .title("Background jobs")
                .count(format!("{running}/{total}")),
            move || {
                rows.iter()
                    .map(|(row, tail)| job_row(row, cancel, tail.clone(), &weak))
                    .collect()
            },
        )
        // A closed card shows no output: stop reading it.
        .on_open_change(move |open, _, cx| {
            if !*open {
                let _ = closing.update(cx, |this, cx| this.stop_job_output(cx));
            }
        })
    }

    /// Extension widgets (`setWidget`): one chip each, its lines on click.
    /// Gentle Shell's YOLO banner (`gentle:yolo`) is left out: the
    /// permission chip already shows the mode.
    fn render_widget_chips(&self) -> Vec<AnyElement> {
        self.extension
            .widgets
            .iter()
            .filter(|(key, _)| key.as_str() != "gentle:yolo")
            .map(|(key, lines)| {
                let lines = lines.clone();
                chip_popover(
                    ChipPopoverOptions::new(
                        SharedString::from(format!("widget-{key}")),
                        IconName::LayoutDashboard,
                        ui::muted(),
                        gentle_core::text::first_line(key, 18),
                    )
                    .title(key),
                    move || {
                        lines
                            .iter()
                            .map(|line| {
                                div()
                                    .min_w_0()
                                    .text_size(ui::code_rems(12.))
                                    .font_family(ui::mono())
                                    .text_color(ui::muted())
                                    .child(line.clone())
                                    .into_any_element()
                            })
                            .collect()
                    },
                )
                .into_any_element()
            })
            .collect()
    }

    /// Helpers (subagents): "1/3" active of total; the card lists each with
    /// its status, task (or, from the structured list, its model) and last
    /// step, and opens the full Agents window.
    fn render_helpers_chip(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (active, rows) = if self.desktop.subagents_ready() {
            let agents = self.desktop.subagents();
            (
                agents
                    .iter()
                    .filter(|agent| {
                        matches!(agent.status.as_str(), "running" | "queued" | "waiting")
                    })
                    .count(),
                agents
                    .iter()
                    .take(20)
                    .map(|agent| {
                        (
                            agent.agent.clone(),
                            agent.status.clone(),
                            crate::features::session::agents::status_color(&agent.status),
                            agent.model.as_ref().map(|model| match &agent.thinking {
                                Some(thinking) => format!("{model} · {thinking}"),
                                None => model.clone(),
                            }),
                            Some(agent.last_step.to_string()),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        } else {
            match &self.activity.agents {
                Some(activity) => (
                    (activity.summary.running + activity.summary.waiting + activity.summary.queued)
                        as usize,
                    activity
                        .tasks
                        .iter()
                        .take(20)
                        .map(|task| {
                            let s = &task.summary;
                            (
                                s.agent.clone(),
                                s.status.clone(),
                                crate::features::session::agents::status_color(&s.status),
                                s.label.clone(),
                                s.last_step
                                    .as_deref()
                                    .map(|step| gentle_core::text::first_line(step, 60)),
                            )
                        })
                        .collect::<Vec<_>>(),
                ),
                None => (0, Vec::new()),
            }
        };
        let total = rows.len();
        let color = if active > 0 {
            ui::warning()
        } else {
            ui::muted()
        };
        let weak = cx.entity().downgrade();
        let open_agents = weak.clone();
        chip_popover(
            ChipPopoverOptions::new(
                "helpers",
                IconName::Users,
                color,
                format!("{active}/{total}"),
            )
            .title("Helpers")
            .count(format!("{active} active"))
            .footer_action(Box::new(move |_window, cx| {
                h_flex()
                    .pt_1()
                    .child(
                        Button::new("helpers-open-agents")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::Users))
                            .label(format!("Open Agents ({})", ui::shortcut_shift("A")))
                            .on_click(cx.listener({
                                let open_agents = open_agents.clone();
                                move |state: &mut PopoverState, _, window, cx| {
                                    state.dismiss(window, cx);
                                    let _ = open_agents
                                        .update(cx, |this, cx| this.open_agents(None, cx));
                                }
                            })),
                    )
                    .into_any_element()
            })),
            move || {
                rows.iter()
                    .map(|(agent, status, color, label, step)| {
                        v_flex()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .min_w_0()
                                    .text_size(rems(12. / 16.))
                                    .child(
                                        div()
                                            .flex_none()
                                            .size(rems(6. / 16.))
                                            .rounded_full()
                                            .bg(*color),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(ui::fg())
                                            .child(agent.clone()),
                                    )
                                    .child(
                                        div().flex_none().text_color(*color).child(status.clone()),
                                    ),
                            )
                            .when_some(label.clone(), |d, label| {
                                d.child(
                                    div()
                                        .pl_4()
                                        .min_w_0()
                                        .text_size(rems(11. / 16.))
                                        .text_color(ui::muted())
                                        .child(label),
                                )
                            })
                            .when_some(step.clone(), |d, step| {
                                d.child(
                                    div()
                                        .pl_4()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(ui::code_rems(11.))
                                        .font_family(ui::mono())
                                        .text_color(ui::faint())
                                        .child(step),
                                )
                            })
                            .into_any_element()
                    })
                    .collect()
            },
        )
    }

    /// The working tree's changes: the `±` icon with the file count in
    /// amber, as on the sidebar's Changes tab; a click opens the review.
    fn render_changes_button(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let files = self.git.snapshot.as_ref()?.files.len();
        if files == 0 {
            return None;
        }
        let tip = format!(
            "Review changes · {files} {}",
            if files == 1 { "file" } else { "files" }
        );
        Some(
            h_flex()
                .id("review-changes")
                .flex_none()
                // The box of an xsmall ghost button, as its neighbours.
                .h_5()
                .px_1()
                .gap_0p5()
                .rounded_md()
                .cursor_pointer()
                .text_color(ui::faint())
                .hover(|s| s.text_color(ui::muted()).bg(ui::surface_hover()))
                .child(Icon::new(IconName::Diff).xsmall())
                .child(
                    div()
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(10.5))
                        .text_color(ui::warning())
                        .child(files.to_string()),
                )
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.emit(crate::features::session::SessionEvent::OpenScreen(
                        gentle_core::commands::Screen::Changes,
                    ))
                })),
        )
    }

    /// What the session spent, quietly beside the send: the cost in the
    /// code font after a dollar sign (or the last response's output), the
    /// last response's tokens on click.
    pub(super) fn render_usage_chip(&self) -> Option<impl IntoElement + use<>> {
        let usage = self.transcript.last_usage;
        let cost = self.usage.cost;
        let (icon, label) = if cost > 0.0 {
            (IconName::DollarSign, format!("{cost:.2}"))
        } else {
            (
                IconName::ChartPie,
                format!("out {}", format_tokens(usage?.output)),
            )
        };
        let rows: Vec<(&'static str, String)> = usage
            .map(|usage| {
                vec![
                    ("Input", format_tokens(usage.input)),
                    ("Output", format_tokens(usage.output)),
                    ("Cache read", format_tokens(usage.cache_read)),
                ]
            })
            .unwrap_or_default()
            .into_iter()
            .chain((cost > 0.0).then(|| ("Session cost", format!("${cost:.2}"))))
            .collect();
        Some(chip_popover(
            ChipPopoverOptions::new("last-usage", icon, ui::faint(), label)
                .title("Usage")
                .mono(),
            move || {
                rows.iter()
                    .map(|(name, value)| {
                        h_flex()
                            .text_size(rems(12. / 16.))
                            .child(div().flex_1().text_color(ui::muted()).child(*name))
                            .child(
                                div()
                                    .font_family(ui::mono())
                                    .text_size(ui::code_rems(12.))
                                    .text_color(ui::fg())
                                    .child(value.clone()),
                            )
                            .into_any_element()
                    })
                    .collect()
            },
        ))
    }
}

/// gentle-engram's status as a button in the changes button's look: the
/// brain, `#id` once the status names a memory (saved, updated, read,
/// reviewed); the status itself and what opens in the tooltip.
fn render_memory_button(
    status: String,
    target: gentle_core::engram::MemoryTarget,
    cx: &mut Context<SessionView>,
) -> impl IntoElement + use<> {
    let opens = match (target.observation, &target.project) {
        (Some(id), _) => format!("Open memory #{id}"),
        (None, Some(project)) => format!("Open {project}'s memories"),
        (None, None) => "Open Memory".to_string(),
    };
    let tip = format!("{status}\n{opens}");
    h_flex()
        .id("engram-memory")
        .flex_none()
        // The box of an xsmall ghost button, as its neighbours.
        .h_5()
        .px_1()
        .gap_0p5()
        .rounded_md()
        .cursor_pointer()
        .text_color(ui::faint())
        .hover(|s| s.text_color(ui::muted()).bg(ui::surface_hover()))
        .child(Icon::new(IconName::Brain).xsmall())
        .when_some(target.observation, |this, id| {
            this.child(
                div()
                    .font_family(ui::mono())
                    .text_size(ui::code_rems(10.5))
                    .child(format!("#{id}")),
            )
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.emit(crate::features::session::SessionEvent::OpenMemory(
                target.clone(),
            ))
        }))
}

/// An extra control at a chip card header's right side (where the count
/// otherwise sits): it is rebuilt with the card, so it can carry a listener
/// on the popover's own state — `cx.listener` + `state.dismiss` — to act and
/// close in one click.
pub(super) type ChipHeaderAction =
    Box<dyn Fn(&mut Window, &mut Context<PopoverState>) -> AnyElement>;

/// An extra control at the bottom of a chip card's scrollable body. Like
/// [`ChipHeaderAction`], it is rebuilt with the card and can dismiss the
/// popover from its click handler.
pub(super) type ChipFooterAction = ChipHeaderAction;

/// The slots of a chip card's header, shared by every chip popover: the
/// trigger's id, icon and color, its label, the card's title, and either a
/// count in `ui::mono()` or a header action where the count would sit.
pub(super) struct ChipPopoverOptions {
    id: SharedString,
    icon: IconName,
    color: Hsla,
    label: SharedString,
    title: SharedString,
    count: SharedString,
    action: Option<ChipHeaderAction>,
    footer_action: Option<ChipFooterAction>,
    mono: bool,
    ring: Option<(
        f32,
        ButtonCustomVariant,
        Entity<SessionView>,
        bool,
        bool,
        bool,
    )>,
}

impl ChipPopoverOptions {
    pub(super) fn new(
        id: impl Into<SharedString>,
        icon: IconName,
        color: Hsla,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            color,
            label: label.into(),
            title: "".into(),
            count: "".into(),
            action: None,
            footer_action: None,
            mono: false,
            ring: None,
        }
    }

    /// The card's title, next to the icon.
    pub(super) fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = title.into();
        self
    }

    /// A right-side count in `ui::mono()` ("2/3", "3.4k of 200k").
    pub(super) fn count(mut self, count: impl Into<SharedString>) -> Self {
        self.count = count.into();
        self
    }

    /// A right-side control instead of the count (see [`ChipHeaderAction`]).
    pub(super) fn action(mut self, action: ChipHeaderAction) -> Self {
        self.action = Some(action);
        self
    }

    pub(super) fn ring(
        mut self,
        percent: f32,
        session: Entity<SessionView>,
        open: bool,
        hovered: bool,
        compacting: bool,
        cx: &App,
    ) -> Self {
        self.ring = Some((
            percent,
            ButtonCustomVariant::new(cx),
            session,
            open,
            hovered,
            compacting,
        ));
        self
    }

    /// A bottom control inside the card body (see [`ChipFooterAction`]).
    pub(super) fn footer_action(mut self, action: ChipFooterAction) -> Self {
        self.footer_action = Some(action);
        self
    }

    /// The trigger's label in `ui::mono()`, for figures.
    pub(super) fn mono(mut self) -> Self {
        self.mono = true;
        self
    }
}

/// A status chip over the composer that opens a card above it: icon, title,
/// optional count or header action, and the rows `body` builds when the card
/// opens. Every chip (todos, jobs, widgets, usage) shares this look.
pub(super) fn chip_popover(
    options: ChipPopoverOptions,
    body: impl Fn() -> Vec<AnyElement> + 'static,
) -> Popover {
    let ChipPopoverOptions {
        id,
        icon,
        color,
        label,
        title,
        count,
        action,
        footer_action,
        mono,
        ring,
    } = options;
    let ring_session = ring
        .as_ref()
        .map(|(_, _, session, open, _, compacting)| (session.downgrade(), *open && !compacting));
    let trigger = if let Some((percent, style, session, open, hovered, compacting)) = ring {
        let hover_session = session.downgrade();
        Button::new("context-ring-trigger")
            .custom(style)
            .xsmall()
            .size(rems(25. / 16.))
            .p_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .disabled(compacting)
            .when(!compacting, |button| button.cursor_pointer())
            .on_hover(move |hovered, _, cx| {
                let _ = hover_session.update(cx, |session, cx| {
                    let hovered = *hovered && !session.usage.compacting;
                    if session.usage.context_ring_hovered != hovered {
                        session.usage.context_ring_hovered = hovered;
                        cx.notify();
                    }
                });
            })
            .child(
                div()
                    .id("context-ring")
                    .when(!open && !compacting, |d| {
                        d.tooltip(move |window, cx| {
                            let session = session.clone();
                            let overview = cx.new(|cx| {
                                cx.observe(&session, |_, _, cx| cx.notify()).detach();
                                ContextOverview { session }
                            });
                            Tooltip::element(move |_, _| overview.clone()).build(window, cx)
                        })
                    })
                    .child(context_ring(percent, color, hovered && !compacting)),
            )
    } else {
        Button::new(SharedString::from(format!("{id}-chip")))
            .ghost()
            .xsmall()
            .text_color(color)
            .when(mono, |b| b.font_family(ui::mono()))
            .icon(Icon::new(icon))
            .label(label)
    };
    Popover::new(SharedString::from(format!("{id}-popover")))
        .anchor(Anchor::BottomRight)
        .trigger(trigger)
        .w(rems(360. / 16.))
        .appearance(false)
        .when_some(ring_session, |popover, (session, open)| {
            popover.open(open).on_open_change(move |open, _, cx| {
                let _ = session.update(cx, |session, cx| {
                    session.usage.context_details_open = *open && !session.usage.compacting;
                    cx.notify();
                });
            })
        })
        .content(move |_, window, cx| {
            ui::island(cx)
                .w_full()
                .child(ui::popover_in(
                    ElementId::Name(format!("{id}-content-in").into()),
                    v_flex()
                        .p_3()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(Icon::new(icon).small().text_color(
                                    if id == "context-ring" {
                                        ui::muted()
                                    } else {
                                        color
                                    },
                                ))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(ui::fg())
                                        .child(title.clone()),
                                )
                                .child(div().flex_1())
                                .when(!count.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .debug_selector({
                                                let id = id.clone();
                                                move || format!("{id}-header-count")
                                            })
                                            .text_size(ui::code_rems(12.))
                                            .font_family(ui::mono())
                                            .text_color(ui::muted())
                                            .child(count.clone()),
                                    )
                                })
                                .when_some(action.as_ref(), |d, build| d.child(build(window, cx))),
                        )
                        .child(
                            v_flex()
                                .id(SharedString::from(format!("{id}-popover-list")))
                                .max_h(rems(380. / 16.))
                                .overflow_y_scroll()
                                .gap_1p5()
                                .children(body())
                                .when_some(footer_action.as_ref(), |d, build| {
                                    d.child(build(window, cx))
                                }),
                        ),
                ))
                .into_any_element()
        })
}

struct ContextOverview {
    session: Entity<SessionView>,
}

impl Render for ContextOverview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let usage = &self.session.read(cx).usage;
        let percent = usage.context_percent.map(|p| p.clamp(0., 100.));
        let tone = match percent {
            Some(p) if p >= 90. => ui::danger(),
            Some(p) if p >= 70. => ui::warning(),
            _ => ui::success(),
        };
        let prefix = if usage.context_estimated { "~" } else { "" };
        let percent_label = percent.map_or_else(|| "–".to_string(), |p| format!("{prefix}{p:.0}%"));
        let usage_label = usage.context_tokens.map_or_else(
            || percent_label.clone(),
            |tokens| format!("{prefix}{} ({percent_label})", format_tokens(tokens)),
        );
        let window_label = self
            .session
            .read(cx)
            .context_window()
            .map_or_else(|| "–".to_string(), format_tokens);
        let percent_selector = format!("context-ring-overview-{usage_label}");
        let window_selector = format!("context-ring-overview-window-{window_label}");
        v_flex()
            .w(rems(220. / 16.))
            .py_2()
            .gap_2()
            .child(
                h_flex()
                    .gap_1p5()
                    .text_color(ui::muted())
                    .child(Icon::new(IconName::Gauge).small())
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(ui::fg())
                            .child("Context"),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .text_color(ui::muted())
                    .child(
                        div()
                            .debug_selector(move || percent_selector.clone())
                            .child(usage_label),
                    )
                    .child(
                        div()
                            .debug_selector(move || window_selector.clone())
                            .child(window_label),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .h(px(5.))
                    .rounded_full()
                    .overflow_hidden()
                    .bg(ui::border_strong())
                    .child(
                        div()
                            .h_full()
                            .w(relative(percent.unwrap_or(0.) as f32 / 100.))
                            .rounded_full()
                            .bg(tone),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "context-ring-overview-hint".into())
                    .text_size(rems(11. / 16.))
                    .text_color(ui::faint())
                    .child("Click ring for details"),
            )
    }
}

fn context_ring(percent: f32, color: Hsla, hovered: bool) -> impl IntoElement {
    let fraction = if percent.is_finite() {
        percent.clamp(0., 100.) / 100.
    } else {
        0.
    };
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let center = bounds.center();
            let radius = bounds.size.width.min(bounds.size.height) * (17. / 25.) / 2. - px(1.5);
            if hovered {
                window.paint_quad(
                    fill(bounds, ui::surface_hover().opacity(0.6))
                        .corner_radii(bounds.size.width.min(bounds.size.height) / 2.),
                );
            }
            let outer_radius = radius + px(1.);
            let track_bounds = Bounds::new(
                point(center.x - outer_radius, center.y - outer_radius),
                size(outer_radius * 2., outer_radius * 2.),
            );
            window.paint_quad(
                outline(track_bounds, ui::border_strong(), BorderStyle::Solid)
                    .border_widths(px(2.))
                    .corner_radii(outer_radius),
            );
            let draw = |window: &mut Window, fraction: f32, color: Hsla| {
                if fraction <= 0. {
                    return;
                }
                let segments = (64. * fraction).ceil() as usize;
                let mut path = PathBuilder::stroke(px(2.));
                for i in 0..=segments {
                    let angle = -std::f32::consts::FRAC_PI_2
                        + std::f32::consts::TAU * fraction * i as f32 / segments as f32;
                    let point = point(
                        center.x + radius * angle.cos(),
                        center.y + radius * angle.sin(),
                    );
                    if i == 0 {
                        path.move_to(point);
                    } else {
                        path.line_to(point);
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            draw(window, fraction, color);
        },
    )
    .size(rems(25. / 16.))
}

/// `Claude rate limit warning: 51% used (seven_day)` → (51, `seven_day`).
pub(in crate::features::session) fn parse_rate_limit(message: &str) -> Option<(u32, String)> {
    const MARK: &str = "% used (";
    let at = message.find(MARK)?;
    let before = &message[..at];
    let digits = before.len() - before.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let percent = before[at - digits..].parse().ok()?;
    let rest = &message[at + MARK.len()..];
    let window = &rest[..rest.find(')')?];
    (!window.is_empty()
        && window
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'))
    .then(|| (percent, window.to_string()))
}

/// `seven_day` → `7d`, `five_hour` → `5h`.
fn limit_label(window: &str) -> String {
    let short = window
        .replace("seven_day", "7d")
        .replace("five_hour", "5h")
        .replace("one_hour", "1h")
        .replace("one_day", "1d");
    short.replace('_', " ")
}

#[cfg(test)]
mod tests {
    use super::{limit_label, parse_rate_limit};

    #[test]
    fn rate_limit_warnings_parse() {
        assert_eq!(
            parse_rate_limit("Claude rate limit warning: 51% used (seven_day)"),
            Some((51, "seven_day".to_string()))
        );
        assert_eq!(parse_rate_limit("Everything is fine"), None);
        assert_eq!(limit_label("seven_day"), "7d");
        assert_eq!(limit_label("five_hour"), "5h");
        assert_eq!(limit_label("seven_day_opus"), "7d opus");
    }
}
