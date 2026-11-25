//! Terminal-facing workspace commands. The GUI owns all mutations.

use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::time::{Duration, Instant};

use gentle_core::workspace_commands::{
    self as protocol, Command, Output, Parsed, Reply, WorkspaceInfo,
};

use crate::{features::workspaces, platform::instance};

pub fn run(args: Vec<String>) -> i32 {
    // Release builds use the Windows GUI subsystem. Attach only in CLI mode.
    #[cfg(windows)]
    unsafe {
        let _ = windows::Win32::System::Console::AttachConsole(
            windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
    let (command, json) = match protocol::parse(&args) {
        Ok(Parsed::Help) => {
            println!("{}", protocol::HELP);
            return 0;
        }
        Ok(Parsed::Run { command, json }) => (command, json),
        Err(error) => {
            eprintln!("Error: {error}");
            return 2;
        }
    };
    let command = match prepare(command) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("Error: {error}");
            return 2;
        }
    };
    match execute(&command).and_then(|output| print_output(output, json)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Error: {error}");
            1
        }
    }
}

fn prepare(mut command: Command) -> Result<Command, String> {
    match &mut command {
        Command::Create { root, .. } | Command::SetRoot { root, .. } => {
            *root = canonical_folder(root)?;
        }
        Command::Remove { force: false, .. } => {
            return Err(
                "Removing a workspace closes its tabs and stops its agents. \
                 Repeat with --force to confirm; files and conversations stay on disk."
                    .into(),
            );
        }
        _ => {}
    }
    Ok(command)
}

fn canonical_folder(path: &Path) -> Result<PathBuf, String> {
    let folder = dunce::canonicalize(path)
        .map_err(|error| format!("Cannot open folder {}: {error}", path.display()))?;
    if !folder.is_dir() {
        return Err(format!("Not a directory: {}", folder.display()));
    }
    Ok(folder)
}

fn execute(command: &Command) -> Reply {
    if let Some(reply) = instance::workspace(command)? {
        return reply;
    }
    // Read-only commands can inspect the last saved state without opening a window.
    if matches!(command, Command::List | Command::Current) {
        let state = workspaces::load();
        let infos = state
            .workspaces
            .iter()
            .enumerate()
            .map(|(index, space)| WorkspaceInfo {
                name: space.name.clone(),
                root: space.root.clone(),
                active: index == state.current,
                tabs: space.tabs.len(),
            })
            .collect();
        return protocol::select(infos, matches!(command, Command::Current));
    }
    // No connection was made, so no command was sent. Start the normal app
    // and retry only until a connection exists; never retry an uncertain mutation.
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Process::new(exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not start Gentle Shell Desktop: {e}"))?;
    // The window outlives the command; don't block waiting for it to close.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(reply) = instance::workspace(command)? {
            return reply;
        }
    }
    Err("Gentle Shell Desktop did not start in time; open it and retry.".into())
}

fn print_output(output: Output, json: bool) -> Result<(), String> {
    match output {
        Output::Workspaces(infos) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&infos).map_err(|e| e.to_string())?
            );
        }
        Output::Workspaces(infos) => {
            if infos.is_empty() {
                println!("No saved workspaces.");
            } else {
                for info in infos {
                    let marker = if info.active { "*" } else { " " };
                    println!(
                        "{marker} {}\t{}\t{} tab(s)",
                        info.name,
                        info.root.display(),
                        info.tabs
                    );
                }
            }
        }
        Output::Message(message) => println!("{message}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_folder_is_not_a_fallback() {
        let path = std::env::temp_dir().join(format!("gsd-missing-{}", std::process::id()));
        assert!(
            prepare(Command::Create {
                name: "demo".into(),
                root: path,
            })
            .is_err()
        );
    }

    #[test]
    fn unconfirmed_remove_is_rejected_before_ipc() {
        assert!(
            prepare(Command::Remove {
                name: "demo".into(),
                force: false,
            })
            .is_err()
        );
    }
}
