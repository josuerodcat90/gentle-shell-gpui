//! Model defaults, profile and provider inventory, and an installed Claude
//! bridge.

use gpui_kit::component::v_flex;
use gpui_kit::*;

use super::super::{SettingsView, models, snapshot::Snapshot};

pub(super) fn render(
    view: &SettingsView,
    snapshot: &Snapshot,
    cx: &mut Context<SettingsView>,
) -> Div {
    v_flex()
        .gap_3()
        .child(models::models_section(snapshot))
        .child(models::providers_section(snapshot, cx))
        .children(view.bridge_section(snapshot, cx))
}
