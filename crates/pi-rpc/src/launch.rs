use anyhow::{Result, anyhow};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use crate::file_cache::Stamp;

/// A `Command` for a background child of this GUI app.
///
/// On Windows it sets `CREATE_NO_WINDOW`: a release build has no console, so
/// every console child (git, node, shells, the backend) would flash a console
/// window of its own, and a debug build's children would attach to the
/// launching terminal. It also starts in [`neutral_dir`] unless the caller
/// sets `current_dir`, so no child holds the app's install folder open.
/// Elsewhere it is plain `Command::new`.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = std::process::Command::new(program);
        command.creation_flags(CREATE_NO_WINDOW);
        if let Some(dir) = neutral_dir() {
            command.current_dir(dir);
        }
        command
    }
    #[cfg(not(windows))]
    std::process::Command::new(program)
}

/// A folder outside any install for this app and its children to work in on
/// Windows: the user's profile folder, else `System32`.
///
/// A process holds its working folder open, and Windows can't rename or
/// delete a folder a process works in, so a child (or the app itself)
/// started in the app's install folder would make the installer's update
/// fail with "Access is denied". The profile comes from the shell's known
/// folder, not `%USERPROFILE%`: gentle-ai's managed install points that
/// variable at a folder inside the install for its children.
#[cfg(windows)]
pub fn neutral_dir() -> Option<&'static Path> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        known_profile_dir()
            .or_else(system_dir)
            .filter(|dir| dir.is_dir())
    })
    .as_deref()
}

#[cfg(windows)]
fn known_profile_dir() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Profile, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    // SAFETY: the returned string is read once and then freed as documented.
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Profile, KF_FLAG_DEFAULT, None).ok()?;
        let dir = path.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(path.0 as *const _));
        dir.filter(|dir| dir.is_dir())
    }
}

#[cfg(windows)]
fn system_dir() -> Option<PathBuf> {
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buffer = [0u16; 260];
    // SAFETY: the buffer outlives the call, which writes at most its length.
    let len = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    (len > 0 && len < buffer.len()).then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..len])))
}

/// How to start one RPC backend process.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, OsString)>,
    /// The `gentle-shell` command a person runs for this install: shown in
    /// the app and named in its messages. `program` itself, except for
    /// gentle-ai's managed install, whose Node the app starts directly.
    pub launcher: PathBuf,
    /// The Node and `gentle-pi` package of a self-contained or managed
    /// install; `None` for a launcher (npm, pnpm) that finds its own.
    pub bundled: Option<BundledInstall>,
}

impl LaunchSpec {
    /// `gentle-shell <sub…>` as this install runs it: the same program and
    /// the arguments that precede Pi's (launcher options from
    /// `GENTLE_SHELL_ARGS`, or a managed install's entry script), in `cwd`
    /// with `env`.
    pub fn launcher_command<S: AsRef<OsStr>>(
        &self,
        sub: impl IntoIterator<Item = S>,
    ) -> std::process::Command {
        let mut command = command(&self.program);
        command
            .args(self.args.iter().take_while(|a| *a != "--mode"))
            .args(sub)
            .current_dir(&self.cwd);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }

    /// [`Self::launcher_command`] for a one-shot run no host answers
    /// (`--print`): without `GENTLE_SHELL_INTERACTIVE_HOST`, whether it is
    /// set here or passed to a backend as an `env` argument.
    pub fn print_command<S: AsRef<OsStr>>(
        &self,
        sub: impl IntoIterator<Item = S>,
    ) -> std::process::Command {
        const HOST: &str = "GENTLE_SHELL_INTERACTIVE_HOST";
        let mut command = command(&self.program);
        command
            .args(
                self.args
                    .iter()
                    .take_while(|a| *a != "--mode")
                    .filter(|a| a.split_once('=').is_none_or(|(key, _)| key != HOST)),
            )
            .args(sub)
            .current_dir(&self.cwd)
            .env_remove(HOST);
        for (key, value) in self.env.iter().filter(|(key, _)| key != HOST) {
            command.env(key, value);
        }
        command
    }

    /// gentle-ai installed this Gentle Shell and is what repairs and updates
    /// it (its TUI): `gentle-shell upgrade` and `setup` must not, and its
    /// launcher config is gentle-ai's to write.
    pub fn managed_by_gentle_ai(&self) -> bool {
        self.bundled.as_ref().is_some_and(|bundled| bundled.managed)
    }

    /// Whether a failed launcher must be repaired by its own installer.
    pub fn requires_installer_repair(&self) -> bool {
        let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        let candidates = bundled_candidates(
            &crate::home::home_dir(),
            local_app_data.as_deref(),
            cfg!(windows),
        );
        self.bundled.is_some() || protected_launcher(&self.launcher, &candidates)
    }
}

fn protected_launcher(launcher: &Path, candidates: &[PathBuf]) -> bool {
    let is_candidate = |path: &Path| {
        candidates.iter().any(|candidate| {
            path == candidate
                || (cfg!(windows)
                    && path
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&candidate.to_string_lossy()))
                || dunce::canonicalize(path)
                    .ok()
                    .zip(dunce::canonicalize(candidate).ok())
                    .is_some_and(|(path, candidate)| path == candidate)
        })
    };
    if is_candidate(launcher) {
        return true;
    }
    let Ok(script) = std::fs::read_to_string(launcher) else {
        return false;
    };
    if script.contains(BUNDLED_LAUNCHER_MARK) || supervisor_root(&script).is_some() {
        return true;
    }
    shim_target(&script).is_some_and(|target| {
        is_candidate(&target)
            || std::fs::read_to_string(target)
                .ok()
                .is_some_and(|binding| supervisor_root(&binding).is_some())
    })
}

/// Resolve `gentle-shell` and build the RPC launch for `cwd`.
///
/// Order: `$GENTLE_SHELL_BIN`, then an install in its own folder
/// ([`bundled_launcher`]), then `gentle-shell` on the PATH. GUI launches
/// (Finder, a desktop entry) inherit a minimal PATH without the user's Node
/// version manager, so the login PATH (`login_path`) is merged in first;
/// the child gets the same PATH so the launcher's `#!/usr/bin/env node`
/// resolves. gentle-ai's managed install starts its own Node instead.
pub fn resolve_backend(cwd: &Path) -> Result<LaunchSpec> {
    let path = search_path();
    let launcher = match chosen_launcher() {
        Some(launcher) => launcher,
        None => which::which_in("gentle-shell", Some(&path), cwd).map_err(|_| {
            anyhow!(
                "gentle-shell was not found on your PATH.\n\
                 Install it with `npm i -g gentle-pi`, or set GENTLE_SHELL_BIN to the launcher."
            )
        })?,
    };
    let prompt = SYSTEM_PROMPT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let extensions = EXTENSIONS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Ok(launch_spec(
        launcher,
        cwd,
        path,
        prompt.as_deref(),
        &extensions,
    ))
}

/// The RPC launch of `gentle-shell` in another environment (a WSL distro, a
/// Linux VM, a container). `launch` runs a program there in the project's
/// folder (`gentle_core::backend::Backend::launch`), `path` is that
/// environment's login PATH when known (its launcher's `#!/usr/bin/env node`
/// needs the user's Node), `env` is what the tab adds (already as the
/// backend reads it), and `map` gives each host file Pi reads (the app's
/// instructions, its extensions) a path in the backend (a copy there).
/// The host's
/// launcher, `GENTLE_SHELL_ARGS` and PATH belong to this computer and stay
/// out; the environment travels as `env` arguments, which `wsl.exe` and
/// other commands pass on where they don't forward their own environment.
pub fn remote_backend(
    launch: &[String],
    path: Option<&str>,
    env: &[(String, String)],
    map: impl Fn(&Path) -> Option<String>,
) -> Result<LaunchSpec> {
    let (program, prefix) = launch
        .split_first()
        .ok_or_else(|| anyhow!("the backend has no command to run"))?;
    let mut args = prefix.to_vec();
    args.extend([
        "env".to_string(),
        "GENTLE_SHELL_INTERACTIVE_HOST=1".to_string(),
    ]);
    args.extend(path.map(|path| format!("PATH={path}")));
    args.extend(env.iter().map(|(key, value)| format!("{key}={value}")));
    args.extend(["gentle-shell", "--mode", "rpc"].map(str::to_string));
    let host_file = |file: &Path| {
        map(file).ok_or_else(|| {
            anyhow!(
                "Couldn't copy {} into the backend, a file the app gives every agent.",
                file.display()
            )
        })
    };
    let prompt = SYSTEM_PROMPT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(file) = prompt {
        args.push("--append-system-prompt".to_string());
        args.push(host_file(&file)?);
    }
    let extensions = EXTENSIONS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for file in extensions {
        args.push("--extension".to_string());
        args.push(host_file(&file)?);
    }
    Ok(LaunchSpec {
        program: PathBuf::from(program),
        args,
        cwd: crate::home::home_dir(),
        env: Vec::new(),
        launcher: PathBuf::from("gentle-shell"),
        bundled: None,
    })
}

/// The RPC launch through `launcher`, with `path` as the child's PATH,
/// `prompt` as the host's instructions file and `extensions` as the host's
/// Pi extensions.
fn launch_spec(
    launcher: PathBuf,
    cwd: &Path,
    path: OsString,
    prompt: Option<&Path>,
    extensions: &[PathBuf],
) -> LaunchSpec {
    let managed = managed_install(&launcher);
    // Launcher options such as `--link` or `--home <dir>` go before the Pi
    // args. A managed install takes none: its supervisor refuses them too.
    let mut args = match &managed {
        Some(managed) => vec![managed.cli().to_string_lossy().into_owned()],
        None => extra_args(),
    };
    args.extend(["--mode".to_string(), "rpc".to_string()]);
    // The host's own instructions (how to open pages and devices in the
    // app), as a file: arguments through Windows' `.cmd` launcher mangle
    // quotes, `%` and newlines.
    if let Some(file) = prompt {
        args.push("--append-system-prompt".to_string());
        args.push(file.to_string_lossy().into_owned());
    }
    // The host's Pi extensions (the generative UI tool); the launcher passes
    // `--extension` through to Pi.
    for file in extensions {
        args.push("--extension".to_string());
        args.push(file.to_string_lossy().into_owned());
    }
    // Turns on ask_user_question dialogs and live helper activity for
    // interactive RPC hosts (gentle-shell docs: "Interactive RPC hosts").
    let mut env: Vec<(String, OsString)> =
        vec![("GENTLE_SHELL_INTERACTIVE_HOST".into(), "1".into())];
    match managed {
        Some(managed) => {
            env.extend(managed.env(&path));
            LaunchSpec {
                program: managed.node(),
                args,
                cwd: cwd.to_path_buf(),
                env,
                launcher: managed.binding(),
                bundled: Some(BundledInstall {
                    node: managed.node(),
                    package: managed.package(),
                    managed: true,
                }),
            }
        }
        None => {
            env.push(("PATH".into(), path));
            LaunchSpec {
                program: launcher.clone(),
                args,
                cwd: cwd.to_path_buf(),
                env,
                bundled: bundled_install(&launcher),
                launcher,
            }
        }
    }
}

/// The launcher chosen before the PATH is searched: `$GENTLE_SHELL_BIN`, else
/// an install in its own folder ([`bundled_launcher`]).
fn chosen_launcher() -> Option<PathBuf> {
    match std::env::var_os("GENTLE_SHELL_BIN") {
        Some(bin) if !bin.is_empty() => Some(PathBuf::from(bin)),
        _ => bundled_launcher(),
    }
}

/// gentle-ai's managed install, when it is the launcher chosen without
/// searching the PATH: the homes and the launcher config follow it.
pub(crate) fn chosen_managed_install() -> Option<ManagedInstall> {
    chosen_launcher().as_deref().and_then(managed_install)
}

/// The folder of gentle-ai's managed install
/// (`%LOCALAPPDATA%\gentle-shell\install`) when it is the chosen launcher,
/// which its installer replaces whole on an update.
pub fn managed_install_root() -> Option<PathBuf> {
    chosen_managed_install().map(|install| install.root)
}

/// Gentle Shell installed in its own folder, when present: the self-contained
/// install its installer and `gentle-shell upgrade` (since 4.7) set up, with
/// its own Node and Pi, or on Windows gentle-ai's managed install. It wins
/// over a global npm `gentle-pi` found on the PATH, which upgrading leaves
/// behind and which may come first there.
pub fn bundled_launcher() -> Option<PathBuf> {
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    bundled_candidates(
        &crate::home::home_dir(),
        local_app_data.as_deref(),
        cfg!(windows),
    )
    .into_iter()
    .find(|launcher| launcher.is_file())
}

/// Where those installs keep their launcher: `~/.gentle-shell/bin` on macOS
/// and Linux; on Windows `%LOCALAPPDATA%\gentle-shell\bin`, or
/// `%USERPROFILE%\.gentle-shell-bundle\bin` when the first folder was
/// refused, then gentle-ai's managed install (its command binding in
/// `%LOCALAPPDATA%\gentle-shell\install\bin`, gentle-ai
/// `DiscoverUserShellInstalls`).
fn bundled_candidates(home: &Path, local_app_data: Option<&Path>, windows: bool) -> Vec<PathBuf> {
    if windows {
        let launcher = Path::new("bin").join("gentle-shell.cmd");
        let own = local_app_data.map(|dir| dir.join("gentle-shell"));
        own.iter()
            .map(|dir| dir.join(&launcher))
            .chain([home.join(".gentle-shell-bundle").join(&launcher)])
            .chain(own.iter().map(|dir| dir.join("install").join(&launcher)))
            .collect()
    } else {
        vec![home.join(".gentle-shell").join("bin").join("gentle-shell")]
    }
}

/// The first line Gentle Shell writes into its self-contained install's
/// generated launcher (`scripts/bundled-install.mjs`, `LAUNCHER_MARK`).
const BUNDLED_LAUNCHER_MARK: &str = "gentle-shell bundled launcher (generated; do not edit)";

/// What a self-contained or managed install runs: its own Node and the
/// active `gentle-pi` package.
#[derive(Debug, Clone)]
pub struct BundledInstall {
    pub node: PathBuf,
    pub package: PathBuf,
    /// gentle-ai's managed install ([`LaunchSpec::managed_by_gentle_ai`]).
    pub managed: bool,
}

/// `launcher` as Gentle Shell's self-contained install (4.7) lays it out:
/// `<prefix>/bin/gentle-shell[.cmd]` running `<prefix>/runtime/node-<v>`'s
/// Node (`bin/node`, or `node.exe` on Windows) with the active version,
/// `<prefix>/current` (a link, or on Windows a file naming the folder in
/// `<prefix>/versions`). `None` for any other launcher.
pub fn bundled_install(launcher: &Path) -> Option<BundledInstall> {
    let script = std::fs::read_to_string(launcher).ok()?;
    if !script.contains(BUNDLED_LAUNCHER_MARK) {
        return None;
    }
    let prefix = launcher.parent()?.parent()?;
    let runtime = node_runtime_name(&script)?;
    let runtime = prefix.join("runtime").join(runtime);
    let node = [runtime.join("bin").join("node"), runtime.join("node.exe")]
        .into_iter()
        .find(|node| node.is_file())?;
    let current = prefix.join("current");
    let version = if current.is_dir() {
        current
    } else {
        let name = std::fs::read_to_string(&current).ok()?;
        prefix.join("versions").join(name.trim())
    };
    let package = version.join("node_modules").join("gentle-pi");
    package.is_dir().then_some(BundledInstall {
        node,
        package,
        managed: false,
    })
}

/// `node-<version>` from the launcher's `…runtime/node-<version>/…` path.
fn node_runtime_name(script: &str) -> Option<&str> {
    script.match_indices("runtime").find_map(|(at, _)| {
        let rest = script[at + "runtime".len()..].strip_prefix(['/', '\\'])?;
        let name = &rest[..rest.find(['/', '\\'])?];
        name.starts_with("node-").then_some(name)
    })
}

/// gentle-ai's managed Gentle Shell on Windows (its TUI's "Separate"
/// install, gentle-ai `internal/shellinstaller`): one folder,
/// `%LOCALAPPDATA%\gentle-shell\install`, with its own Node, `gentle-pi`,
/// agent home and config. Its `bin\gentle-shell.cmd` runs `supervisor.exe
/// shell launch <root> gentle-shell`, which checks the folder and starts
/// `runtime\node\node.exe prefix\node_modules\gentle-pi\bin\gentle-shell.mjs`
/// in an environment it builds from scratch (`userWindowsLaunchEnvironment`);
/// the `gentle-shell.cmd` it publishes in WindowsApps, on the PATH, only
/// `call`s that binding.
///
/// The app starts that Node itself, in the same environment: through the
/// supervisor, `GENTLE_SHELL_INTERACTIVE_HOST` (Gentle Desktop, dialogs) and
/// the app's PATH would never reach Gentle Shell, and the launcher's own
/// subcommands (`--version`, `setup`) would go to Pi as arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedInstall {
    root: PathBuf,
}

impl ManagedInstall {
    fn node(&self) -> PathBuf {
        self.root.join("runtime").join("node").join("node.exe")
    }

    fn package(&self) -> PathBuf {
        self.root
            .join("prefix")
            .join("node_modules")
            .join("gentle-pi")
    }

    /// gentle-ai `userWindowsLaunchCLI`.
    fn cli(&self) -> PathBuf {
        self.package().join("bin").join("gentle-shell.mjs")
    }

    /// gentle-ai `userWindowsBinding`.
    fn binding(&self) -> PathBuf {
        self.root.join("bin").join("gentle-shell.cmd")
    }

    fn agent(&self) -> PathBuf {
        self.root.join("agent")
    }

    fn config(&self) -> PathBuf {
        self.root.join("config")
    }

    /// Its launcher config, with the setup marker its installer records.
    pub(crate) fn config_file(&self) -> PathBuf {
        self.config().join("gentle-shell.json")
    }

    /// Where its Gentle Shell keeps its state: `GENTLE_SHELL_HOME` (the
    /// isolated home its config selects) and `GENTLE_PI_CONFIG_HOME`.
    pub(crate) fn homes(&self) -> crate::Homes {
        crate::Homes {
            agent: self.agent(),
            config: self.config(),
        }
    }

    /// gentle-ai `userWindowsEnvironment(root)` for its launch, with `path`
    /// (the app's PATH) after the install's own runtime folders instead of
    /// only `System32`. The rest of the app's environment is inherited.
    fn env(&self, path: &OsStr) -> Vec<(String, OsString)> {
        let root = &self.root;
        let (home, agent, config) = (root.join("home"), self.agent(), self.config());
        let own = [
            root.join("runtime").join("node"),
            root.join("runtime").join("go").join("bin"),
            root.join("runtime").join("codegraph").join("bin"),
        ];
        let path = merge_dirs(own.into_iter().chain(std::env::split_paths(path)))
            .unwrap_or_else(|| path.to_os_string());
        [
            ("HOME", home.clone().into_os_string()),
            ("USERPROFILE", home.clone().into_os_string()),
            ("APPDATA", config.clone().into_os_string()),
            ("LOCALAPPDATA", root.join("state").into_os_string()),
            ("TEMP", root.join("tmp").into_os_string()),
            ("TMP", root.join("tmp").into_os_string()),
            ("GENTLE_PI_CONFIG_HOME", config.clone().into_os_string()),
            ("PI_CODING_AGENT_DIR", agent.clone().into_os_string()),
            ("GENTLE_PI_AGENT_HOME", agent.clone().into_os_string()),
            ("GENTLE_PI_NO_SKILL_REGISTRY", "1".into()),
            ("GENTLE_SHELL_HOME", agent.into_os_string()),
            ("GENTLE_SHELL_CONFIG", self.config_file().into_os_string()),
            ("GOPATH", home.join("go").into_os_string()),
            (
                "GOMODCACHE",
                home.join("go").join("pkg").join("mod").into_os_string(),
            ),
            (
                "GOCACHE",
                root.join("state").join("go-build").into_os_string(),
            ),
            ("PATH", path),
            ("NPM_CONFIG_PREFIX", root.join("prefix").into_os_string()),
            ("NPM_CONFIG_IGNORE_SCRIPTS", "true".into()),
            ("NPM_CONFIG_AUDIT", "false".into()),
            ("NPM_CONFIG_FUND", "false".into()),
            (
                "NPM_CONFIG_USERCONFIG",
                config.join("user.npmrc").into_os_string(),
            ),
            (
                "NPM_CONFIG_GLOBALCONFIG",
                config.join("global.npmrc").into_os_string(),
            ),
            (
                "NPM_CONFIG_CACHE",
                root.join("runtime").join("cache").into_os_string(),
            ),
            ("NODE_USE_SYSTEM_CA", "1".into()),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
    }
}

/// `launcher` as gentle-ai's managed install: its command binding, or a
/// shim (`gentle-shell.cmd` in WindowsApps) that `call`s one, into a folder
/// with the supervisor, its manifest, its Node and its `gentle-pi`. `None`
/// for any other launcher.
pub(crate) fn managed_install(launcher: &Path) -> Option<ManagedInstall> {
    let script = std::fs::read_to_string(launcher).ok()?;
    let binding = match shim_target(&script) {
        Some(target) => std::fs::read_to_string(target).ok()?,
        None => script,
    };
    let install = ManagedInstall {
        root: supervisor_root(&binding)?,
    };
    let laid_out = [
        install.root.join("supervisor.exe"),
        install.root.join("manifest.json"),
        install.node(),
        install.cli(),
    ]
    .iter()
    .all(|file| file.is_file());
    laid_out.then_some(install)
}

/// The binding a WindowsApps shim forwards to: `call "<binding>" %*`
/// (gentle-ai `userWindowsLauncherShim`).
fn shim_target(script: &str) -> Option<PathBuf> {
    script.lines().find_map(|line| {
        let (target, _) = line.trim().strip_prefix("call \"")?.split_once('"')?;
        Some(PathBuf::from(target))
    })
}

/// `<root>` from a managed install's command binding:
/// `"<root>\supervisor.exe" shell launch "<root>" gentle-shell %*`
/// (gentle-ai `userWindowsBinding`).
fn supervisor_root(script: &str) -> Option<PathBuf> {
    script.lines().find_map(|line| {
        let (supervisor, rest) = line.trim().strip_prefix('"')?.split_once('"')?;
        let name = supervisor.rsplit(['\\', '/']).next()?;
        if !name.eq_ignore_ascii_case("supervisor.exe") {
            return None;
        }
        let (root, product) = rest
            .trim_start()
            .strip_prefix("shell launch \"")?
            .split_once('"')?;
        (product.split_whitespace().next() == Some("gentle-shell")).then(|| PathBuf::from(root))
    })
}

/// The Node script `launcher` runs, by its real path: the launcher itself
/// when it is (a link to) one, as npm and yarn link `<prefix>/bin/gentle-shell`
/// on macOS and Linux, else the script a package manager's shim starts
/// ([`shim_script`]): npm's, pnpm's or yarn's `gentle-shell.cmd` on Windows,
/// pnpm's `gentle-shell` everywhere. Links are followed to the end, so a pnpm
/// global install lands in its store (`…/links/@/gentle-pi/<v>/<hash>/
/// node_modules/gentle-pi/bin/gentle-shell.mjs`). `None` when the launcher
/// is neither.
pub(crate) fn launcher_script(launcher: &Path) -> Option<PathBuf> {
    let real = dunce::canonicalize(launcher).ok()?;
    if is_node_script(&real.to_string_lossy()) {
        return Some(real);
    }
    let shim = std::fs::read_to_string(&real).ok()?;
    dunce::canonicalize(shim_script(&shim, real.parent()?)?).ok()
}

/// `path` names a Node script (`.mjs`, `.cjs`, `.js`), whatever its separators.
fn is_node_script(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, ext)| {
        ["mjs", "cjs", "js"]
            .iter()
            .any(|js| ext.eq_ignore_ascii_case(js))
    })
}

/// The script a package manager's shim for a Node bin starts, from the shim's
/// text and the folder it is in (`@zkochan/cmd-shim`, which pnpm and yarn
/// use, and npm's `cmd-shim`):
/// - the POSIX shim pnpm writes records it: `# cmd-shim-target=<absolute path>`;
/// - the `.cmd`, `.ps1` and older POSIX shims quote it relative to their own
///   folder: `"%~dp0\<path>"` (pnpm, which doubles `%` since 11, and yarn),
///   `"%dp0%\<path>"` (npm), `"$basedir/<path>"`, `"$basedir_win/<path>"`,
///   or absolute when it is on another drive.
fn shim_script(text: &str, dir: &Path) -> Option<PathBuf> {
    if let Some(target) = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("# cmd-shim-target="))
    {
        return Some(PathBuf::from(target.trim()));
    }
    text.lines()
        .flat_map(|line| line.split('"').skip(1).step_by(2))
        .filter(|quoted| is_node_script(quoted))
        .find_map(|quoted| {
            if let Some(path) = quoted.strip_prefix("%~dp0") {
                return Some(join_relative(dir, &path.replace("%%", "%")));
            }
            let relative = ["%dp0%", "$basedir_win", "$basedir"]
                .iter()
                .find_map(|base| quoted.strip_prefix(base));
            match relative {
                Some(path) => Some(join_relative(dir, path)),
                None => Path::new(quoted)
                    .is_absolute()
                    .then(|| PathBuf::from(quoted)),
            }
        })
}

/// `dir` joined with `relative`, a path with `\` or `/` separators.
fn join_relative(dir: &Path, relative: &str) -> PathBuf {
    relative
        .split(['\\', '/'])
        .filter(|part| !part.is_empty() && *part != ".")
        .fold(dir.to_path_buf(), |path, part| path.join(part))
}

/// Launcher options from `GENTLE_SHELL_ARGS`.
pub(crate) fn extra_args() -> Vec<String> {
    std::env::var("GENTLE_SHELL_ARGS")
        .map(|extra| split_args(&extra))
        .unwrap_or_default()
}

/// Split like a shell does for the common cases: whitespace separates,
/// `"…"` or `'…'` keeps spaces inside one argument (`--home "C:\Users\Ana
/// Li\gs"`) and the quotes go. A backslash stays literal: on Windows it is
/// the path separator, not an escape.
pub fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    // An argument started, even if empty (`""`).
    let mut started = false;
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

/// `node` on the merged search PATH (Gentle Shell requires it).
pub fn resolve_node(cwd: &Path) -> Result<PathBuf> {
    which::which_in("node", Some(search_path()), cwd)
        .map_err(|_| anyhow::anyhow!("node was not found on your PATH"))
}

/// `name` on the merged search PATH (with the app's extra dirs first).
pub fn find_program(name: &str) -> Option<PathBuf> {
    let cwd = std::env::temp_dir();
    which::which_in(name, Some(search_path()), cwd).ok()
}

/// The merged search PATH handed to every child.
pub fn child_path() -> OsString {
    search_path()
}

/// Directories searched before everything else (toolchains the app installed
/// into folders it owns). Replaces the previous list.
pub fn set_extra_path(dirs: Vec<PathBuf>) {
    *EXTRA_PATH.lock().unwrap_or_else(|e| e.into_inner()) = dirs;
}

static SYSTEM_PROMPT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// A file Pi appends to every new RPC session's system prompt
/// (`--append-system-prompt`), or `None`. Sessions already running keep
/// what they started with.
pub fn set_system_prompt(file: Option<PathBuf>) {
    *SYSTEM_PROMPT.lock().unwrap_or_else(|e| e.into_inner()) = file;
}

static EXTENSIONS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Pi extension files loaded into every new RPC session (`--extension`).
/// Replaces the previous list; sessions already running keep theirs.
pub fn set_extensions(files: Vec<PathBuf>) {
    *EXTENSIONS.lock().unwrap_or_else(|e| e.into_inner()) = files;
}

/// Forget everything resolved from the environment: cached `--version`
/// output and the login PATH (both recomputed on next use), after
/// something was installed.
pub fn forget_cached() {
    forget_versions();
    *BASE_PATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The launcher's own version line (`gentle-shell 4.6.0` → `4.6.0`).
pub fn backend_version(spec: &LaunchSpec) -> Option<String> {
    let text = version_output(spec)?;
    let line = text.lines().find(|l| l.starts_with("gentle-shell "))?;
    Some(line.trim_start_matches("gentle-shell ").trim().to_string())
}

/// Which launcher build, and which home config, a cached `--version` ran against.
#[derive(Clone, Copy, PartialEq, Eq)]
struct VersionKey {
    program: Stamp,
    /// `~/.gentle-shell/config.json`: its `home` line follows `gentle-shell home`.
    config: Option<Stamp>,
}

static VERSIONS: Mutex<BTreeMap<PathBuf, (VersionKey, String)>> = Mutex::new(BTreeMap::new());
/// Held while a probe runs, so concurrent tab starts share one run.
static VERSION_RUN: Mutex<()> = Mutex::new(());

/// Everything `<launcher> --version` prints on stdout, cached per launcher
/// until the launcher file (an upgrade) or the home config changes. For a
/// self-contained or managed install that file is its `gentle-pi`'s
/// `package.json`: an update replaces the package, not the launcher or Node.
pub fn version_output(spec: &LaunchSpec) -> Option<String> {
    let identity = spec.bundled.as_ref().map_or_else(
        || spec.program.clone(),
        |bundled| bundled.package.join("package.json"),
    );
    let program = dunce::canonicalize(&identity).unwrap_or(identity);
    let key = || {
        Some(VersionKey {
            program: Stamp::of(&program)?,
            config: Stamp::of(&crate::home::launcher_config_path()),
        })
    };
    let cached = |key: VersionKey| {
        let versions = VERSIONS.lock().unwrap_or_else(|e| e.into_inner());
        versions
            .get(&program)
            .filter(|(k, _)| *k == key)
            .map(|(_, text)| text.clone())
    };
    if let Some(text) = key().and_then(cached) {
        return Some(text);
    }
    let _run = VERSION_RUN.lock().unwrap_or_else(|e| e.into_inner());
    // Stat before running, so a change mid-run only costs another run later.
    let key = key();
    if let Some(text) = key.and_then(cached) {
        return Some(text);
    }
    let text = run_version(spec)?;
    if let Some(key) = key {
        VERSIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(program, (key, text.clone()));
    }
    Some(text)
}

/// Drop every cached `--version` (after `gentle-shell upgrade`).
pub fn forget_versions() {
    VERSIONS.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// How long `--version` may take: a Node start on a slow, busy machine.
const VERSION_TIMEOUT: Duration = Duration::from_secs(30);

fn run_version(spec: &LaunchSpec) -> Option<String> {
    let mut command = spec.launcher_command(["--version"]);
    command.stdin(Stdio::null()).stderr(Stdio::null());
    output_within(&mut command, VERSION_TIMEOUT).map(|(_, stdout)| stdout)
}

/// Re-run a failed version probe to capture bounded diagnostics, without
/// changing the cached stdout or the normal probe's treatment of stderr.
pub fn version_stderr(spec: &LaunchSpec) -> Option<String> {
    // Shorter than VERSION_TIMEOUT: a hung launcher already cost one full wait.
    version_stderr_within(spec, Duration::from_secs(10))
}

fn version_stderr_within(spec: &LaunchSpec, timeout: Duration) -> Option<String> {
    use std::io::Read as _;

    const STDERR_LIMIT: usize = 16 * 1024;
    let (mut reader, writer) = std::io::pipe().ok()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut remaining = STDERR_LIMIT;
        while remaining > 0 {
            let Ok(n) = reader.read(&mut buf[..remaining.min(4096)]) else {
                break;
            };
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
            remaining -= n;
        }
    });
    let mut command = spec.launcher_command(["--version"]);
    command.stdin(Stdio::null()).stderr(Stdio::from(writer));
    let _ = output_within(&mut command, timeout);
    drop(command);
    let deadline = std::time::Instant::now() + DRAIN_GRACE;
    let mut bytes = Vec::new();
    while let Ok(chunk) =
        rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
    {
        bytes.extend_from_slice(&chunk);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// After the child exits, how long to wait for its stdout to close: a
/// process it left in the background may hold the pipe open for good.
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

/// Run `command` with its stdout piped and return whether it succeeded and
/// what it printed; `None` when it can't start or outlives `timeout` (it is
/// killed then). Stdout is read on a thread of its own, so neither a full
/// pipe nor a background process that inherited it (an ssh-agent started by
/// a shell's rc file) can keep the caller waiting past the deadline.
pub fn output_within(
    command: &mut std::process::Command,
    timeout: std::time::Duration,
) -> Option<(bool, String)> {
    use std::io::Read as _;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Instant;

    let mut child = command.stdout(std::process::Stdio::piped()).spawn().ok()?;
    // A run killed at its deadline may leave processes it started; they end
    // with the app at the latest (Windows; elsewhere a no-op).
    crate::job::contain(&child);
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    let mut exited: Option<(std::process::ExitStatus, Instant)> = None;
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(20)) {
            Ok(chunk) => out.extend_from_slice(&chunk),
            // Stdout closed: everything it printed is in.
            Err(RecvTimeoutError::Disconnected) if exited.is_some() => break,
            Err(_) => {}
        }
        if exited.is_none()
            && let Ok(Some(status)) = child.try_wait()
        {
            exited = Some((status, Instant::now()));
        }
        match exited {
            Some((_, at)) if at.elapsed() >= DRAIN_GRACE => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            _ => {}
        }
    }
    // Late chunks that arrived in the same instant.
    while let Ok(chunk) = rx.try_recv() {
        out.extend_from_slice(&chunk);
    }
    let (status, _) = exited?;
    Some((status.success(), String::from_utf8_lossy(&out).into_owned()))
}

static EXTRA_PATH: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The login PATH merged ahead of the inherited one, computed on first use
/// and again after `forget_cached`.
static BASE_PATH: Mutex<Option<OsString>> = Mutex::new(None);

fn base_path() -> OsString {
    let login = login_path();
    let current = std::env::var_os("PATH").unwrap_or_default();
    // Last, filling a gap only: where `go install` puts programs (gentle-ai,
    // Engram), missing from the PATH when Go's installer didn't add it.
    let go = go_bin_dir(
        std::env::var_os("GOBIN"),
        std::env::var_os("GOPATH"),
        &crate::home::home_dir(),
    );
    let dirs = login
        .iter()
        .flat_map(std::env::split_paths)
        .chain(std::env::split_paths(&current))
        .chain(go.is_dir().then_some(go));
    merge_dirs(dirs).unwrap_or(current)
}

/// Where `go install` puts programs (`go help environment`): `GOBIN`, else
/// the first `GOPATH` entry's `bin`, else `~/go/bin` (`%USERPROFILE%\go\bin`).
fn go_bin_dir(gobin: Option<OsString>, gopath: Option<OsString>, home: &Path) -> PathBuf {
    if let Some(gobin) = gobin.filter(|dir| !dir.is_empty()) {
        return PathBuf::from(gobin);
    }
    gopath
        .as_deref()
        .and_then(|gopath| std::env::split_paths(gopath).find(|dir| !dir.as_os_str().is_empty()))
        .unwrap_or_else(|| home.join("go"))
        .join("bin")
}

/// `dirs` in order, each once, as one PATH value.
fn merge_dirs(dirs: impl IntoIterator<Item = PathBuf>) -> Option<OsString> {
    let mut unique: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        if !unique.contains(&dir) {
            unique.push(dir);
        }
    }
    std::env::join_paths(unique).ok()
}

fn search_path() -> OsString {
    // Held while the login PATH is read, so concurrent callers share one run.
    let base = BASE_PATH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(base_path)
        .clone();
    let extra = EXTRA_PATH.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if extra.is_empty() {
        return base;
    }
    merge_dirs(extra.into_iter().chain(std::env::split_paths(&base))).unwrap_or(base)
}

/// The PATH a session started now would get, which the inherited one may
/// lack: on Unix the login shell's (a GUI launch inherits a minimal PATH);
/// on Windows the one saved in the registry (an install since this app,
/// or what launched it, started).
fn login_path() -> Option<OsString> {
    if cfg!(windows) {
        registry_path()
    } else {
        login_shell_path()
    }
}

fn login_shell_path() -> Option<OsString> {
    const START: &str = "__GENTLE_PATH_START__";
    const END: &str = "__GENTLE_PATH_END__";
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    // Interactive login so rc files that set up nvm/fnm/mise run; markers fence
    // the value from anything those files print.
    let mut command = crate::command(&shell);
    command
        .args([
            "-l",
            "-i",
            "-c",
            &format!("printf '{START}%s{END}' \"$PATH\""),
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let (_, out) = output_within(&mut command, Duration::from_secs(5))?;
    let start = out.find(START)? + START.len();
    let end = out[start..].find(END)? + start;
    Some(OsString::from(&out[start..end]))
}

/// Windows builds a new process's PATH from the machine's `Path`, then the
/// user's, as saved in the registry with their `%VARIABLES%` expanded. Read
/// with `reg query` (always in `System32`), each within 5 s.
fn registry_path() -> Option<OsString> {
    const KEYS: [&str; 2] = [
        r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        r"HKCU\Environment",
    ];
    let reg = std::env::var_os("SystemRoot").map_or_else(
        || PathBuf::from("reg.exe"),
        |root| PathBuf::from(root).join("System32").join("reg.exe"),
    );
    let entries: Vec<String> = KEYS
        .iter()
        .filter_map(|key| {
            let mut command = crate::command(&reg);
            command
                .args(["query", key, "/v", "Path"])
                .stdin(Stdio::null())
                .stderr(Stdio::null());
            let (ok, out) = output_within(&mut command, Duration::from_secs(5))?;
            if !ok {
                return None;
            }
            let value = reg_value(&out, "Path")?;
            Some(expand_vars(&value, |name| std::env::var(name).ok()))
        })
        .filter(|value| !value.is_empty())
        .collect();
    (!entries.is_empty()).then(|| OsString::from(entries.join(";")))
}

/// `name`'s string data in `reg query` output, whose value lines read
/// `    <name>    REG_SZ|REG_EXPAND_SZ    <data>`.
fn reg_value(out: &str, name: &str) -> Option<String> {
    out.lines().find_map(|line| {
        let (key, rest) = line.trim_start().split_once("    ")?;
        if !key.eq_ignore_ascii_case(name) {
            return None;
        }
        let rest = rest.trim_start();
        let (kind, data) = rest.split_once("    ").unwrap_or((rest.trim_end(), ""));
        matches!(kind, "REG_SZ" | "REG_EXPAND_SZ").then(|| data.trim_end().to_string())
    })
}

/// `%NAME%` replaced by `lookup(NAME)`; an unknown one stays as written,
/// as Windows' `ExpandEnvironmentStrings` leaves it.
fn expand_vars(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            out.push('%');
            rest = after;
            continue;
        };
        let name = &after[..end];
        match lookup(name).filter(|_| !name.is_empty()) {
            Some(value) => out.push_str(&value),
            None => {
                out.push('%');
                out.push_str(name);
                out.push('%');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::{Duration, SystemTime};

    #[test]
    fn launcher_args_keep_quoted_paths_with_spaces() {
        assert_eq!(split_args("--link"), ["--link"]);
        assert_eq!(split_args("  --home   /tmp/gs  "), ["--home", "/tmp/gs"]);
        // A Windows home with a space, backslashes kept as they are.
        assert_eq!(
            split_args(r#"--home "C:\Users\Ana Li\gs" --isolated"#),
            ["--home", r"C:\Users\Ana Li\gs", "--isolated"]
        );
        assert_eq!(split_args("--home='/a b'"), ["--home=/a b"]);
        assert_eq!(split_args(r#"--x """#), ["--x", ""]);
        assert!(split_args("   ").is_empty());
    }

    /// A backend's one-shot run keeps its PATH and the tab's variables but
    /// never the interactive host, which only an RPC host answers.
    #[test]
    fn print_runs_in_a_backend_without_the_interactive_host() {
        let launch = ["wsl.exe", "-d", "Ubuntu", "--cd", "/w", "--exec"].map(str::to_string);
        let spec = remote_backend(
            &launch,
            Some("/usr/bin"),
            &[("K".to_string(), "V".to_string())],
            |file| Some(file.display().to_string()),
        )
        .unwrap();
        let command = spec.print_command(["--print"]);
        let args: Vec<_> = command.get_args().map(|a| a.to_string_lossy()).collect();
        assert_eq!(
            args,
            [
                "-d",
                "Ubuntu",
                "--cd",
                "/w",
                "--exec",
                "env",
                "PATH=/usr/bin",
                "K=V",
                "gentle-shell",
                "--print"
            ]
        );
        assert!(
            spec.launcher_command(["--version"])
                .get_args()
                .any(|a| a == "GENTLE_SHELL_INTERACTIVE_HOST=1")
        );
    }

    /// A shell rc file that starts a daemon (ssh-agent, keychain) leaves it
    /// holding stdout: the run still returns once the shell itself exits.
    #[cfg(unix)]
    #[test]
    fn output_within_returns_while_a_background_child_holds_stdout() {
        let started = std::time::Instant::now();
        let mut command = crate::command("/bin/sh");
        command.args(["-c", "printf ready; sleep 30 &"]);
        let out = output_within(&mut command, std::time::Duration::from_secs(10));
        assert_eq!(out, Some((true, "ready".to_string())));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn output_within_kills_a_run_past_its_deadline() {
        let started = std::time::Instant::now();
        let mut command = crate::command("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let out = output_within(&mut command, std::time::Duration::from_millis(300));
        assert_eq!(out, None);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn bundled_install_lives_in_gentle_shells_own_folder() {
        let home = Path::new("/home/me");
        assert_eq!(
            bundled_candidates(home, None, false),
            [PathBuf::from("/home/me/.gentle-shell/bin/gentle-shell")]
        );
        // Windows: LOCALAPPDATA first, the fallback folder after it, then
        // gentle-ai's managed install.
        let local = Path::new("/home/me/AppData/Local");
        assert_eq!(
            bundled_candidates(home, Some(local), true),
            [
                local
                    .join("gentle-shell")
                    .join("bin")
                    .join("gentle-shell.cmd"),
                home.join(".gentle-shell-bundle")
                    .join("bin")
                    .join("gentle-shell.cmd"),
                local
                    .join("gentle-shell")
                    .join("install")
                    .join("bin")
                    .join("gentle-shell.cmd"),
            ]
        );
        assert_eq!(bundled_candidates(home, None, true).len(), 1);
    }

    fn set_mtime(path: &Path, secs: u64) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn extra_dirs_come_first_without_duplicates() {
        let base = OsString::from("/usr/bin:/opt/x/bin:/bin");
        let extra: Vec<PathBuf> = vec!["/opt/x/bin".into(), "/tc/node/bin".into()];
        let merged = merge_dirs(extra.into_iter().chain(std::env::split_paths(&base))).unwrap();
        assert_eq!(
            merged,
            OsString::from("/opt/x/bin:/tc/node/bin:/usr/bin:/bin")
        );
    }

    /// `go install github.com/…/gentle-ai` puts it in `~/go/bin` unless
    /// `GOBIN` or `GOPATH` say otherwise; that folder ends the search PATH.
    #[test]
    fn programs_installed_with_go_are_found() {
        let home = std::env::temp_dir().join(format!("pi-rpc-go-{}", std::process::id()));
        let bin = home.join("go").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let gentle_ai = bin.join("gentle-ai");
        std::fs::write(&gentle_ai, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&gentle_ai, std::fs::Permissions::from_mode(0o755)).unwrap();

        let go = go_bin_dir(None, None, &home);
        assert_eq!(go, bin);
        let inherited = OsString::from("/usr/bin:/bin");
        let path = merge_dirs(std::env::split_paths(&inherited).chain([go])).unwrap();
        assert_eq!(
            which::which_in("gentle-ai", Some(&path), &home).unwrap(),
            gentle_ai
        );

        assert_eq!(
            go_bin_dir(Some("/opt/gobin".into()), Some("/gp".into()), &home),
            PathBuf::from("/opt/gobin")
        );
        assert_eq!(
            go_bin_dir(Some("".into()), Some("/gp1:/gp2".into()), &home),
            PathBuf::from("/gp1/bin")
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn version_is_cached_until_the_launcher_changes() {
        let dir = std::env::temp_dir().join(format!("pi-rpc-version-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runs = dir.join("runs");
        let program = dir.join("gentle-shell");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\necho x >> '{}'\necho 'gentle-shell 1.2.3'\necho 'home isolated /h'\n",
                runs.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        set_mtime(&program, 3_000_000);
        let spec = LaunchSpec {
            program: program.clone(),
            args: Vec::new(),
            cwd: dir.clone(),
            env: Vec::new(),
            launcher: program.clone(),
            bundled: None,
        };
        let run_count = || std::fs::read_to_string(&runs).map_or(0, |s| s.lines().count());

        // Linux refuses to run a file another thread's child still holds open
        // for writing ("Text file busy"): another test spawning while this
        // one wrote the script leaks the descriptor until that child execs.
        // A refused run never starts the script and isn't cached, so retry.
        let first = (0..50).find_map(|_| {
            backend_version(&spec).or_else(|| {
                std::thread::sleep(std::time::Duration::from_millis(20));
                None
            })
        });
        assert_eq!(first.as_deref(), Some("1.2.3"));
        assert_eq!(
            version_output(&spec).as_deref(),
            Some("gentle-shell 1.2.3\nhome isolated /h\n")
        );
        assert_eq!(run_count(), 1);

        // An upgrade rewrites the launcher.
        set_mtime(&program, 3_000_001);
        assert_eq!(backend_version(&spec).as_deref(), Some("1.2.3"));
        assert_eq!(run_count(), 2);

        forget_versions();
        assert_eq!(backend_version(&spec).as_deref(), Some("1.2.3"));
        assert_eq!(run_count(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// gentle-ai's managed Windows install, as its installer lays it out
/// (`internal/shellinstaller`), in a temporary folder on any OS.
#[cfg(test)]
mod managed_tests {
    use super::*;

    struct Layout {
        dir: PathBuf,
        root: PathBuf,
        shim: PathBuf,
    }

    impl Drop for Layout {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn layout(name: &str) -> Layout {
        let dir =
            std::env::temp_dir().join(format!("pi-rpc-managed-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("gentle-shell").join("install");
        let write = |path: PathBuf, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        for file in [
            "supervisor.exe",
            "manifest.json",
            "main.json",
            "selection.json",
            "runtime/node/node.exe",
            "prefix/node_modules/gentle-pi/bin/gentle-shell.mjs",
            "prefix/node_modules/gentle-pi/package.json",
        ] {
            write(root.join(file), "");
        }
        // gentle-ai `userWindowsBinding` and `userWindowsLauncherShim`.
        let binding = root.join("bin").join("gentle-shell.cmd");
        write(
            binding.clone(),
            &format!(
                "@echo off\r\n\"{}\" shell launch \"{}\" gentle-shell %*\r\n",
                root.join("supervisor.exe").display(),
                root.display()
            ),
        );
        let shim = dir
            .join("Microsoft")
            .join("WindowsApps")
            .join("gentle-shell.cmd");
        write(
            shim.clone(),
            &format!("@echo off\r\ncall \"{}\" %*\r\n", binding.display()),
        );
        Layout { dir, root, shim }
    }

    fn env_of<'a>(spec: &'a LaunchSpec, key: &str) -> Option<&'a OsStr> {
        spec.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value.as_os_str())
    }

    #[test]
    fn the_managed_install_runs_its_own_node_in_its_own_homes() {
        let layout = layout("launch");
        let root = &layout.root;
        // Joined part by part: the arguments are strings, compared with the
        // platform's own separators.
        let cli = [
            "prefix",
            "node_modules",
            "gentle-pi",
            "bin",
            "gentle-shell.mjs",
        ]
        .iter()
        .fold(root.clone(), |path, part| path.join(part));
        let inherited = std::env::join_paths([PathBuf::from("/usr/bin")]).unwrap();
        // Found on the PATH (the WindowsApps shim) or in its own folder (the
        // command binding): the same launch.
        for launcher in [
            layout.shim.clone(),
            root.join("bin").join("gentle-shell.cmd"),
        ] {
            let spec = launch_spec(launcher, root, inherited.clone(), None, &[]);
            assert_eq!(spec.program, root.join("runtime/node/node.exe"));
            assert_eq!(spec.args, [&*cli.to_string_lossy(), "--mode", "rpc"]);
            assert_eq!(spec.launcher, root.join("bin").join("gentle-shell.cmd"));
            assert!(spec.managed_by_gentle_ai());
            assert_eq!(
                spec.bundled.as_ref().map(|b| b.package.clone()),
                Some(root.join("prefix/node_modules/gentle-pi"))
            );
            assert_eq!(
                env_of(&spec, "GENTLE_SHELL_INTERACTIVE_HOST"),
                Some(OsStr::new("1"))
            );
            // Where Pi and Gentle Shell keep their state is where the app
            // reads it.
            let homes = managed_install(&spec.launcher).unwrap().homes();
            assert_eq!(homes.agent, root.join("agent"));
            assert_eq!(homes.config, root.join("config"));
            for key in ["GENTLE_SHELL_HOME", "PI_CODING_AGENT_DIR"] {
                assert_eq!(env_of(&spec, key), Some(homes.agent.as_os_str()));
            }
            assert_eq!(
                env_of(&spec, "GENTLE_PI_CONFIG_HOME"),
                Some(homes.config.as_os_str())
            );
            assert_eq!(
                env_of(&spec, "GENTLE_SHELL_CONFIG"),
                Some(root.join("config").join("gentle-shell.json").as_os_str())
            );
            assert_eq!(env_of(&spec, "HOME"), Some(root.join("home").as_os_str()));
            // Its runtime first, then the app's PATH.
            let path: Vec<PathBuf> =
                std::env::split_paths(env_of(&spec, "PATH").unwrap()).collect();
            assert_eq!(path.first(), Some(&root.join("runtime").join("node")));
            assert_eq!(path.last(), Some(&PathBuf::from("/usr/bin")));
            // Launcher subcommands reach the launcher, not Pi.
            let version = spec.launcher_command(["--version"]);
            assert_eq!(version.get_program(), spec.program.as_os_str());
            let args: Vec<&OsStr> = version.get_args().collect();
            assert_eq!(args, [cli.as_os_str(), OsStr::new("--version")]);
        }
    }

    #[test]
    fn host_extensions_follow_the_system_prompt() {
        let layout = layout("extensions");
        let prompt = layout.dir.join("agent-instructions.md");
        let extension = layout.dir.join("gentle-ui.ts");
        let spec = launch_spec(
            layout.shim.clone(),
            &layout.root,
            OsString::new(),
            Some(&prompt),
            std::slice::from_ref(&extension),
        );
        assert_eq!(
            spec.args[1..],
            [
                "--mode".to_string(),
                "rpc".to_string(),
                "--append-system-prompt".to_string(),
                prompt.to_string_lossy().into_owned(),
                "--extension".to_string(),
                extension.to_string_lossy().into_owned(),
            ]
        );
    }

    #[test]
    fn only_a_complete_managed_install_is_recognized() {
        let layout = layout("partial");
        assert!(managed_install(&layout.shim).is_some());
        std::fs::remove_file(layout.root.join("supervisor.exe")).unwrap();
        assert!(managed_install(&layout.shim).is_none());
        // An npm or self-contained launcher is not one.
        let npm = layout.dir.join("npm-gentle-shell");
        std::fs::write(&npm, "#!/usr/bin/env node\n").unwrap();
        assert!(managed_install(&npm).is_none());
        let spec = launch_spec(npm.clone(), &layout.dir, OsString::new(), None, &[]);
        assert_eq!(spec.program, npm);
        assert!(!spec.managed_by_gentle_ai());
        // Another product of the supervisor is not Gentle Shell.
        assert_eq!(
            supervisor_root(r#""C:\i\supervisor.exe" shell launch "C:\i" pi %*"#),
            None
        );
        assert_eq!(
            supervisor_root(r#""C:\i\supervisor.exe" shell launch "C:\i" gentle-shell %*"#),
            Some(PathBuf::from(r"C:\i"))
        );
    }

    #[test]
    fn real_npm_and_pnpm_shims_are_not_protected() {
        let layout = layout("shims");
        for (name, body) in [
            (
                "npm.cmd",
                "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\gentle-pi\\bin\\gentle-shell.mjs\" %*\r\n",
            ),
            (
                "pnpm.cmd",
                "@SETLOCAL\r\n@IF EXIST \"%~dp0\\node.exe\" (\r\n  \"%~dp0\\node.exe\"  \"%~dp0\\..\\gentle-pi\\bin\\gentle-shell.mjs\" %*\r\n) ELSE (\r\n  node  \"%~dp0\\..\\gentle-pi\\bin\\gentle-shell.mjs\" %*\r\n)\r\n",
            ),
        ] {
            let path = layout.dir.join(name);
            std::fs::write(&path, body).unwrap();
            assert!(!protected_launcher(&path, &[]), "{name}");
        }
    }

    #[test]
    fn broken_generated_and_managed_launchers_are_protected() {
        let layout = layout("repair");
        let inherited = OsString::new();
        let generated = layout
            .dir
            .join("standalone")
            .join("bin")
            .join("gentle-shell.cmd");
        std::fs::create_dir_all(generated.parent().unwrap()).unwrap();
        std::fs::write(
            &generated,
            "@echo off\r\nrem gentle-shell bundled launcher (generated; do not edit)\r\n",
        )
        .unwrap();
        let spec = launch_spec(generated.clone(), &layout.dir, inherited.clone(), None, &[]);
        assert!(spec.bundled.is_none()); // No current version or runtime.
        assert!(spec.requires_installer_repair());

        std::fs::remove_file(layout.root.join("supervisor.exe")).unwrap();
        for launcher in [
            layout.shim.clone(),
            layout.root.join("bin/gentle-shell.cmd"),
        ] {
            let spec = launch_spec(launcher, &layout.dir, inherited.clone(), None, &[]);
            assert!(spec.bundled.is_none());
            assert!(spec.requires_installer_repair());
        }

        let candidate = layout.dir.join("candidate.cmd");
        std::fs::write(&candidate, "@echo off\r\n").unwrap();
        assert!(protected_launcher(
            &candidate,
            std::slice::from_ref(&candidate)
        ));
        let npm = layout.dir.join("npm.cmd");
        std::fs::write(
            &npm,
            "@node \"%dp0%\\node_modules\\gentle-pi\\bin\\gentle-shell.mjs\" %*\r\n",
        )
        .unwrap();
        let spec = launch_spec(npm.clone(), &layout.dir, inherited, None, &[]);
        assert!(!spec.requires_installer_repair());
        assert!(!protected_launcher(&npm, &[candidate]));
    }

    #[test]
    fn failed_version_diagnostics_capture_stderr_without_changing_version_output() {
        let layout = layout("version-error");
        let path = layout.dir.join(if cfg!(windows) {
            "broken.cmd"
        } else {
            "broken"
        });
        let script = if cfg!(windows) {
            "@echo off\r\necho gentle-shell: no active version 1>&2\r\nexit /b 1\r\n"
        } else {
            "#!/bin/sh\necho 'gentle-shell: no active version' >&2\nexit 1\n"
        };
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let spec = LaunchSpec {
            program: path.clone(),
            args: Vec::new(),
            cwd: layout.dir.clone(),
            env: Vec::new(),
            launcher: path,
            bundled: None,
        };
        assert_eq!(version_output(&spec).as_deref(), Some(""));
        assert_eq!(backend_version(&spec), None);
        assert_eq!(
            version_stderr(&spec).as_deref().map(str::trim),
            Some("gentle-shell: no active version")
        );
    }

    #[test]
    fn diagnostics_survive_a_version_probe_timing_out() {
        let layout = layout("version-timeout");
        // Sleep in the process we probe instead of busy-spinning or leaving
        // a separate sleeper behind when output_within kills the child.
        let (program, args) = if cfg!(windows) {
            (
                PathBuf::from("powershell.exe"),
                vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    // -Command consumes the remaining arguments, including
                    // the --version appended by launcher_command.
                    "[Console]::Error.WriteLine('broken before timeout'); Start-Sleep -Seconds 30; #"
                        .into(),
                ],
            )
        } else {
            (
                PathBuf::from("/bin/sh"),
                vec![
                    "-c".into(),
                    "echo 'broken before timeout' >&2; exec sleep 30".into(),
                ],
            )
        };
        let spec = LaunchSpec {
            launcher: program.clone(),
            program,
            args,
            cwd: layout.dir.clone(),
            env: Vec::new(),
            bundled: None,
        };
        // Allow for shell startup and scheduling under load. The child
        // sleeps much longer than this test-only deadline, so we still
        // exercise the timeout path.
        assert_eq!(
            version_stderr_within(&spec, Duration::from_secs(5))
                .as_deref()
                .map(str::trim),
            Some("broken before timeout")
        );
    }

    #[test]
    fn successful_version_output_is_still_cached() {
        let layout = layout("version-cache");
        let path = layout.dir.join(if cfg!(windows) {
            "working.cmd"
        } else {
            "working"
        });
        let runs = layout.dir.join("runs");
        let script = if cfg!(windows) {
            format!(
                "@echo off\r\necho x>>\"{}\"\r\necho gentle-shell 4.8.0\r\necho pi 1.5.2\r\necho warning 1>&2\r\n",
                runs.display()
            )
        } else {
            format!(
                "#!/bin/sh\necho x >> '{}'\necho 'gentle-shell 4.8.0'\necho 'pi 1.5.2'\necho warning >&2\n",
                runs.display()
            )
        };
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let spec = LaunchSpec {
            program: path.clone(),
            args: Vec::new(),
            cwd: layout.dir.clone(),
            env: Vec::new(),
            launcher: path,
            bundled: None,
        };
        let expected = if cfg!(windows) {
            "gentle-shell 4.8.0\r\npi 1.5.2\r\n"
        } else {
            "gentle-shell 4.8.0\npi 1.5.2\n"
        };
        assert_eq!(version_output(&spec).as_deref(), Some(expected));
        assert_eq!(version_output(&spec).as_deref(), Some(expected));
        assert_eq!(backend_version(&spec).as_deref(), Some("4.8.0"));
        assert_eq!(std::fs::read_to_string(runs).unwrap().lines().count(), 1);
    }

    #[test]
    fn windows_saved_path_is_read_and_expanded() {
        let out = "\r\nHKEY_CURRENT_USER\\Environment\r\n    Path    REG_EXPAND_SZ    %USERPROFILE%\\AppData\\Local\\Microsoft\\WindowsApps;C:\\Program Files\\Go\\bin;%USERPROFILE%\\go\\bin;%NOPE%\\x\r\n\r\n";
        let value = reg_value(out, "Path").unwrap();
        let lookup = |name: &str| (name == "USERPROFILE").then(|| r"C:\Users\Ana Li".to_string());
        assert_eq!(
            expand_vars(&value, lookup),
            r"C:\Users\Ana Li\AppData\Local\Microsoft\WindowsApps;C:\Program Files\Go\bin;C:\Users\Ana Li\go\bin;%NOPE%\x"
        );
        assert_eq!(
            reg_value("    PATH    REG_SZ    C:\\bin\r\n", "Path").as_deref(),
            Some(r"C:\bin")
        );
        assert_eq!(reg_value("    Path    REG_DWORD    0x1\r\n", "Path"), None);
        assert_eq!(expand_vars("50% off", lookup), "50% off");
    }
}
