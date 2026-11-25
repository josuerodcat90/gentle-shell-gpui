//! Node, Go, Git, Engram, the provider and the Claude Code sign-in.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::gentle::bridge_version;
use super::probe::{
    go_version, os_release_value, output_of, parse_version, run_logged, show_version,
};
use super::{Fix, Log, MIN_ENGRAM, MIN_GO, MIN_NODE, Status, fetch, refresh_paths};

// ── Node.js ────────────────────────────────────────────────────────────────

pub fn check_node() -> (Status, Fix) {
    let fix =
        if fetch::node_asset("v22.0.0", std::env::consts::OS, std::env::consts::ARCH).is_some() {
            Fix::Auto
        } else {
            Fix::None
        };
    let Some(node) = pi_rpc::find_program("node") else {
        return (Status::Missing("Node.js isn't installed".into()), fix);
    };
    let Some(text) = output_of(&node, &["--version"]) else {
        return (
            Status::Failed(format!("{} doesn't run", node.display())),
            fix,
        );
    };
    let Some(version) = parse_version(&text) else {
        return (
            Status::Failed(format!("Unexpected `node --version`: {text}")),
            fix,
        );
    };
    if version < MIN_NODE {
        return (
            Status::Outdated(format!(
                "Node {} — needs {} or newer",
                show_version(version),
                show_version(MIN_NODE)
            )),
            fix,
        );
    }
    match pi_rpc::find_program("npm").and_then(|npm| output_of(&npm, &["--version"])) {
        Some(npm) => (
            Status::Ok(format!("Node {} · npm {npm}", show_version(version))),
            Fix::None,
        ),
        None => (
            Status::Missing(format!("Node {} found, but not npm", show_version(version))),
            fix,
        ),
    }
}

// ── winget (Windows) ───────────────────────────────────────────────────────

fn winget() -> Option<PathBuf> {
    pi_rpc::find_program("winget")
}

/// A per-machine package via winget; Windows may show its own UAC prompt.
fn winget_install(id: &str, what: &str, log: &Log) -> Result<(), String> {
    let winget = winget().ok_or("winget isn't available on this system.")?;
    let mut command = pi_rpc::command(winget);
    command.args([
        "install",
        "--id",
        id,
        "-e",
        "--silent",
        "--accept-source-agreements",
        "--accept-package-agreements",
    ]);
    log(format!(
        "Installing {what} with winget (Windows may ask for permission)…"
    ));
    run_logged(command, "winget install", log)
}

/// Install folders of tools a winget install puts on the machine PATH, which
/// this already-running app doesn't see: `(program, dir)`.
#[cfg(windows)]
pub fn windows_fallback_dirs() -> Vec<(&'static str, PathBuf)> {
    let env = |key: &str| std::env::var_os(key).map(PathBuf::from);
    let program_files = env("ProgramFiles").unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    let mut dirs = vec![
        ("go", program_files.join("Go").join("bin")),
        ("git", program_files.join("Git").join("cmd")),
        ("node", program_files.join("nodejs")),
    ];
    if let Some(local) = env("LOCALAPPDATA") {
        dirs.push(("git", local.join("Programs").join("Git").join("cmd")));
    }
    if let Some(roaming) = env("APPDATA") {
        dirs.push(("gentle-shell", roaming.join("npm")));
    }
    dirs
}

// ── Go (Windows) ───────────────────────────────────────────────────────────

fn go_fix() -> Fix {
    if winget().is_some() {
        Fix::Auto
    } else {
        Fix::Terminal("start https://go.dev/dl/".into())
    }
}

pub fn check_go() -> (Status, Fix) {
    let Some(go) = pi_rpc::find_program("go") else {
        return (Status::Missing("Go isn't installed".into()), go_fix());
    };
    match output_of(&go, &["version"]).as_deref().and_then(go_version) {
        Some(v) if v >= MIN_GO => (Status::Ok(format!("Go {}", show_version(v))), Fix::None),
        Some(v) => (
            Status::Outdated(format!(
                "Go {} — needs {} or newer",
                show_version(v),
                show_version(MIN_GO)
            )),
            go_fix(),
        ),
        None => (
            Status::Failed(format!("{} doesn't run", go.display())),
            go_fix(),
        ),
    }
}

pub fn install_go(log: &Log) -> Result<(), String> {
    let result = winget_install("GoLang.Go", "Go", log);
    refresh_paths();
    // winget exits non-zero when it's already installed; the probe decides.
    match check_go() {
        (Status::Ok(_), _) => Ok(()),
        _ => result.and(Err(
            "Go was installed, but this app can't find it yet.".into()
        )),
    }
}

// ── Git ────────────────────────────────────────────────────────────────────

/// The distro's package manager command for `package`.
pub fn linux_install_command(
    package: &str,
    os_release: &str,
    has: impl Fn(&str) -> bool,
) -> String {
    let ids: Vec<String> = ["ID", "ID_LIKE"]
        .iter()
        .filter_map(|key| os_release_value(os_release, key))
        .flat_map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>())
        .collect();
    let is = |names: &[&str]| ids.iter().any(|id| names.contains(&id.as_str()));
    let manager = if is(&["debian", "ubuntu"]) {
        "apt"
    } else if is(&["fedora", "rhel", "centos"]) {
        "dnf"
    } else if is(&["arch"]) {
        "pacman"
    } else if is(&["suse", "opensuse"]) {
        "zypper"
    } else if is(&["alpine"]) {
        "apk"
    } else {
        ["apt", "dnf", "pacman", "zypper", "apk"]
            .into_iter()
            .find(|m| has(m))
            .unwrap_or("apt")
    };
    match manager {
        "pacman" => format!("sudo pacman -S {package}"),
        "apk" => format!("sudo apk add {package}"),
        other => format!("sudo {other} install {package}"),
    }
}

fn git_fix() -> Fix {
    match std::env::consts::OS {
        "windows" if winget().is_some() => Fix::Auto,
        "windows" => Fix::Terminal("start https://git-scm.com/download/win".into()),
        "macos" => Fix::Terminal("xcode-select --install".into()),
        _ => {
            let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
            Fix::Terminal(linux_install_command("git", &release, |m| {
                pi_rpc::find_program(m).is_some()
            }))
        }
    }
}

pub fn check_git() -> (Status, Fix) {
    let mut git = pi_rpc::find_program("git");
    // macOS's /usr/bin/git is a stub that pops the Command Line Tools
    // installer until they are installed.
    if cfg!(target_os = "macos")
        && git.as_deref() == Some(Path::new("/usr/bin/git"))
        && output_of(Path::new("/usr/bin/xcode-select"), &["-p"]).is_none()
    {
        git = None;
    }
    match git.and_then(|git| output_of(&git, &["--version"])) {
        Some(text) => (
            Status::Ok(format!(
                "Git {}",
                text.trim_start_matches("git version ").trim()
            )),
            Fix::None,
        ),
        None => (Status::Missing("Git isn't installed".into()), git_fix()),
    }
}

pub fn install_git(log: &Log) -> Result<(), String> {
    if !cfg!(windows) {
        return match git_fix() {
            Fix::Terminal(command) => Err(format!(
                "Git needs your password to install: run `{command}`."
            )),
            _ => Err("Install Git from your system's package manager.".into()),
        };
    }
    let result = winget_install("Git.Git", "Git", log);
    refresh_paths();
    match check_git() {
        (Status::Ok(_), _) => Ok(()),
        _ => result.and(Err(
            "Git was installed, but this app can't find it yet.".into()
        )),
    }
}

// ── Engram ─────────────────────────────────────────────────────────────────

fn engram_bin() -> Option<PathBuf> {
    std::env::var_os("ENGRAM_BIN")
        .filter(|b| !b.is_empty())
        .map(PathBuf::from)
}

fn engram() -> Option<PathBuf> {
    engram_bin().or_else(|| pi_rpc::find_program("engram"))
}

/// Why an `ENGRAM_BIN` override can't run: nothing it names can be executed.
/// Resolved the way the override is run, not as a path on disk, so a bare name
/// on the PATH and a Windows path without `.exe` still pass. Installing doesn't
/// help (the override still wins), and Gentle Shell reads it too, so it's
/// reported rather than skipped.
fn stale_engram_bin(bin: &Path) -> Option<String> {
    pi_rpc::find_program(&bin.to_string_lossy())
        .is_none()
        .then(|| {
            format!(
                "ENGRAM_BIN points to {}, which can't be run. Fix or remove it.",
                bin.display()
            )
        })
}

/// `stale_engram_bin` for the current environment. Only for Engram on this
/// computer: the path is read here, so it is checked here. When agents run
/// in a backend, `remote` looks for Engram there and never gets this far.
fn engram_bin_problem() -> Option<String> {
    engram_bin().and_then(|bin| stale_engram_bin(&bin))
}

pub fn check_engram() -> (Status, Fix) {
    if let Some(why) = engram_bin_problem() {
        return (Status::Failed(why), Fix::None);
    }
    match engram().and_then(|e| output_of(&e, &["version"])) {
        Some(text) => engram_status(&text),
        None => (Status::Missing("Not installed".into()), Fix::Auto),
    }
}

/// From `engram version` (`engram 1.18.0`, then an update notice). A version
/// it can't read stays OK, as before.
pub(super) fn engram_status(text: &str) -> (Status, Fix) {
    let line = text.lines().next().unwrap_or("installed").trim();
    match line.split_whitespace().last().and_then(parse_version) {
        Some(version) if version < MIN_ENGRAM => (
            Status::Outdated(format!(
                "Engram {} — needs {} or newer",
                show_version(version),
                show_version(MIN_ENGRAM)
            )),
            Fix::Auto,
        ),
        _ => (Status::Ok(line.to_string()), Fix::None),
    }
}

fn brew() -> Option<PathBuf> {
    pi_rpc::find_program("brew").or_else(|| {
        [
            "/opt/homebrew/bin/brew",
            "/usr/local/bin/brew",
            "/home/linuxbrew/.linuxbrew/bin/brew",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
    })
}

/// Homebrew's tap when Homebrew is there, else the GitHub release binary;
/// in the backend agents run in, when there is one.
pub fn install_engram(log: &Log) -> Result<(), String> {
    if let Some(choice) = super::remote::backend() {
        return super::remote::install_engram(&choice, log).map(drop);
    }
    if let Some(why) = engram_bin_problem() {
        return Err(why);
    }
    if !cfg!(windows)
        && let Some(brew) = brew()
    {
        // Newer Homebrew wants third-party taps trusted; older ones lack the command.
        let _ = output_of(&brew, &["trust", "gentleman-programming/tap"]);
        let mut command = pi_rpc::command(&brew);
        command.args(["install", "gentleman-programming/tap/engram"]);
        log("Installing Engram with Homebrew…".into());
        match run_logged(command, "brew install", log) {
            Ok(()) => return Ok(()),
            Err(e) => log(format!("{e} Downloading the release instead.")),
        }
    }
    fetch::install_engram_release(log)?;
    refresh_paths();
    match check_engram() {
        (Status::Ok(_), _) => Ok(()),
        // An `ENGRAM_BIN` or an older install earlier on the PATH still wins.
        (Status::Outdated(why), _) => {
            Err(format!("Engram was installed, but another one runs: {why}"))
        }
        _ => Err("Engram was installed, but it doesn't run.".into()),
    }
}

/// The version `engram version` reports, as `X.Y.Z` (where agents run).
pub fn engram_version() -> Option<String> {
    let text = match super::remote::backend() {
        Some(choice) => super::remote::engram_version_text(&choice)?,
        None => engram().and_then(|e| output_of(&e, &["version"]))?,
    };
    engram_version_of(&text)
}

pub(super) fn engram_version_of(text: &str) -> Option<String> {
    let word = text.lines().next()?.split_whitespace().last()?;
    let version = word.strip_prefix('v').unwrap_or(word);
    gentle_core::version::stable_parts(version).map(|_| version.to_string())
}

/// Update Engram the way it was installed: `brew upgrade` for Homebrew's,
/// else the latest release binary into the app's toolchain (which comes
/// first on the app's PATH). The version it runs afterwards.
pub fn upgrade_engram(log: &Log) -> Result<String, String> {
    if let Some(choice) = super::remote::backend() {
        return super::remote::install_engram(&choice, log);
    }
    if let Some(why) = engram_bin_problem() {
        return Err(why);
    }
    let installed = engram().ok_or("Engram isn't installed.")?;
    let real = dunce::canonicalize(&installed).unwrap_or(installed);
    let from_brew = real.components().any(|c| c.as_os_str() == "Cellar");
    match brew().filter(|_| from_brew && !cfg!(windows)) {
        Some(brew) => {
            let _ = output_of(&brew, &["trust", "gentleman-programming/tap"]);
            let mut command = pi_rpc::command(&brew);
            command.args(["upgrade", "gentleman-programming/tap/engram"]);
            log("Updating Engram with Homebrew…".into());
            run_logged(command, "brew upgrade", log)?;
        }
        None => fetch::install_engram_release(log)?,
    }
    refresh_paths();
    engram_version().ok_or_else(|| "Engram was updated, but it doesn't run.".into())
}

// ── Provider and Claude Code ───────────────────────────────────────────────

fn json_keys(path: &Path, field: Option<&str>) -> Vec<String> {
    let Some(value) = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return Vec::new();
    };
    let value = field.map_or(&value, |f| &value[f]);
    value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

const ENV_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "OPENROUTER_API_KEY",
    "GROQ_API_KEY",
    "XAI_API_KEY",
    "MISTRAL_API_KEY",
];

/// `claude_login`: Claude Code's sign-in, when the bridge can use it.
pub fn check_provider(agent: &Path, claude_login: Option<bool>) -> (Status, Fix) {
    let mut found = json_keys(&agent.join("auth.json"), None);
    found.extend(json_keys(&agent.join("models.json"), Some("providers")));
    if !found.is_empty() {
        return (
            Status::Ok(format!("Signed in: {}", found.join(", "))),
            Fix::None,
        );
    }
    if let Some(key) = ENV_KEYS
        .iter()
        .find(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
    {
        return (Status::Ok(format!("{key} is set")), Fix::None);
    }
    if bridge_version(agent).is_some_and(|v| v.is_some()) && claude_login == Some(true) {
        return (
            Status::Ok("Claude through your Claude Code sign-in".into()),
            Fix::None,
        );
    }
    (Status::Missing("No provider signed in".into()), Fix::SignIn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_git_command_per_distro() {
        let none = |_: &str| false;
        assert_eq!(
            linux_install_command("git", "ID=ubuntu\nID_LIKE=debian\n", none),
            "sudo apt install git"
        );
        assert_eq!(
            linux_install_command(
                "git",
                "ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\n",
                none
            ),
            "sudo dnf install git"
        );
        assert_eq!(
            linux_install_command("git", "ID=arch\n", none),
            "sudo pacman -S git"
        );
        assert_eq!(
            linux_install_command(
                "git",
                "ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n",
                none
            ),
            "sudo zypper install git"
        );
        assert_eq!(
            linux_install_command("git", "ID=unknown\n", |m| m == "pacman"),
            "sudo pacman -S git"
        );
        assert_eq!(
            linux_install_command("git", "", none),
            "sudo apt install git"
        );
    }

    #[test]
    fn engram_older_than_three_is_outdated() {
        let status = |text: &str| engram_status(text).0;
        assert_eq!(
            status("engram 1.18.0\nUpdate available: 1.18.0 -> 3.7.0\n"),
            Status::Outdated("Engram 1.18.0 — needs 3.0.0 or newer".into())
        );
        assert!(matches!(status("engram 2.9.9"), Status::Outdated(_)));
        assert_eq!(status("engram 3.0.0"), Status::Ok("engram 3.0.0".into()));
        assert_eq!(status("engram v3.7.0"), Status::Ok("engram v3.7.0".into()));
        assert_eq!(status("engram dev"), Status::Ok("engram dev".into()));
    }

    #[test]
    fn engram_version_reads_the_first_line() {
        let version = |text: &str| engram_version_of(text);
        assert_eq!(version("engram 3.6.2").as_deref(), Some("3.6.2"));
        assert_eq!(
            version("engram v3.6.2\nUpdate available: 3.6.2 -> 3.7.2").as_deref(),
            Some("3.6.2")
        );
        // A development build has no version to compare.
        assert_eq!(version("engram dev"), None);
    }

    #[test]
    fn engram_bin_is_judged_the_way_it_is_run() {
        // An absolute path to a program that runs: this test's own binary.
        let real = std::env::current_exe().expect("the test binary has a path");
        assert_eq!(stale_engram_bin(&real), None);

        // A bare name is resolved on the PATH, the way the override is run.
        let on_path = Path::new(if cfg!(windows) { "cmd" } else { "sh" });
        assert_eq!(stale_engram_bin(on_path), None);

        // A path that names nothing is reported, with the path in the message.
        let missing = std::env::temp_dir().join(format!("gs-no-engram-{}", std::process::id()));
        let why = stale_engram_bin(&missing).expect("a missing ENGRAM_BIN is reported");
        assert!(why.contains("ENGRAM_BIN"), "{why}");
        assert!(why.contains(&missing.display().to_string()), "{why}");

        // So is a name that is on no PATH, and a folder is not a program.
        assert!(stale_engram_bin(Path::new("gs-no-such-engram-anywhere")).is_some());
        assert!(stale_engram_bin(&std::env::temp_dir()).is_some());
    }
}
