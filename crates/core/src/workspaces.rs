//! Pure rules for named workspaces: picking a name nobody else uses,
//! multi-selecting tabs, and reordering workspaces.

use std::collections::HashSet;

/// `base`, or `base 2`, `base 3`… when taken.
pub fn unique_name<'a>(taken: impl IntoIterator<Item = &'a str> + Clone, base: &str) -> String {
    let base = base.trim();
    let base = if base.is_empty() { "workspace" } else { base };
    let free = |name: &str| {
        !taken
            .clone()
            .into_iter()
            .any(|t| t.eq_ignore_ascii_case(name))
    };
    if free(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|name| free(name))
        .expect("an unused name")
}

/// Result of applying a tab click to a multi-selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionUpdate {
    /// Selected tab ids, in no particular order.
    pub selected: Vec<usize>,
    /// The anchor id for the next range selection.
    pub anchor: Option<usize>,
    /// Whether the click should also activate the clicked tab.
    pub activate: bool,
}

/// Apply a tab click to the current selection.
///
/// `tabs` are the tab ids in display order; `selected` are the currently
/// selected ids; `anchor` is the last ordinary or toggle click; `clicked`
/// is the tab that was clicked. `platform_modifier` is Cmd on macOS or
/// Ctrl elsewhere; `shift` is Shift.
///
/// Ordinary click clears the selection and activates the tab. The platform
/// modifier toggles the clicked tab. Shift selects the range from the
/// anchor to the clicked tab. Shift takes precedence over the platform
/// modifier when both are held.
pub fn select_tab(
    tabs: &[usize],
    selected: &[usize],
    anchor: Option<usize>,
    clicked: usize,
    platform_modifier: bool,
    shift: bool,
) -> SelectionUpdate {
    if shift {
        // An anchor that is not among the tabs shown (filtered out, closed)
        // falls back to the clicked tab.
        let anchor = anchor.filter(|a| tabs.contains(a)).unwrap_or(clicked);
        let range = range_between(tabs, anchor, clicked);
        SelectionUpdate {
            selected: range,
            anchor: Some(anchor),
            activate: false,
        }
    } else if platform_modifier {
        let mut new_selected: Vec<usize> = selected.to_vec();
        if new_selected.contains(&clicked) {
            new_selected.retain(|&id| id != clicked);
        } else {
            new_selected.push(clicked);
        }
        SelectionUpdate {
            selected: new_selected,
            anchor: Some(clicked),
            activate: false,
        }
    } else {
        SelectionUpdate {
            selected: Vec::new(),
            anchor: Some(clicked),
            activate: true,
        }
    }
}

fn range_between(tabs: &[usize], from: usize, to: usize) -> Vec<usize> {
    let Some(from_idx) = tabs.iter().position(|&id| id == from) else {
        return Vec::new();
    };
    let Some(to_idx) = tabs.iter().position(|&id| id == to) else {
        return Vec::new();
    };
    let (start, end) = if from_idx <= to_idx {
        (from_idx, to_idx)
    } else {
        (to_idx, from_idx)
    };
    tabs[start..=end].to_vec()
}

/// Decide the new pinned state for a multi-selection.
///
/// If any selected tab is unpinned, every selected tab is pinned; otherwise
/// every selected tab is unpinned. Returns `None` when the selection is empty.
pub fn pin_state_for_selection(
    is_pinned: impl Fn(usize) -> bool,
    selected: &[usize],
) -> Option<bool> {
    if selected.is_empty() {
        return None;
    }
    Some(selected.iter().any(|&id| !is_pinned(id)))
}

/// The active tab index after closing tabs at `removed` indices.
///
/// `len` is the original number of tabs; `active` is the original active
/// index. `removed` must contain distinct indices in any order.
pub fn active_after_removals(len: usize, active: usize, removed: &[usize]) -> usize {
    let removed_set: HashSet<usize> = removed.iter().copied().collect();
    let removed_before = removed_set.iter().filter(|&&idx| idx < active).count();
    let remaining = len.saturating_sub(removed_set.len());
    active
        .saturating_sub(removed_before)
        .min(remaining.saturating_sub(1))
}

/// What a left click on a tab does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabClick {
    /// Show the tab (and keep it as the range anchor).
    Activate,
    /// Start renaming the tab (plain double-click).
    Rename,
    /// Toggle or range-select; never activates.
    Select,
    /// Nothing: the second click of a modified double-click.
    Ignore,
}

/// Decide a tab click from its count and modifiers. Modifiers win over the
/// click count, so a Cmd/Ctrl/Shift double-click never starts a rename.
pub fn tab_click_action(click_count: usize, platform_modifier: bool, shift: bool) -> TabClick {
    match (platform_modifier || shift, click_count) {
        (true, 0 | 1) => TabClick::Select,
        (true, _) => TabClick::Ignore,
        (false, 0 | 1) => TabClick::Activate,
        (false, _) => TabClick::Rename,
    }
}

/// Return the indices of tabs removed in one batch from highest to lowest:
/// removing in this order keeps every remaining index valid, and pushing
/// closes onto the reopen stack in this order makes the lowest index pop
/// (reopen) first.
pub fn close_stack_order(indices: &[usize]) -> Vec<usize> {
    let mut sorted = indices.to_vec();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    sorted
}

/// Adjust a workspace index after another workspace is deleted.
/// Returns `None` when the indexed state referred to the deleted workspace.
pub fn adjust_workspace_index_after_delete(index: usize, deleted: usize) -> Option<usize> {
    if index == deleted {
        None
    } else if index > deleted {
        Some(index - 1)
    } else {
        Some(index)
    }
}

/// Reorder workspaces by moving the workspace at `from` to `to`.
///
/// Returns a vector where position `new_index` holds the old index that now
/// sits there.
pub fn reorder_workspaces(count: usize, from: usize, to: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..count).collect();
    if from != to && from < count && to < count {
        let item = order.remove(from);
        order.insert(to, item);
    }
    order
}

/// Map old workspace indices to their new indices from a reordering.
///
/// `order[new_index] == old_index`. The returned vector satisfies
/// `remap[old_index] == new_index`.
pub fn workspace_index_remap(order: &[usize]) -> Vec<usize> {
    let mut remap = vec![0; order.len()];
    for (new_idx, old_idx) in order.iter().enumerate() {
        remap[*old_idx] = new_idx;
    }
    remap
}

/// A sidebar's width (px at 100 % interface size) before anyone drags it.
pub const SIDEBAR_DEFAULT_WIDTH: f32 = 260.;
/// Narrower than this, a sidebar's header no longer fits.
pub const SIDEBAR_MIN_WIDTH: f32 = 200.;
pub const SIDEBAR_MAX_WIDTH: f32 = 480.;
/// The share of the window a sidebar may take at most.
pub const SIDEBAR_MAX_SHARE: f32 = 0.4;
/// Dragged narrower than this, a sidebar collapses; between this and the
/// minimum it holds at the minimum, so a resize doesn't hide it by accident.
pub const SIDEBAR_COLLAPSE_BELOW: f32 = 140.;

/// A saved width as the app keeps it: between the minimum and the maximum,
/// the default when it isn't a number.
pub fn saved_sidebar_width(width: f32) -> f32 {
    if !width.is_finite() {
        return SIDEBAR_DEFAULT_WIDTH;
    }
    width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH)
}

/// The width a sidebar shows at in a `window` this wide: its saved width,
/// held to the window's share (and never below the minimum). The saved width
/// stays, so a wider window gives it back.
pub fn shown_sidebar_width(saved: f32, window: f32) -> f32 {
    let cap = (window * SIDEBAR_MAX_SHARE).clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
    saved_sidebar_width(saved).min(cap)
}

/// What dragging a sidebar's inner edge does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SidebarDrag {
    /// The sidebar takes this width.
    Width(f32),
    /// Dragged past the collapse point: the sidebar hides.
    Collapse,
    /// The same held drag moved back to the minimum usable width.
    Reopen,
}

/// Dragging a sidebar's edge to `pointer`, its distance from the sidebar's
/// outer edge, in a `window` this wide.
pub fn drag_sidebar_width(pointer: f32, window: f32) -> SidebarDrag {
    if pointer < SIDEBAR_COLLAPSE_BELOW {
        SidebarDrag::Collapse
    } else {
        SidebarDrag::Width(shown_sidebar_width(pointer, window))
    }
}

/// State owned by the active drag, not by the sidebar.
#[derive(Clone, Default)]
pub struct SidebarGesture {
    collapsed: bool,
    ended: bool,
}

impl SidebarGesture {
    pub fn update(
        &mut self,
        pointer: f32,
        window: f32,
        visible: bool,
        left_held: bool,
    ) -> Option<SidebarDrag> {
        // A later left press must not revive a stale drag payload.
        self.ended |= !left_held;
        if self.ended {
            return None;
        }
        if visible {
            let action = drag_sidebar_width(pointer, window);
            self.collapsed = action == SidebarDrag::Collapse;
            Some(action)
        } else if self.collapsed && pointer >= SIDEBAR_MIN_WIDTH {
            self.collapsed = false;
            Some(SidebarDrag::Reopen)
        } else {
            None
        }
    }
}

/// A reversible ease-out-quint visibility transition, independent of layout width.
#[derive(Clone, Copy)]
pub struct SidebarSlide {
    from: f32,
    to: f32,
}

impl SidebarSlide {
    pub const SECONDS: f32 = 0.22;

    pub fn new(from: f32, opening: bool) -> Self {
        Self {
            from,
            to: if opening { 1. } else { 0. },
        }
    }

    pub fn fraction(self, elapsed: f32) -> f32 {
        let t = (elapsed / Self::SECONDS).clamp(0., 1.);
        if t == 0. {
            return self.from;
        }
        if t == 1. {
            return self.to;
        }
        self.to + (self.from - self.to) * (1. - t).powi(5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_slide_reverses_from_its_current_width_and_lands_exactly() {
        let closing = SidebarSlide::new(1., false);
        assert_eq!(closing.fraction(0.), 1.);
        let midway = closing.fraction(0.03);
        assert!(midway > 0. && midway < 1.);
        let opening = SidebarSlide::new(midway, true);
        assert_eq!(opening.fraction(0.), midway);
        assert!(opening.fraction(0.06) > midway);
        assert_eq!(opening.fraction(0.22), 1.);
        assert_eq!(closing.fraction(0.22), 0.);
    }

    #[test]
    fn sidebar_drag_without_left_held_cannot_resume() {
        let mut drag = SidebarGesture::default();
        assert_eq!(
            drag.update(139., 1000., true, true),
            Some(SidebarDrag::Collapse)
        );
        assert_eq!(drag.update(240., 1000., false, false), None);
        assert_eq!(drag.update(240., 1000., false, true), None);
        assert_eq!(drag.update(240., 1000., true, true), None);
    }

    #[test]
    fn sidebar_drag_reopens_only_a_collapse_in_the_same_gesture() {
        let mut held = SidebarGesture::default();
        assert_eq!(
            held.update(139., 1000., true, true),
            Some(SidebarDrag::Collapse)
        );
        assert_eq!(held.update(140., 1000., false, true), None);
        assert_eq!(held.update(199., 1000., false, true), None);
        assert_eq!(
            SidebarGesture::default().update(240., 1000., false, true),
            None
        );
        assert_eq!(
            held.update(200., 1000., false, true),
            Some(SidebarDrag::Reopen)
        );
        assert_eq!(
            held.update(450., 1000., true, true),
            Some(SidebarDrag::Width(400.))
        );
    }

    #[test]
    fn a_saved_sidebar_width_stays_between_the_minimum_and_the_maximum() {
        assert_eq!(saved_sidebar_width(320.), 320.);
        assert_eq!(saved_sidebar_width(120.), SIDEBAR_MIN_WIDTH);
        assert_eq!(saved_sidebar_width(900.), SIDEBAR_MAX_WIDTH);
        assert_eq!(saved_sidebar_width(f32::NAN), SIDEBAR_DEFAULT_WIDTH);
        assert_eq!(saved_sidebar_width(f32::INFINITY), SIDEBAR_DEFAULT_WIDTH);
    }

    #[test]
    fn a_sidebar_takes_at_most_40_percent_of_the_window() {
        // Wide window: the saved width shows as is.
        assert_eq!(shown_sidebar_width(400., 1600.), 400.);
        // 1000 px window: 40 % is 400.
        assert_eq!(shown_sidebar_width(480., 1000.), 400.);
        // Never narrower than the minimum, however small the window.
        assert_eq!(shown_sidebar_width(300., 300.), SIDEBAR_MIN_WIDTH);
        assert_eq!(shown_sidebar_width(f32::NAN, 1600.), SIDEBAR_DEFAULT_WIDTH);
    }

    #[test]
    fn dragging_a_sidebar_edge_resizes_it_or_collapses_it() {
        assert_eq!(drag_sidebar_width(330., 1600.), SidebarDrag::Width(330.));
        // Past the maximum or the window share, it stops there.
        assert_eq!(
            drag_sidebar_width(700., 1600.),
            SidebarDrag::Width(SIDEBAR_MAX_WIDTH)
        );
        assert_eq!(drag_sidebar_width(450., 1000.), SidebarDrag::Width(400.));
        // Between the collapse point and the minimum it holds at the minimum.
        assert_eq!(
            drag_sidebar_width(SIDEBAR_COLLAPSE_BELOW, 1600.),
            SidebarDrag::Width(SIDEBAR_MIN_WIDTH)
        );
        assert_eq!(
            drag_sidebar_width(SIDEBAR_COLLAPSE_BELOW - 1., 1600.),
            SidebarDrag::Collapse
        );
        assert_eq!(drag_sidebar_width(-20., 1600.), SidebarDrag::Collapse);
    }

    #[test]
    fn names_stay_unique() {
        let taken = ["Default", "app", "app 2"];
        assert_eq!(unique_name(taken, "new"), "new");
        assert_eq!(unique_name(taken, "App"), "App 3");
        assert_eq!(unique_name(taken, "  "), "workspace");
    }

    #[test]
    fn ordinary_click_clears_selection_and_activates() {
        let update = select_tab(&[1, 2, 3], &[1, 3], Some(1), 2, false, false);
        assert_eq!(update.selected, Vec::<usize>::new());
        assert_eq!(update.anchor, Some(2));
        assert!(update.activate);
    }

    #[test]
    fn platform_modifier_toggles_a_tab() {
        let add = select_tab(&[1, 2, 3], &[1], Some(1), 3, true, false);
        assert_eq!(add.selected, vec![1, 3]);
        assert_eq!(add.anchor, Some(3));
        assert!(!add.activate);

        let remove = select_tab(&[1, 2, 3], &[1, 3], Some(3), 1, true, false);
        assert_eq!(remove.selected, vec![3]);
        assert_eq!(remove.anchor, Some(1));
    }

    #[test]
    fn shift_selects_range_from_anchor() {
        let forward = select_tab(&[10, 20, 30, 40], &[10], Some(10), 30, false, true);
        assert_eq!(forward.selected, vec![10, 20, 30]);
        assert_eq!(forward.anchor, Some(10));
        assert!(!forward.activate);

        let backward = select_tab(&[10, 20, 30, 40], &[40], Some(30), 10, false, true);
        assert_eq!(backward.selected, vec![10, 20, 30]);
        assert_eq!(backward.anchor, Some(30));
    }

    #[test]
    fn shift_without_anchor_selects_clicked_only() {
        let update = select_tab(&[1, 2, 3], &[], None, 2, false, true);
        assert_eq!(update.selected, vec![2]);
        assert_eq!(update.anchor, Some(2));
    }

    #[test]
    fn shift_takes_precedence_over_platform_modifier() {
        let update = select_tab(&[1, 2, 3, 4], &[1], Some(1), 3, true, true);
        assert_eq!(update.selected, vec![1, 2, 3]);
        assert!(!update.activate);
    }

    #[test]
    fn pin_state_pins_when_any_unpinned() {
        assert_eq!(pin_state_for_selection(|_| true, &[1, 2]), Some(false));
        assert_eq!(pin_state_for_selection(|id| id == 1, &[1, 2]), Some(true));
        assert_eq!(pin_state_for_selection(|_| false, &[1]), Some(true));
        assert_eq!(pin_state_for_selection(|_| true, &[]), None);
    }

    #[test]
    fn active_after_removals_keeps_or_clamps_active() {
        // Active after removed tabs: unchanged.
        assert_eq!(active_after_removals(5, 3, &[0, 1]), 1);
        // Active before removed tabs: unchanged.
        assert_eq!(active_after_removals(5, 0, &[2, 3]), 0);
        // Active removed: selects the previous surviving tab.
        assert_eq!(active_after_removals(4, 3, &[2, 3]), 1);
        // Last tab removed: clamps.
        assert_eq!(active_after_removals(2, 1, &[1]), 0);
    }

    #[test]
    fn close_stack_order_is_descending() {
        assert_eq!(close_stack_order(&[1, 5, 2]), vec![5, 2, 1]);
        assert_eq!(close_stack_order(&[0]), vec![0]);
    }

    #[test]
    fn modifiers_win_over_click_count() {
        use TabClick::*;
        assert_eq!(tab_click_action(1, false, false), Activate);
        assert_eq!(tab_click_action(2, false, false), Rename);
        assert_eq!(tab_click_action(1, true, false), Select);
        assert_eq!(tab_click_action(1, false, true), Select);
        assert_eq!(tab_click_action(2, true, false), Ignore);
        assert_eq!(tab_click_action(2, false, true), Ignore);
        assert_eq!(tab_click_action(3, true, true), Ignore);
    }

    #[test]
    fn shift_with_a_hidden_anchor_falls_back_to_the_clicked_tab() {
        // The anchor (99) is not among the tabs shown (filtered by search).
        let update = select_tab(&[1, 2, 3], &[], Some(99), 2, false, true);
        assert_eq!(update.selected, vec![2]);
        assert_eq!(update.anchor, Some(2));
    }

    #[test]
    fn shift_after_an_ordinary_click_selects_from_that_tab() {
        let tabs = [1, 2, 3, 4];
        let click = select_tab(&tabs, &[], None, 2, false, false);
        assert!(click.activate);
        let range = select_tab(&tabs, &click.selected, click.anchor, 4, false, true);
        assert_eq!(range.selected, vec![2, 3, 4]);
    }

    #[test]
    fn adjust_workspace_index_after_delete_shifts_or_drops() {
        assert_eq!(adjust_workspace_index_after_delete(0, 0), None);
        assert_eq!(adjust_workspace_index_after_delete(1, 0), Some(0));
        assert_eq!(adjust_workspace_index_after_delete(2, 1), Some(1));
        assert_eq!(adjust_workspace_index_after_delete(0, 2), Some(0));
    }

    #[test]
    fn reorder_workspaces_moves_and_keeps_others() {
        assert_eq!(reorder_workspaces(5, 0, 3), vec![1, 2, 3, 0, 4]);
        assert_eq!(reorder_workspaces(5, 3, 0), vec![3, 0, 1, 2, 4]);
        assert_eq!(reorder_workspaces(3, 1, 1), vec![0, 1, 2]);
        assert_eq!(reorder_workspaces(3, 5, 0), vec![0, 1, 2]);
    }

    #[test]
    fn workspace_index_remap_inverts_order() {
        let order = vec![1, 2, 3, 0, 4];
        let remap = workspace_index_remap(&order);
        assert_eq!(remap, vec![3, 0, 1, 2, 4]);
        // Verify round-trip: order[new] should map back through remap.
        for (new, old) in order.iter().enumerate() {
            assert_eq!(remap[*old], new);
        }
    }
}
