//! Application, Gentle Shell, Pi and Engram update controls.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::features::updates::status::{main_update_target, pi_update_available, status_rows};

use super::super::{
    SettingsView, app,
    layout::{row, section},
    snapshot::Snapshot,
    upgrade,
};

pub(super) fn render(
    view: &SettingsView,
    snapshot: &Snapshot,
    cx: &mut Context<SettingsView>,
) -> Div {
    v_flex()
        .gap_3()
        .child(app::app_section(cx))
        .child(backend_updates_section(view, snapshot, cx))
}

fn backend_updates_section(
    view: &SettingsView,
    snapshot: &Snapshot,
    cx: &mut Context<SettingsView>,
) -> Div {
    let status = view.shell_updates.status.as_ref();
    let main = view.shell_update.main_commit(
        status.and_then(|status| main_update_target(&status.gentle_shell, snapshot.managed)),
    );
    let pi = status.is_some_and(|status| pi_update_available(status, snapshot.managed));
    section(IconName::RefreshCw, "Gentle Shell and Engram")
        .child(row("Versions", snapshot.versions.join(" · ")))
        .children(
            status
                .into_iter()
                .flat_map(|status| status_rows(status, snapshot.managed))
                .map(|(label, text)| row(label, text)),
        )
        .when_some(view.shell_reports_updates(), |card, checking| {
            card.child(
                h_flex().pt_2().child(
                    Button::new("shell-update-check")
                        .small()
                        .ghost()
                        .icon(Icon::new(IconName::RefreshCw))
                        .label(if view.waiting_for_tab() {
                            "Waiting for the agent tab to start…"
                        } else if checking {
                            "Checking Gentle Shell for updates…"
                        } else {
                            "Check for updates"
                        })
                        .loading(checking)
                        .disabled(checking)
                        .on_click(cx.listener(|this, _, _, cx| this.reload_with(true, cx))),
                ),
            )
        })
        .when_some(view.update_offer(snapshot), |card, offer| {
            card.child(view.render_update(offer, cx))
        })
        .when_some(main, |card, commit| {
            card.child(view.render_main_update(commit, cx))
        })
        .children(
            view.shell_update
                .result
                .as_ref()
                .map(upgrade::upgrade_result),
        )
        .children(view.render_pi_update(pi, cx))
        .child(view.render_engram_update(snapshot.engram_update.clone(), cx))
}
