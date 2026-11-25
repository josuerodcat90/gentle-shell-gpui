//! First-run onboarding engine: checks every dependency Gentle Shell needs on
//! this OS and installs what's missing without sudo. Anything that needs a
//! password or an interactive login is handed to the user as a terminal
//! command (`Fix::Terminal`).
//!
//! Toolchains it installs live in `<data dir>/gentle-shell-gpui/toolchain/`
//! (`node/`, `npm-global/`, `bin/`); `apply_toolchain_path` puts them first
//! on pi-rpc's search PATH, so sessions find them on every launch.

pub mod devices;
mod extras;
mod fetch;
mod gentle;
pub mod maestro;
pub mod music;
mod probe;
mod remote;
pub mod tower;

use std::path::{Path, PathBuf};

use serde_json::json;

pub use probe::platform_label;
use probe::{Version, capture, parse_version};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CheckId {
    Node,
    Go,
    GentleShell,
    Setup,
    Provider,
    Git,
    Engram,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    Required,
    Recommended,
    Optional,
}

/// The text says what was found, or why not.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Ok(String),
    Missing(String),
    Outdated(String),
    Failed(String),
}

/// `Auto`: `install` does it unattended. `Terminal`: needs the user (sudo or
/// an interactive login); run the command in a terminal. `SignIn`: Providers.
#[derive(Clone, Debug, PartialEq)]
pub enum Fix {
    None,
    Auto,
    Terminal(String),
    SignIn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub id: CheckId,
    pub title: &'static str,
    pub summary: &'static str,
    pub need: Need,
    pub status: Status,
    pub fix: Fix,
}

/// Progress and tool output, one line at a time.
type Log<'a> = dyn Fn(String) + Send + Sync + 'a;

/// gentle-pi `engines.node`.
const MIN_NODE: Version = (22, 19, 0);
/// gentle-pi's Windows postinstall builds Gentle AI with this Go.
const MIN_GO: Version = (1, 25, 10);
/// The launcher refuses an older Pi.
const MIN_PI: Version = (1, 2, 0);
/// The first gentle-pi with Gentle Desktop (the `gentle:desktop` command,
/// gentle-shell#3240): an older Shell chats, but every tab warns and the
/// app's Desktop features (Profiles over RPC, helpers, usage) stay off.
const MIN_GENTLE_SHELL: Version = (4, 8, 0);
/// gentle-engram accepts a stable Engram core from 3.0.0; with an older
/// server it can't verify the server's identity and memory stays offline.
const MIN_ENGRAM: Version = (3, 0, 0);

// ── Toolchain folders and PATH ─────────────────────────────────────────────

fn user_home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(std::env::temp_dir)
}

/// `<app data dir>/toolchain` (`shared::data_dir`).
fn toolchain_dir() -> Option<PathBuf> {
    crate::shared::data_dir::app().map(|dir| dir.join("toolchain"))
}

/// Where an npm prefix (or a Node install) keeps its executables.
fn prefix_bin(prefix: &Path, windows: bool) -> PathBuf {
    if windows {
        prefix.to_path_buf()
    } else {
        prefix.join("bin")
    }
}

/// The app-owned toolchain folders that exist: managed Node, the user npm
/// prefix, loose binaries (Engram), the Maestro CLI and its Java, the
/// music player, and the Android SDK's emulator and adb when installed.
fn managed_dirs() -> Vec<PathBuf> {
    let Some(toolchain) = toolchain_dir() else {
        return Vec::new();
    };
    let windows = cfg!(windows);
    [
        prefix_bin(&toolchain.join("node"), windows),
        prefix_bin(&toolchain.join("npm-global"), windows),
        toolchain.join("bin"),
        maestro::toolchain_bin(&toolchain),
        maestro::toolchain_java_bin(&toolchain),
        // gentle-stream, so `/gentle:music` in a terminal tab finds it too.
        toolchain.join("music").join("bin"),
    ]
    .into_iter()
    // The Android SDK's tools, so agents can start emulators and use adb.
    .chain(crate::features::devices::android_tool_dirs())
    // agent-device, once "Agents can use devices" installed it.
    .chain(devices::agent_bin_dir())
    .filter(|dir| dir.is_dir())
    .collect()
}

/// Put the app's toolchain folders first on pi-rpc's search PATH (and, on
/// Windows, the install folders of tools winget added since this app
/// started, when they aren't found otherwise). Cheap: called at startup and
/// before every check run.
pub fn apply_toolchain_path() {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut dirs = managed_dirs();
    #[cfg(windows)]
    {
        // The managed folders first, so fallbacks only fill real gaps.
        pi_rpc::set_extra_path(dirs.clone());
        for (program, dir) in extras::windows_fallback_dirs() {
            if dir.is_dir() && !dirs.contains(&dir) && pi_rpc::find_program(program).is_none() {
                dirs.push(dir);
            }
        }
    }
    pi_rpc::set_extra_path(dirs);
}

/// After an install: new folders on the PATH, and pi-rpc's cached login
/// PATH and `--version` answers dropped.
fn refresh_paths() {
    pi_rpc::forget_cached();
    apply_toolchain_path();
}

// ── Checks ─────────────────────────────────────────────────────────────────

fn meta(id: CheckId) -> (&'static str, &'static str, Need) {
    match id {
        CheckId::Node => (
            "Node.js",
            "Runs Gentle Shell and its packages (22.19 or newer, with npm).",
            Need::Required,
        ),
        CheckId::Go => (
            "Go",
            "Builds the Gentle AI companion on Windows (1.25.10 or newer).",
            Need::Required,
        ),
        CheckId::GentleShell => (
            "Gentle Shell",
            "The gentle-pi launcher and the Pi agent it bundles.",
            Need::Required,
        ),
        CheckId::Setup => (
            "Companion setup",
            "Gentle AI's packages, skills and memory in your Gentle Shell home.",
            Need::Required,
        ),
        CheckId::Provider => (
            "AI provider",
            "Sign in to at least one model provider to chat.",
            Need::Required,
        ),
        CheckId::Git => (
            "Git",
            "Reviews, worktrees, changes and branches.",
            Need::Recommended,
        ),
        CheckId::Engram => (
            "Engram",
            "Long-term memory across sessions.",
            Need::Optional,
        ),
    }
}

/// The checks shown on `os` (`std::env::consts::OS`), in display order.
fn check_order(os: &str) -> Vec<CheckId> {
    use CheckId::*;
    let mut ids = vec![Node];
    if os == "windows" {
        ids.push(Go);
    }
    ids.extend([GentleShell, Setup, Provider, Git, Engram]);
    ids
}

/// Blocking: probes every dependency relevant to this OS.
pub fn run_checks() -> Vec<Check> {
    apply_toolchain_path();
    let choice = crate::features::backend::saved();
    if choice != gentle_core::backend::Backend::Local {
        return remote::run_checks(&choice);
    }
    let launcher = gentle::launcher();
    let managed = launcher
        .as_ref()
        .is_ok_and(|launcher| launcher.spec.managed_by_gentle_ai());
    let agent = pi_rpc::resolve_homes().agent;
    let claude = pi_rpc::find_program("claude");
    let signed_in = claude.as_deref().and_then(gentle::claude_signed_in);
    check_order(std::env::consts::OS)
        .into_iter()
        .map(|id| {
            let (status, fix) = match id {
                CheckId::Node => extras::check_node(),
                CheckId::Go => extras::check_go(),
                CheckId::GentleShell => gentle::check_gentle_shell(&launcher),
                CheckId::Setup => gentle::check_setup(&launcher),
                CheckId::Provider => extras::check_provider(&agent, signed_in),
                CheckId::Git => extras::check_git(),
                CheckId::Engram => extras::check_engram(),
            };
            let (status, fix) = if managed {
                for_managed_install(id, status, fix)
            } else {
                (status, fix)
            };
            let (title, summary, need) = meta(id);
            Check {
                id,
                title,
                summary,
                need,
                status,
                fix,
            }
        })
        .collect()
}

/// gentle-ai's managed install brings its own Node.js and Gentle AI, and
/// gentle-ai is what repairs and updates it (its TUI, in a terminal), never
/// the launcher's `upgrade` or `setup`: they would change files it verifies.
fn for_managed_install(id: CheckId, status: Status, fix: Fix) -> (Status, Fix) {
    let ok = matches!(status, Status::Ok(_));
    match id {
        CheckId::Node if !ok => (
            Status::Ok("Gentle Shell runs the Node.js in gentle-ai's managed install".into()),
            Fix::None,
        ),
        CheckId::Go if !ok => (
            Status::Ok("Not needed: gentle-ai's managed install includes Gentle AI".into()),
            Fix::None,
        ),
        CheckId::GentleShell | CheckId::Setup if fix == Fix::Auto => {
            (status, Fix::Terminal("gentle-ai".into()))
        }
        _ => (status, fix),
    }
}

/// Blocking: run `gentle-shell <args>` (e.g. `install <source>`) for the
/// folder `cwd`, with the launcher's home options; its output either way.
pub fn run_gentle_shell(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let spec = crate::features::backend::shell_spec(cwd).map_err(|e| format!("{e:#}"))?;
    let out = spec
        .launcher_command(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

/// The agent dir `gentle-shell --version` names where agents run (as that
/// backend writes it). Blocking.
pub fn launcher_home() -> Option<String> {
    let version = gentle::launcher().ok()?.version?;
    Some(version.home_dir?.to_string_lossy().into_owned())
}

/// The diagram renderer (merman's CLI) the Markdown preview draws Mermaid
/// with, once installed. Not part of the app: downloaded on demand.
pub fn diagram_renderer() -> Option<PathBuf> {
    fetch::merman_bin()
}

/// Blocking: download and install the diagram renderer; its path.
pub fn install_diagram_renderer(log: &(dyn Fn(String) + Send + Sync)) -> Result<PathBuf, String> {
    fetch::install_merman(log)
}

/// The Engram the app runs, as `X.Y.Z`, when it reports one.
pub fn engram_version() -> Option<String> {
    extras::engram_version()
}

/// Blocking: the latest Engram release's version, from GitHub.
pub fn engram_latest() -> Result<String, String> {
    fetch::engram_latest()
}

/// Blocking: update Engram the way it was installed (Homebrew, or the
/// release binary in the app's toolchain); the version it runs afterwards.
pub fn upgrade_engram(log: &(dyn Fn(String) + Send + Sync)) -> Result<String, String> {
    extras::upgrade_engram(log)
}

/// Blocking: installs `id` unattended, streaming progress lines to `log`.
pub fn install(id: CheckId, log: &(dyn Fn(String) + Send + Sync)) -> Result<(), String> {
    let result = match id {
        CheckId::Node => fetch::install_node(log),
        CheckId::Go if cfg!(windows) => extras::install_go(log),
        CheckId::Go => Err("Go is only needed on Windows.".into()),
        CheckId::GentleShell => gentle::install_gentle_shell(log),
        CheckId::Setup => gentle::run_setup(log),
        CheckId::Provider => Err("Sign in from the Providers screen.".into()),
        CheckId::Git => extras::install_git(log),
        CheckId::Engram => extras::install_engram(log),
    };
    refresh_paths();
    crate::features::settings::forget_claude_auth();
    result
}

// ── Completion marker ──────────────────────────────────────────────────────

/// Onboarding was never completed on this machine.
pub fn first_run() -> bool {
    let Some(path) = crate::shared::themes::store_path() else {
        return true;
    };
    let done = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .is_some_and(|settings| settings["onboarding"]["done"] == true);
    !done
}

/// Persist completion in the app's `settings.json` (`onboarding` key).
pub fn mark_done() {
    let Some(path) = crate::shared::themes::store_path() else {
        return;
    };
    let value = json!({ "done": true, "version": env!("CARGO_PKG_VERSION") });
    if let Err(err) = crate::shared::themes::write_key(&path, "onboarding", value) {
        eprintln!("gentle-shell-gpui: could not save onboarding state: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CheckId::*;

    #[test]
    fn checks_per_os_in_display_order() {
        let unix = vec![Node, GentleShell, Setup, Provider, Git, Engram];
        assert_eq!(check_order("macos"), unix);
        assert_eq!(check_order("linux"), unix);
        let windows = check_order("windows");
        assert_eq!(&windows[..3], &[Node, Go, GentleShell]);
        assert_eq!(windows.len(), unix.len() + 1);
    }

    #[test]
    fn needs_per_check() {
        assert_eq!(meta(Node).2, Need::Required);
        assert_eq!(meta(Provider).2, Need::Required);
        assert_eq!(meta(Git).2, Need::Recommended);
        assert_eq!(meta(Engram).2, Need::Optional);
    }

    #[test]
    fn npm_prefix_bin_per_os() {
        let prefix = Path::new("/data/toolchain/npm-global");
        assert_eq!(prefix_bin(prefix, false), prefix.join("bin"));
        assert_eq!(prefix_bin(prefix, true), prefix.to_path_buf());
    }

    /// A Windows machine with only gentle-ai's managed install: no Node or
    /// Go of its own, and gentle-ai, not the launcher, repairs the Shell.
    #[test]
    fn managed_install_needs_no_runtime_and_is_fixed_by_gentle_ai() {
        let missing = || Status::Missing("not here".into());
        for id in [Node, Go] {
            let (status, fix) = for_managed_install(id, missing(), Fix::Auto);
            assert!(matches!(status, Status::Ok(_)), "{id:?}");
            assert_eq!(fix, Fix::None);
        }
        // A system Node still reports its own version.
        let found = Status::Ok("Node 24.1.0 · npm 11.0.0".into());
        assert_eq!(
            for_managed_install(Node, found.clone(), Fix::None),
            (found, Fix::None)
        );
        for id in [GentleShell, Setup] {
            assert_eq!(
                for_managed_install(id, missing(), Fix::Auto),
                (missing(), Fix::Terminal("gentle-ai".into()))
            );
        }
        assert_eq!(
            for_managed_install(Git, missing(), Fix::Auto),
            (missing(), Fix::Auto)
        );
    }
}
