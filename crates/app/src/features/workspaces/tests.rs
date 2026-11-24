use std::path::PathBuf;

use super::store::{load_from, save_to};
use super::{
    Badge, BadgeColor, BadgeIcon, SavedPane, SavedState, SavedTab, SavedWorkspace, TabLayout,
    VERSION,
};
use crate::shared::pane::{PaneKind, PaneTree, SplitAxis};

fn sample() -> SavedState {
    let agent = SavedPane {
        kind: PaneKind::Agent,
        cwd: PathBuf::from("/tmp/project"),
        path: None,
        session: Some(PathBuf::from("/tmp/sessions/a.jsonl")),
    };
    let terminal = SavedPane {
        kind: PaneKind::Terminal,
        cwd: PathBuf::from("/tmp/project"),
        path: None,
        session: None,
    };
    let file = SavedPane {
        kind: PaneKind::File,
        cwd: PathBuf::from("/tmp/project"),
        path: Some(PathBuf::from("/tmp/project/README.md")),
        session: None,
    };
    SavedState {
        version: VERSION,
        current: 1,
        workspaces: vec![
            SavedWorkspace {
                name: "Default".into(),
                root: PathBuf::from("/tmp"),
                tabs: vec![SavedTab {
                    layout: PaneTree::Leaf(agent.clone()),
                    pinned: true,
                    focused: 0,
                }],
                active_tab: 0,
                last_active: 10,
                badge: Badge::default(),
                sidebar_expanded: true,
            },
            SavedWorkspace {
                name: "project".into(),
                root: PathBuf::from("/tmp/project"),
                tabs: vec![SavedTab {
                    pinned: false,
                    layout: PaneTree::Split {
                        axis: SplitAxis::Horizontal,
                        ratio: 0.4,
                        first: Box::new(PaneTree::Leaf(agent)),
                        second: Box::new(PaneTree::Split {
                            axis: SplitAxis::Vertical,
                            ratio: 0.5,
                            first: Box::new(PaneTree::Leaf(terminal)),
                            second: Box::new(PaneTree::Leaf(file)),
                        }),
                    },
                    focused: 2,
                }],
                active_tab: 0,
                last_active: 20,
                sidebar_expanded: false,
                badge: Badge {
                    icon: Some(BadgeIcon::Rocket),
                    color: Some(BadgeColor::Teal),
                },
            },
        ],
        sidebar: false,
        tab_layout: TabLayout::Vertical,
        sidebar_compact: true,
        tabs_sidebar_width: 300.,
        files_sidebar_width: 350.,
    }
}

/// An icon or color saved by a newer build reads as unset, keeping the rest
/// of the badge and the workspace.
#[test]
fn unknown_badge_values_read_as_unset() {
    let mut json = serde_json::to_value(sample()).unwrap();
    json["workspaces"][1]["badge"]["icon"] = "unicorn".into();
    let state: SavedState = serde_json::from_value(json).unwrap();
    assert_eq!(
        state.workspaces[1].badge,
        Badge {
            icon: None,
            color: Some(BadgeColor::Teal),
        }
    );
    // A workspace without a chosen badge doesn't write one.
    let plain = serde_json::to_value(&state.workspaces[0]).unwrap();
    assert!(plain.get("badge").is_none());
}

#[test]
fn json_round_trip() {
    let state = sample();
    let json = serde_json::to_string(&state).unwrap();
    let back: SavedState = serde_json::from_str(&json).unwrap();
    assert_eq!(back, state);
    // Optional fields stay out of the file and default back in.
    assert!(!json.contains("\"path\":null"));
    let minimal: SavedState =
        serde_json::from_str(r#"{"workspaces":[{"name":"x","root":"/x"}]}"#).unwrap();
    assert_eq!(minimal.workspaces[0].tabs.len(), 0);
    assert_eq!(minimal.current, 0);
    // A file from before the sidebar was saved keeps it showing.
    assert!(minimal.sidebar);
    assert!(minimal.workspaces[0].sidebar_expanded);
    assert_eq!(minimal.tab_layout, TabLayout::Horizontal);
    assert!(!minimal.sidebar_compact);
    assert!(back.sidebar_compact);
    assert!(minimal.workspaces[0].tabs.is_empty());
    assert!(back.workspaces[0].tabs[0].pinned);
    // Both sidebar widths round-trip and legacy files default to 260.
    assert_eq!(back.tabs_sidebar_width, 300.);
    assert_eq!(back.files_sidebar_width, 350.);
    assert_eq!(
        minimal.tabs_sidebar_width,
        gentle_core::workspaces::SIDEBAR_DEFAULT_WIDTH
    );
    assert_eq!(
        minimal.files_sidebar_width,
        gentle_core::workspaces::SIDEBAR_DEFAULT_WIDTH
    );

    // Data saved before this feature must still load without layout or pin fields.
    let mut legacy = serde_json::to_value(&state).unwrap();
    legacy.as_object_mut().unwrap().remove("tab_layout");
    legacy.as_object_mut().unwrap().remove("tabs_sidebar_width");
    legacy
        .as_object_mut()
        .unwrap()
        .remove("files_sidebar_width");
    legacy["workspaces"][0]["tabs"][0]
        .as_object_mut()
        .unwrap()
        .remove("pinned");
    let old: SavedState = serde_json::from_value(legacy).unwrap();
    assert_eq!(old.tab_layout, TabLayout::Horizontal);
    assert!(!old.workspaces[0].tabs[0].pinned);
    assert_eq!(old.tabs_sidebar_width, 260.);
    assert_eq!(old.files_sidebar_width, 260.);
    assert_eq!(SavedState::default().sidebar, minimal.sidebar);
}

/// The tabs sidebar's width saved as `sidebar_width` (before the Files
/// sidebar could be resized) still loads, within the current limits.
#[test]
fn legacy_sidebar_width_key_loads_as_the_tabs_width() {
    use gentle_core::workspaces::{SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH, saved_sidebar_width};

    let legacy = |width: f32| -> SavedState {
        let mut json = serde_json::to_value(sample()).unwrap();
        let object = json.as_object_mut().unwrap();
        object.remove("tabs_sidebar_width");
        object.insert("sidebar_width".into(), width.into());
        serde_json::from_value(json).unwrap()
    };
    let old = legacy(340.);
    assert_eq!(old.tabs_sidebar_width, 340.);
    assert_eq!(old.files_sidebar_width, 350.);
    assert!(old.sidebar_compact);
    // The older build allowed 180–600 px; the app holds it to today's range.
    assert_eq!(
        saved_sidebar_width(legacy(180.).tabs_sidebar_width),
        SIDEBAR_MIN_WIDTH
    );
    assert_eq!(
        saved_sidebar_width(legacy(600.).tabs_sidebar_width),
        SIDEBAR_MAX_WIDTH
    );
    // Saving again writes only the current key.
    let json = serde_json::to_value(&old).unwrap();
    assert!(json.get("sidebar_width").is_none());
    assert_eq!(json["tabs_sidebar_width"], 340.);
}

#[test]
fn file_round_trip() {
    let dir = std::env::temp_dir().join(format!("gentle-ws-test-{}", std::process::id()));
    let path = dir.join("nested").join("workspaces.json");
    let state = sample();
    save_to(&path, &state).unwrap();
    assert_eq!(load_from(&path), Some(state));
    std::fs::write(&path, "not json").unwrap();
    assert_eq!(load_from(&path), None);
    let _ = std::fs::remove_dir_all(dir);
}

/// A newer build saved a pane kind this one doesn't know (as the History
/// pane was for older builds): the file still loads with every workspace,
/// and only that pane goes.
#[test]
fn unknown_pane_kinds_drop_only_those_panes() {
    let dir = std::env::temp_dir().join(format!("gentle-ws-unknown-{}", std::process::id()));
    let path = dir.join("workspaces.json");
    std::fs::create_dir_all(&dir).unwrap();
    let mut json = serde_json::to_value(sample()).unwrap();
    // Workspace 1: the file pane (second of the inner split) becomes unknown.
    json["workspaces"][1]["tabs"][0]["layout"]["split"]["second"]["split"]["second"]["leaf"]["kind"] =
        "from_a_newer_build".into();
    // Workspace 0: its only tab is unknown and goes; a second tab stays.
    json["workspaces"][0]["tabs"][0]["layout"]["leaf"]["kind"] = "from_a_newer_build".into();
    let kept = serde_json::to_value(&sample().workspaces[0].tabs[0]).unwrap();
    json["workspaces"][0]["tabs"]
        .as_array_mut()
        .unwrap()
        .push(kept);
    json["workspaces"][0]["active_tab"] = 1.into();
    std::fs::write(&path, json.to_string()).unwrap();

    let state = load_from(&path).expect("loads despite the unknown panes");
    assert_eq!(state.workspaces.len(), 2);
    assert_eq!(state.workspaces[0].tabs.len(), 1);
    assert_eq!(state.workspaces[0].active_tab, 0);
    let tab = &state.workspaces[1].tabs[0];
    let kinds: Vec<PaneKind> = tab.layout.leaves().iter().map(|p| p.kind).collect();
    assert_eq!(kinds, [PaneKind::Agent, PaneKind::Terminal]);
    // The focused pane was the dropped one: focus falls back to the first.
    assert_eq!(tab.focused, 0);
    assert!(!dir.join("workspaces.json.unreadable").exists());
    let _ = std::fs::remove_dir_all(dir);
}

/// A file that doesn't read at all is kept aside before the next save
/// replaces it.
#[test]
fn an_unreadable_file_is_copied_aside() {
    let dir = std::env::temp_dir().join(format!("gentle-ws-unreadable-{}", std::process::id()));
    let path = dir.join("workspaces.json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&path, "{ not json").unwrap();
    assert_eq!(load_from(&path), None);
    assert_eq!(
        std::fs::read_to_string(dir.join("workspaces.json.unreadable")).unwrap(),
        "{ not json"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_folder_reached_another_way_keys_the_same_workspace() {
    let base = std::env::temp_dir().join(format!("ws-key-test-{}", std::process::id()));
    std::fs::create_dir_all(base.join("a")).unwrap();
    let canonical = dunce::canonicalize(base.join("a")).unwrap();
    // The explorer builds `dir.join(name)`; `gsd` passes a canonical path.
    assert_eq!(
        super::root_key(base.join("a").join("..").join("a")),
        canonical
    );
    assert_eq!(super::root_key(canonical.clone()), canonical);
    // A folder that can't be resolved keeps its path.
    let missing = base.join("missing");
    assert_eq!(super::root_key(missing.clone()), missing);
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn a_workspace_saved_with_a_non_canonical_root_is_still_found() {
    let base = std::env::temp_dir().join(format!("ws-find-test-{}", std::process::id()));
    std::fs::create_dir_all(base.join("a")).unwrap();
    std::fs::create_dir_all(base.join("b")).unwrap();
    // Saved before roots were canonical: `a/../a`.
    let roots = [base.join("b"), base.join("a").join("..").join("a")];
    let canonical = dunce::canonicalize(base.join("a")).unwrap();
    assert_eq!(super::find_root(roots.iter(), &canonical), Some(1));
    assert_eq!(super::find_root(roots.iter(), &base.join("missing")), None);
    std::fs::remove_dir_all(&base).unwrap();
}
