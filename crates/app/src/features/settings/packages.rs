//! Packages: `gentle-shell install|remove`, Pi's own package manager
//! against the resolved home. Update badges come from the active agent
//! tab's Desktop RPC when available.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::claude::forget_claude_auth;
use super::layout::{row, section};
use super::snapshot::Snapshot;
use super::{SettingsEvent, SettingsView};
use crate::shared::ui;

/// The Advanced / Packages page's input, status and event subscription.
/// The subscription lives with its input so it cannot be dropped by accident.
pub(super) struct PackageUiState {
    pub(super) input: Entity<InputState>,
    pub(super) running: Option<String>,
    pub(super) log: Vec<String>,
    _subscription: Subscription,
}

impl PackageUiState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("npm:package-name, git:github.com/…, or a local path")
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let source = input.read(cx).value().trim().to_string();
                    if !source.is_empty() && !this.shell_update.blocks_packages() {
                        input.update(cx, |s, cx| s.set_value("", window, cx));
                        this.run_package("install", source, cx);
                    }
                }
            },
        );

        Self {
            input,
            running: None,
            log: Vec::new(),
            _subscription: subscription,
        }
    }
}

impl SettingsView {
    /// `gentle-shell install|remove <source>`: Pi's own package manager
    /// against the resolved home.
    pub(super) fn run_package(
        &mut self,
        verb: &'static str,
        source: String,
        cx: &mut Context<Self>,
    ) {
        if self.package.running.is_some() || self.shell_update.blocks_packages() {
            return;
        }
        self.package.running = Some(format!("{verb} {source}"));
        self.package.log.clear();
        let cwd = self.cwd.clone();
        self._job = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(
                    async move { crate::features::setup::run_gentle_shell(&cwd, &[verb, &source]) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.package.running = None;
                forget_claude_auth();
                let (ok, text) = match result {
                    Ok(text) => (true, text),
                    Err(text) => (false, text),
                };
                this.package.log = text
                    .lines()
                    .rev()
                    .take(8)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                if ok {
                    cx.emit(SettingsEvent::Changed);
                }
                this.reload_with(verb == "update", cx);
                cx.notify();
            });
        });
        cx.notify();
    }

    /// Installed packages (remove each) and a field to install any source.
    pub(super) fn packages_section(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let updates = self.shell_updates.status.as_ref();
        let updating_shell = self.shell_update.blocks_packages();
        let available = updates.map_or(0, crate::features::updates::status::packages_with_updates);
        // gentle-ai updates the packages of an install it manages.
        let managed = s.managed
            || updates.is_some_and(|status| {
                status.gentle_shell.channel == pi_rpc::desktop_rpc::ShellChannel::GentleAiManaged
            });
        section(IconName::Layers, "Packages")
            .when(available > 0, |d| {
                d.child(
                    h_flex()
                        .gap_2()
                        .px_2()
                        .pb_1()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(ui::accent())
                                .child(if available == 1 {
                                    "1 package has an update".to_string()
                                } else {
                                    format!("{available} packages have updates")
                                }),
                        )
                        .when(!managed, |d| {
                            d.child(
                                Button::new("update-packages")
                                    .xsmall()
                                    .ghost()
                                    .icon(Icon::new(IconName::ArrowUp))
                                    .label("Update packages")
                                    .tooltip("gentle-shell update --extensions")
                                    .disabled(self.package.running.is_some() || updating_shell)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.run_package("update", "--extensions".into(), cx)
                                    })),
                            )
                        }),
                )
            })
            .children(
                updates
                    .and_then(|status| {
                        crate::features::updates::status::packages_note(status, available)
                    })
                    .map(|note| {
                        div()
                            .px_2()
                            .pb_1()
                            .text_xs()
                            .text_color(ui::muted())
                            .child(note)
                    }),
            )
            .children(s.packages.iter().enumerate().map(|(i, p)| {
                let source = p.source.clone();
                h_flex()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_lg()
                    .hover(|d| d.bg(ui::surface_hover()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::code_rems(12.))
                            .font_family(ui::mono())
                            .text_color(ui::fg())
                            .child(p.source.clone()),
                    )
                    .when_some(p.version.clone(), |d, v| {
                        d.child(ui::chip(v, ui::info()).flex_shrink_0())
                    })
                    .children(
                        updates
                            .and_then(|status| {
                                crate::features::updates::status::package_status(
                                    &p.source,
                                    &status.packages.items,
                                )
                            })
                            .and_then(|item| {
                                let update = item.state
                                    == pi_rpc::desktop_rpc::PackageUpdateState::UpdateAvailable;
                                crate::features::updates::status::package_chip(item).map(|text| {
                                    let color = if update { ui::accent() } else { ui::muted() };
                                    ui::chip(text, color).flex_shrink_0()
                                })
                            }),
                    )
                    .child(
                        Button::new(("remove-package", i))
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(IconName::Trash))
                            .tooltip("gentle-shell remove")
                            .disabled(updating_shell)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.run_package("remove", source.clone(), cx)
                            })),
                    )
            }))
            .when(!s.extensions.is_empty(), |d| {
                d.child(row("Extension filters", s.extensions.join(", ")))
            })
            .child(
                h_flex()
                    .pt_3()
                    .gap_2()
                    .child(
                        ui::field()
                            .flex_1()
                            .min_w_0()
                            .child(Icon::new(IconName::Plus).small().text_color(ui::faint()))
                            .child(Input::new(&self.package.input).appearance(false).small()),
                    )
                    .child(
                        Button::new("install-package")
                            .primary()
                            .icon(Icon::new(IconName::ArrowDown))
                            .label("Install")
                            .disabled(self.package.running.is_some() || updating_shell)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let source = this.package.input.read(cx).value().trim().to_string();
                                if !source.is_empty() {
                                    this.package
                                        .input
                                        .update(cx, |s, cx| s.set_value("", window, cx));
                                    this.run_package("install", source, cx);
                                }
                            })),
                    ),
            )
            .when_some(self.package.running.clone(), |d, job| {
                d.child(
                    h_flex()
                        .pt_2()
                        .gap_2()
                        .text_xs()
                        .text_color(ui::muted())
                        .child(Spinner::new().xsmall().color(ui::accent()))
                        .child(format!("gentle-shell {job}…")),
                )
            })
            .when(!self.package.log.is_empty(), |d| {
                d.child(
                    ui::well_box().mt_2().px_3().py_2().gap_0p5().children(
                        self.package
                            .log
                            .iter()
                            .map(|l| div().text_color(ui::muted()).child(l.clone())),
                    ),
                )
            })
    }
}
