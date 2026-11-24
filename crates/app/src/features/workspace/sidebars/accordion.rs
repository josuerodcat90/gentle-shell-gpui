//! Accordion: every workspace is a group that opens in place, several at
//! once, with its tabs on an indent guide; the shown tab carries the accent
//! marker. A tab of another workspace opens it. While searching, every
//! workspace with a match opens.

use super::parts::INDENT;
use super::*;

impl Workspace {
    pub(in super::super) fn render_accordion_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let searching = self.searching(cx);
        let mut list = v_flex()
            .id("accordion-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_2()
            .pb_2()
            .gap_px()
            .child(self.workspace_section(cx));
        let query = self.tab_search.read(cx).value().trim().to_lowercase();
        let mut matches = 0;
        for space in self.sidebar_spaces() {
            let tabs = self.visible_tabs(space.index, cx);
            if searching && tabs.is_empty() && !space.name.to_lowercase().contains(&query) {
                continue;
            }
            matches += 1;
            let open = searching || self.sidebar_space_open(space.index);
            list = list.child(self.space_row(&space, Some(open), "accordion-space", cx));
            if !open {
                continue;
            }
            // The indent guide: a tree's, under the workspace's chevron.
            let mut guide = v_flex()
                .flex_none()
                .ml(rems(10. / 16.))
                .pl(rems(4. / 16.))
                .border_l_1()
                .border_color(ui::border().opacity(0.6))
                .gap_px();
            if tabs.is_empty() {
                guide = guide.child(Self::quiet_row("No sessions yet", INDENT - 14.));
            }
            for tab in &tabs {
                guide = guide.child(self.tab_row(tab, INDENT - 14., cx));
                if tab.panes > 1 {
                    guide = guide.children(self.split_sidebar_rows(tab, cx));
                }
            }
            list = list.child(guide);
        }
        if searching && matches == 0 {
            list = list.child(Self::quiet_row("No matching workspaces or sessions", 0.));
        }
        let actions = self.header_actions(cx);
        v_flex()
            .size_full()
            .child(self.sidebar_header(actions, cx))
            .child(self.sidebar_search())
            .child(list)
            .child(self.global_activity_row())
            .child(self.new_tab_tiles(("sidebar-add-agent", "sidebar-add-terminal"), cx))
            .into_any_element()
    }
}
