# Architecture

Gentle Shell GPUI is a native desktop application written in Rust. Its architecture is a **modular monolith by feature**: it ships as a single application, but keeps explicit boundaries between the UI, pure logic and external integrations.

## Goals

- Preserve observable behavior during refactors.
- Give each feature a small public API and private internal details.
- Separate pure rules and transformations from GPUI, processes, files and operating system APIs.
- Avoid abstractions without a current need: no crate per screen, and no trait with a single implementation just to simulate decoupling.
- Allow deterministic tests of state, transitions, parsers and edge cases.

## Current components

```mermaid
flowchart TD
    Main["app: startup and composition"] --> Features["GPUI features"]
    Features --> State["Feature state"]
    Features --> Core["core: pure logic"]
    State --> Core
    Features --> Rpc["pi-rpc: process and JSONL protocol"]
    Features --> Platform["Platform: Git, terminal, files, browser and updates"]
    Features --> Shared["Shared UI, themes and assets"]
    Rpc --> Backend["Gentle Shell"]
    Platform --> OS["Operating system"]
```

| Component | Responsibility |
| --- | --- |
| `crates/app` | Binary, composition, feature state, GPUI views, and the effects (files, processes, sound) of the logic that lives in `core`. |
| `crates/core` (`gentle-core`) | Pure logic with no GPUI or IO: transcript and subagent activity models, tool summaries, permission mode, jobs and ODD phase, `/` command routing, `notifications.json` policy, usage and statistics models, and the profile and model routing model. |
| `crates/pi-rpc` | Backend resolution and launch, JSONL transport, authentication and protocol types. |
| `packaging` | Packaging and distribution integration per operating system. |

`crates/pi-rpc` is a stable boundary: it knows nothing about GPUI, windows, tabs or visual components. `crates/app` depends on it, never the other way around.

`crates/core` depends on nothing in the project (neither `app` nor `pi-rpc`) and not on GPUI; its only dependencies are `serde`, `serde_json` and `regex`. It compiles the same on macOS, Windows and Linux: it contains no platform-specific code except the syntax of the policy's `file:` paths, chosen with `cfg!(windows)` at compile time. When a module mixed pure logic and effects, the pure part lives in `core`, and the effectful part stays in `app` and calls it:

| `core` | Stays in `app` |
| --- | --- |
| `transcript`, `activity`, `tools`, `commands` | `session/agents.rs`: `now_ms` (system clock). |
| `devices`: the device hub's list (plus AVDs it hasn't seen), launch-intent and device-command detection in agents' shell commands | `devices/`: the hub process and API, the panel (tabs, docked or floating, web viewer per device), `gentle-device`, the agents' instructions, SDK and simctl discovery. |
| `scroll`: row weights from content, the scroll estimate for rows not laid out yet, the steady thumb rule | `session/view/rail.rs` and `transcript_list.rs`: recording on-screen row heights, drawing the rail. |
| `music`: gentle-stream's `ctl --json` envelope (the player's state and its structured errors) and the cache folder `setup-samples` uses | `music/`: running `gentle-stream ctl` per request, polling, the ♪ popover; `setup/music.rs`: finding and installing the player. |
| `web`: http(s) URL extraction, loopback filtering and log punctuation, `same_page` (one browser card per page); `browser`: `gentle-browser` ops and arguments, targets, profiles and their store ids, how cards open (`CardLayout`: docked or floating, the docked column's width, a floating card's size; `browserCard` in the app's `settings.json`); `screen`: `gentle-screen` ops and arguments, key chords, which processes input may reach | `browser/` and session events: in-app browser card (floating, or docked in the window's right column with a draggable divider; `shared/floating.rs` holds the geometry, `Mode` and the dock's width rule shared with the device panel) and `WebSurface`, agents driving pages (`automation.rs`, `automation.js`, `cli.rs`, the `browser` tool); `workspace/stages.rs`: one card per page whoever opens it (the design canvas, `gentle-open`, a dev-server URL and `gentle-browser open` reuse the card already on that page); `screen/`: `gentle-screen` (`cli.rs`, the `screen` tool, the OS per platform); `platform/shims.rs` writes the `gentle-open`, `gentle-browser`, `gentle-device` and `gentle-screen` shims. |
| `gentle_state`: `PermissionMode`, jobs, phase inference | `shared/gentle_state.rs`: reading and saving `permission-mode.json` (the fallback for a Shell without Desktop RPC's permission slice). |
| `notifications`: `Alert`, policy validation and changes; `alert_timing`: `Gate` and `ContextLatch` | `alerts/`: file cache and writing, playback, bundled tones, notifying the window. |
| `double_interrupt`: when a second Ctrl+C closes a terminal pane (`WINDOW`, idle at both presses, key repeats and other keys) | `terminal/`: the keystrokes, the close hint and its timer; `platform/shell_idle.rs`: asking the OS whether the shell is idle at its prompt. |
| `text::first_line` | `shared/ui.rs`: the remaining presentation utilities. |
| `usage`: subscription window models and parsers (Codex, NaN), grouping, account claim; session records, statistics aggregation, ISO dates and civil days | `usage/`: reading `auth.json`, resolving keys (`$VAR`, `!command`), HTTP requests, walking and caching session files, clock and local time offset. |
| `profiles`: routing model, `profiles.json` format, pins, agent frontmatter, notes | `profiles/`: homes and agent discovery, atomic writes, saved and materialized routing, Pi settings, pin and worktree files, Make default. |
| `hydration`: the water reminder's settings (`hydration` in the app's `settings.json`) and when the next one is due | `hydration/`: the clock from launch, the timer, saving the setting, the drops animation. |
| `diagnostics`: a recording's lines (header, sample per tab and process tree), grouping a process table into the trees under the app, rates, file names and retention, and the report that ranks a recording | `diagnostics/`: the sampler (`sysinfo`, the tabs, writing the file), `--diagnostics-report`; `shared/diagnostics.rs`: the frame, render and poll counters every view calls. |
| `markdown`: where the Mermaid blocks are in a document; a sent message as the Markdown its bubble renders (`sent_message`: HTML fragments literal, line breaks kept; `trim_folded`) | `files/mermaid.rs`: drawing them with merman's CLI (installed on demand by `setup`), measured with the system sans font, light or dark theme, and swapping them for images in the file preview and the conversation (`session/view.rs`: `shown_markdown`, `markdown_view`), with the install banner where they can't be drawn yet; `session/view.rs`: the bubble (`BubbleText`, `user_bubble`), its images only from `data:` URLs and local files (`files::offline_image`). |
| `preview`: the file pane's size limits: what loads at once, what waits for Load anyway, what never loads, what shows without syntax colors | `files/viewer.rs`: reading files and running git off the UI thread, the notice and its Load anyway button, the *Plain text* chip. |
| `docs`: the skill's `docs-check` report, each document's standing and front-matter title | `docs/`: the Architecture docs screen, installing the skill (`gentle-shell install`), running `docs-check` with Node, listing `docs/`. |
| `fuzzy`: fuzzy scoring and file ranking | `palette/`: walking files, reading branches with git. |
| `prompt_history`: the composer's sent prompts and where ↑/↓ are in them (draft kept, seeded from a resumed conversation) | `session/submit.rs`: recording at send, the ↑/↓ keystrokes (first and last line, `/` menu first) and putting a prompt in the composer. |
| `backend`: where agents, terminals, git and Setup run (this computer, a WSL distro, a command), its `settings.json` value, the WSL distro a folder implies, host ↔ backend paths both ways, the launch and terminal argv, the login PATH probe and `wsl.exe --list --verbose` | `backend/`: reading and saving the choice, probing the backend, staging the app's files there, `git` and `gentle-shell` commands through it, the backend's homes, listing WSL's distros; `setup/remote.rs`: Setup's checks there; `settings/runs_on.rs`: the Configuration section. |
| `ides`: code editors and IDEs (VS Code and its forks, Zed, Sublime Text, Xcode, Visual Studio, Android Studio, JetBrains, …), where each installs per OS, versioned-folder matching and the launch command | `platform/ides.rs`: probing those folders and the PATH, launching the editor; `files/tree.rs` and `history/view.rs`: the ⋯ menus' "Open in …". |
| `web_browsers`: where agents' pages open (`agentPages` in the app's `settings.json`: inside the app, the default, or in the user's browser) and which browser (`externalBrowser`: the system's default or a catalogued one — Safari, Chrome, Chromium, Arc, Dia, Firefox, Zen, LibreWolf, Brave, Edge, Opera, Vivaldi, Orion, …), where each installs per OS, which choice applies, the http(s)-only URL check and the launch command (`open -a <App> <url>` on macOS, the browser's program with the URL elsewhere, never a shell) | `platform/browsers.rs`: probing those folders, the PATH and Flatpak exports (with `platform::ides`' lookup), starting the browser; `browser/external.rs`: the saved choices and opening a page outside the app (the card's ↗; falls back to the system's default when the chosen browser is gone); `workspace/stages.rs`: routing agents' pages (`gentle-open`, dev-server URLs, the design canvas; each picked-out URL opens outside once per window); `settings/browser.rs`: Configuration › Devices › Browser. |
| `sessions`: session summary, folder name from cwd, ordering, ISO dates and relative time | `sessions/`: finding and reading `.jsonl` files (line scanning is tied to reading), cache. |
| `git`: status parsers (v1 and v2), numstat, the Changes tab's groups (staged, unstaged, untracked, in that order), refs, stash, worktrees (and where a new one goes), submodules, ahead/behind; `log` records, the commit graph's lanes and the Git pane's filter; `activity` the Git pane's year of commits as a grid of days (weeks, shades, month labels); `ops` pull modes, reset modes, the command git stopped in (merge, rebase, cherry-pick, revert) and how to continue or abort it, a commit's changed files, rewording a summary; diff trimming | `git/`: running `git`, counting lines of untracked files, stage, commit, push and branch operations, reading the commits a scope reaches (`log.rs`) and the last year's for the activity strip (`mod.rs`), the Git pane's actions (`ops.rs`); `history/`: the Git pane and painting the graph. |
| `version`: stable versions, comparison and Gentle Shell update offer | `updates/`: launcher cache, npm query, clock. |
| `engram`: observation model and query encoding | `memory/`: HTTP requests to Engram. |
| `workspaces`: unique workspace names; tab multi-selection rules (toggle, range, pin decision, active index after multiple closes); workspace reordering and index remapping; sidebar widths (limits, window share, drag to collapse) | `workspaces/`: saved model (uses `PaneTree`), reading and writing `workspaces.json`; `workspace/tab_panel.rs`: tab placement, pinning, closing and multi-selection menus. |
| `genui`: the `gentle_ui` diagram spec (parsing, limits, normalization), its layered auto-layout and hit testing, the change list between two versions (diff, apply, human-readable sentences), the `/gentle-ui:event` command line and the Agent UI mode (`mode`: saved value, launch variable, mode command) | `genui/`: the Pi extension it ships (`gentle-ui.ts`, written by `extension.rs`), surfaces rebuilt from the transcript (`GenUi`), the diagram view (`DiagramView`: viewport, pointer and keyboard edits, undo, send) and its painting; `mode.rs`: the Agent UI choice every tab follows. |
| `canvas`: canvas tabs' whiteboard model after Tern's canvas blocks (shapes, Tern's 16-color palette, fill, pen types and sizes, which of them show on each kind of shape (`Kind::controls`, `Style::adopt`), the file format), perfect-freehand ink outlines (`freehand`), geometry, hit testing, lasso and box selection, the camera, copy and paste, the undo history, and what its agent reads, edits and sees of it (`agent`: the flat shape form, ops applied all or nothing, the SVG of an area) | `canvas/`: the view (`CanvasView`: tools, gestures, keys, saving the file), painting (shapes tessellated once in page units, only moved onto the screen each frame), the tool bar, zoom pill, style panel (only the controls the current tool and the selection show) and minimap, and the agent floating over it (`agent.rs`: a `SessionView` in a card, started with the `gentle-canvas.ts` extension it installs; answering its requests, rasterizing shots). The workspace handles that agent's events as an agent pane's (`on_session_event`) and saves its conversation with the canvas pane. |
| `commands`: `/gentle:tower` routes to the Tower tab (`Screen::Tower`); `tower`: the URL from `gentle-shell tower`'s `--json` line or its older `Gentle Tower: <url>` text, and whether a Shell refused `--json` | `tower/`: Gentle Tower in a tab (`TowerView`): runs `gentle-shell tower --no-open --json` (again without `--json` on a Shell older than 5.0.0 that refuses it), shows its page in a `WebSurface`, shown or hidden by the workspace's `sync_web_views` like agents' web views. |
| `commands`: `/gentle:changes` routes to the review screen (`Screen::Changes`) | `review/`: the review screen (`ReviewView`), a modal over the window (`Overlay::Review`, Esc closes it like the other screens): a `files::FileView` diff (Split · Unified) beside its own `files::ChangesView`, which polls while the review is open; the composer's `±` and `/gentle:changes` open it (`workspace/overlays.rs` `open_review`). |

## Layout

```text
crates/
├── app/
│   └── src/
│       ├── main.rs              # startup: CLI, single instance, toolchain PATH, config adoption
│       ├── app/                 # composition root (mod.rs): GPUI, themes, shortcuts, menus, window; integration.rs registers panels and sidebar
│       ├── cli.rs               # arguments (`[FOLDER] [--cwd] [-p TEXT]`) and initial folder
│       ├── features/            # one folder per feature; its entry point is mod.rs
│       │   ├── session/  workspace/  workspaces/  sessions/  palette/  profiles/  settings/  usage/  setup/
│       │   ├── browser/  devices/  music/  files/  terminal/  git/  history/  providers/  memory/  onboarding/  notices/  alerts/  hydration/  docs/
│       │   └── app_update/  updates/  welcome/  genui/  diagnostics/  canvas/  tower/  review/
│       ├── platform/            # OS integration: single instance, agent command shims, Dock and window icons, installed editors, power state, whether a terminal's shell is idle
│       └── shared/              # pieces used by several features, with no logic of their own:
│                                # ui, overlay (live indicators' layer), floating, themes, backdrop (theme scenes), assets, banner, surge, pane, gentle_state
├── core/                        # shared pure logic (gentle-core)
└── pi-rpc/
```

Paths follow the folder: `crate::features::session`, `crate::shared::ui`, `crate::platform::instance`. Each feature is a `features/<name>/` folder: `mod.rs` keeps its public types and API, and child modules add `impl` blocks and mark what they share as `pub(super)`. A new feature goes in `features/` as a folder even if it starts as a single file; something moves to `shared/` only when more than one feature uses it and it has no product logic.

`core` grows one complete behavior at a time and only with pure logic; anything that needs IO, processes, the clock or GPUI stays in `app` and calls `core`. No traits or adapters are introduced just to move something into `core`.

## Anatomy of a feature

A large feature can be split by responsibility:

```text
feature/
├── mod.rs          # public API and coordination
├── state.rs        # its own state and models
├── update.rs       # pure transitions, if any
├── effects.rs      # processes, network, files and async tasks
└── view/           # render and purely visual state
```

This shape is a guideline, not a mandatory template. A small feature should stay a single module.

### Current session boundary

`SessionView` is a tab: one `gentle-shell --mode rpc` backend and its conversation. Its logic is split by responsibility into `impl SessionView` blocks in sibling modules; `session/mod.rs` keeps the struct, the constructor, the public API used by the window (`workspace/mod.rs`), notifications to the window (`SessionEvent`, `sync_chrome`), actions and module wiring:

```text
session/
├── mod.rs          # SessionView, constructor, public API, actions and window utilities
├── state.rs        # groups of fields that change together and their pure transitions
├── events.rs       # RPC events, extension UI requests, stderr/output, Git refreshes and generative UI surface requests (send, ask, expand)
├── rpc.rs          # request/response, state read from the backend and commands (model, compact, export, sessions, prompts)
├── lifecycle.rs    # startup, restart (now or at end of turn), resume and conversation reset
├── submit.rs       # composer submission: `!` shell, built-ins, native screens and attachments
├── agents.rs       # agents panel
├── builtins.rs     # command classification and capture of extension output (pure)
├── attachments.rs  # image validation and payload, long pastes, `[Image #N]` / `[Pasted text #N]` tokens (pure)
└── view.rs, view/  # render (see below)
```

`state.rs` groups the backend and its lifecycle (`Backend`: process state, version, stderr, pending prompt and session, pending restart), models and thinking level (`Models`), usage and compaction (`Usage`), extension UI (`ExtensionUi`: dialogs, statuses, widgets, usage limits and recent toasts), turn activity (`TurnActivity`: phase, queue, jobs and helpers), the todo island (`TodoIsland`) and Git state (`GitStatus`). Event interpretations that don't need GPUI (phase and jobs of a tool call, queue count, Gentle Design canvas, level of a `notify`) are pure functions or methods with tests in `events.rs`; effects (requests, timers, notifications) stay in `SessionView` methods.

### Current session view boundary

`session/view.rs` keeps the composition (`Render`), the sync of what is derived from the conversation (`sync_view`, rendered Markdown), the rendering of conversation items (tool cards, thinking, messages, notices) and the pieces shared by several views. Each visual responsibility of the composer and the conversation lives in a child module that exposes to the view only the API it needs:

```text
session/
├── view.rs                   # Render, conversation rows and shared pieces
└── view/
    ├── live.rs               # timers and transient visual state
    ├── transcript_list.rs    # rows, measurements and caches of the virtualized list
    ├── cards.rs              # extension custom message cards
    ├── composer.rs           # composer: context ring, attachments, text and bottom bar
    ├── activity.rs           # activity line and status chips (jobs, widgets, usage, helpers, limits)
    ├── slash_menu.rs         # `/` command menu and its keyboard handling
    ├── model_picker.rs       # model picker, search and keyboard handling
    ├── effort.rs             # effort control (thinking level) and its slider
    ├── permissions.rs        # permission mode chip and picker
    ├── dialogs.rs            # extension dialogs and their keyboard handling
    └── todo_island.rs        # todo island over the conversation
```

`transcript_list.rs` owns the row shape, the computation of partial updates, the cache of prepared data and the GPUI list state. `view.rs` decides what content to render for each row, but does not directly modify the list's internal structure.

Child modules add their own `impl SessionView` blocks; what another module consumes is marked `pub(super)`, and what `session/mod.rs` uses is marked `pub(in crate::features::session)` or re-exported from `view.rs`. The width constants (`NARROW`, `TINY`), the collapse states and the shared pieces (`clock`, `image_tile`, `number_badge`) stay in `view.rs`.

### Other split features

Each one keeps its public type and API in the parent file; child modules add their own `impl` blocks and mark what they share as `pub(super)`.

```text
workspace/mod.rs     # Workspace, types (Pane, Tab, Space, Overlay) and public API
workspace/
├── layout.rs       # pane split calculations, tiling direction, drop zones
├── panes.rs        # create, split, focus and close panes
├── closed.rs       # reopening closed panes and tabs where they were
├── spaces.rs       # named workspaces
├── menu.rs         # right-click menus (workspace, tab, empty area) and inline workspace rename
├── tabs.rs         # tabs and renaming
├── persistence.rs  # snapshot and saving
├── actions.rs      # shortcut handlers
├── overlays.rs     # overlay screens, palette and max animation
├── stages.rs       # web and device cards, tiling, the title bar's instance tray
├── view.rs         # title bar, tabs and panes
└── render.rs       # impl Render

profiles/mod.rs      # ProfilesView, ProfilesEvent, state and queries
profiles/
├── env.rs          # homes, gentle-pi package and agent discovery
├── files.rs        # atomic writes, profiles.json, models.json, settings.json, loading
├── pins.rs         # repository pins (files and Git worktree)
├── apply.rs        # route materialization, orchestrator and Make default
├── draft.rs        # draft edits (model, effort, tier)
├── actions.rs      # store actions and exit, name and delete flows
├── tests.rs        # file effect tests
└── view.rs, view/  # Render; cells, table, profile_list, confirm, footer

usage/mod.rs         # UsageView, state, refreshes and public API
usage/
├── credentials.rs  # auth.json → cards and requests; key resolution
├── fetch.rs        # usage HTTP requests
├── stats.rs        # session files: walking, reading, cache
└── view.rs, view/  # Render; subscription, stats

settings/mod.rs      # SettingsView: state, loading, page navigation
settings/
├── events.rs       # SettingsEvent, what the workspace handles
├── view.rs         # Render: header, category rail, page body
├── navigation.rs   # SettingsPage (the nine categories), search, rail and compact picker
├── pages/          # one file per category, composing the sections below
├── services/       # blocking work off the UI thread: snapshot load, upgrades
├── layout.rs       # row, section, or_dash
├── snapshot.rs     # reading the configuration and launcher probes
├── claude.rs       # Claude Code session and bridge plan
├── background.rs   # background subagents policy and Gentle AI section
├── upgrade.rs      # gentle-shell upgrade, its result and the card
├── packages.rs     # install and remove packages
└── notifications.rs, theme.rs, app.rs, models.rs, devices.rs, maestro.rs, music.rs, hydration.rs, agent_ui.rs  # one section each

palette/mod.rs       # PaletteView, actions, events, categories
palette/
├── commands.rs     # screens, panel commands and backend `/` commands
├── sources.rs      # files under the root and local branches
├── rows.rs         # rows and groups for the query and category
├── keyboard.rs     # selection, run row, categories, rename workspace
└── view.rs         # Render

sessions/mod.rs      # SessionsView, SessionsEvent, Scope
sessions/
├── discovery.rs    # sessionDir and .jsonl files per folder or all
├── summary.rs      # lightweight JSONL scan and per-file cache
├── view.rs         # state and actions
├── render.rs       # rows, list and Render
└── tests.rs        # tests with temporary session files

git/mod.rs           # run git, toplevel; re-exports the types callers use
git/
├── snapshot.rs     # changes for the panel and the tree
├── changes.rs      # Changes tab: stage, discard, stash, commit, push
├── refs.rs         # Branches tab: branches, stash, remote checkout, worktrees (add, remove, prune)
├── log.rs          # Git pane: the commits of HEAD, a branch or every ref, total, branches, dirty work tree, upstream, remotes, a command stopped midway
└── ops.rs          # Git pane: fetch, pull, branch, tag, switch, checkout, cherry-pick, revert, reset, reword, remotes, LFS, continue or abort

history/            # mod.rs: HistoryView (a `PaneKind::History` pane, the Git pane), scope, paging, filter, selection and its details, the activity strip's read; ops.rs (actions off the UI thread, the inline prompt, confirmations); avatars.rs (authors' pictures: lookup, disk cache); graph.rs (paints a row's lanes), view.rs (header, toolbar, rows and their menus, details, activity strip, hover card, footer)

genui/              # mod.rs: GenUi (surfaces rebuilt from the transcript, live card vs superseded rows, expanded); extension.rs + gentle-ui.ts (the Pi extension);
                    # diagram.rs: DiagramView (versions, local edits, undo, viewport, pointer and keys, send); view.rs (card, nodes, details, footer, conversation rows); paint.rs (groups, edges, arrowheads)

app_update/mod.rs    # AppUpdate (Global), Status, Problem; check, install, skip, restart
app_update/
├── release.rs      # versions, per-OS asset, release and SHA256SUMS parsing, decision
├── cache.rs        # app-update.json: 6 h cache and skipped version
├── network.rs      # fetch the release, download with sha256 and progress
├── archive.rs      # safe zip and tar.gz extraction; per-OS payload
├── install.rs      # replace the bundle (macOS), the file (Linux) and the running exe (Windows)
└── relaunch.rs     # process that reopens the app on exit

providers/          # mod.rs: types and constructor; login.rs (login helper and flow), list.rs, flow_view.rs, render.rs
onboarding/         # mod.rs: state and API; checks.rs (install plan, tones), install.rs, view.rs
notices/            # mod.rs: Notice, Notices; store.rs (push, read, limit), bell.rs, panel.rs
memory/             # mod.rs: MemoryView (opens on a MemoryTarget); client.rs (HTTP to Engram), view.rs
alerts/             # mod.rs: fire, context_threshold; policy.rs, sound.rs, tones.rs
updates/            # mod.rs: check and skip; cache.rs (launcher config), registry.rs (npm)
workspaces/         # mod.rs: saved model; store.rs (workspaces.json), tests.rs
```

Tabs are `Tab { id, pinned, state }`: `TabState::Unbuilt(SavedTab)` until first shown (`ensure_tab_built`, called by `activate`, the one path that makes a tab the shown one), then `TabState::Built { tree, focused }`. Opening a workspace builds only its active tab; every workspace's tabs exist as unbuilt tabs from launch, so the sidebar, counts, search and menus see one model, and `persistence.rs` `saved_tab` writes an unbuilt tab back unchanged.

## Data flow

```mermaid
flowchart LR
    Input["UI or RPC event"] --> Update["Update state"]
    Update --> State["New state"]
    Update --> Effect["Required effect"]
    Effect --> Adapter["RPC, file, Git or system"]
    Adapter --> Result["Result event"]
    Result --> Update
    State --> View["GPUI render"]
```

Effects must not hide state changes. Results return to the feature so it decides how to represent them.

## Dependency rules

1. `pi-rpc` **does not depend** on `app` or GPUI.
2. `app` may depend on `pi-rpc` and `core`; the reverse direction is forbidden.
3. `core` **does not depend** on anything in the project (`app`, `pi-rpc`), nor on GPUI, the filesystem, processes, the clock or system APIs.
4. A feature consumes another through its public API; it does not access its private modules.
5. Shared UI code contains no business rules and knows nothing about specific features.
6. Integration with the operating system, processes and files stays at the edges.
7. Types stay private unless they are part of a real contract between modules.
8. No trait is introduced until there is a boundary that needs substitution, several implementations, or a test that cannot be solved with a pure function.
9. Splitting a file by size is not enough: each extracted module must have a nameable, cohesive responsibility.
10. Every refactor preserves behavior. When behavior is not covered, add a relevant characterization test first.

## Testing strategy

- **Pure logic:** unit tests for transitions, precedence, limits and errors.
- **Parsers and protocol:** representative inputs, incomplete data and invalid records.
- **Integrations:** tests at the edge, or a smoke test that runs the real path.
- **UI:** test state and observable decisions; don't test that a function merely delegates or copies fields.

Before completing a stage, run formatting, tests, Clippy and a smoke test of the modified path. If the environment does not allow a check, document it explicitly; don't assume it passed.

## How to add a feature

1. Find the feature that owns the behavior.
2. Put state, events and view there while they stay cohesive.
3. Extract pure logic into its own module when it can be tested without GPUI or IO.
4. Use `pi-rpc` or a platform module for external effects; don't duplicate the transport or system detection.
5. Expose from `mod.rs` only what other modules consume.
6. Add tests that cover behavior prone to regression.
7. Update this document or create an ADR if a boundary or a dependency rule changes.

## Related decisions

- [ADR-0001: Modular monolith by feature](adr/0001-modular-monolith.md)
- [ADR-0002: Incremental pure core](adr/0002-incremental-pure-core.md)
- [ADR-0003: Behavior-preserving refactors](adr/0003-behavior-preserving-refactors.md)
- [ADR-0004: Draw Mermaid diagrams with merman's CLI, downloaded on demand](adr/0004-native-mermaid-rendering.md)
