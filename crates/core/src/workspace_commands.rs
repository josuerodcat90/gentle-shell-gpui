//! Typed commands and replies for the Desktop workspace CLI (no GPUI or IO).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const HELP: &str = "Usage: gsd workspace <command>\n\
  list [--json]                      List workspaces\n\
  current [--json]                   Show the current workspace\n\
  create <name> [folder]             Create and switch (default: .)\n\
  switch <name>                      Switch without stopping other agents\n\
  rename <name> <new-name>            Rename a workspace\n\
  set-root <name> <folder>           Change root for future tabs\n\
  remove <name> --force              Remove workspace (closes its tabs)\n\
  --help                             Show this help";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    List,
    Current,
    Create { name: String, root: PathBuf },
    Switch { name: String },
    Rename { name: String, new_name: String },
    SetRoot { name: String, root: PathBuf },
    Remove { name: String, force: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    Help,
    Run { command: Command, json: bool },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub name: String,
    pub root: PathBuf,
    pub active: bool,
    pub tabs: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Output {
    Workspaces(Vec<WorkspaceInfo>),
    Message(String),
}

pub type Reply = Result<Output, String>;

pub fn parse(args: &[String]) -> Result<Parsed, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (command, json) = match args.as_slice() {
        [] | ["--help" | "-h" | "help"] => return Ok(Parsed::Help),
        ["list"] => (Command::List, false),
        ["list", "--json"] => (Command::List, true),
        ["current"] => (Command::Current, false),
        ["current", "--json"] => (Command::Current, true),
        ["create", name] => (
            Command::Create {
                name: name.to_string(),
                root: ".".into(),
            },
            false,
        ),
        ["create", name, root] => (
            Command::Create {
                name: name.to_string(),
                root: PathBuf::from(*root),
            },
            false,
        ),
        ["switch", name] => (
            Command::Switch {
                name: name.to_string(),
            },
            false,
        ),
        ["rename", name, new_name] => (
            Command::Rename {
                name: name.to_string(),
                new_name: new_name.to_string(),
            },
            false,
        ),
        ["set-root", name, root] => (
            Command::SetRoot {
                name: name.to_string(),
                root: PathBuf::from(*root),
            },
            false,
        ),
        ["remove", name] => (
            Command::Remove {
                name: name.to_string(),
                force: false,
            },
            false,
        ),
        ["remove", name, "--force"] => (
            Command::Remove {
                name: name.to_string(),
                force: true,
            },
            false,
        ),
        _ => return Err("Invalid workspace command. Run gsd workspace --help.".into()),
    };
    Ok(Parsed::Run { command, json })
}

pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name == name.trim() && !name.chars().any(char::is_control)
}

pub fn select(infos: Vec<WorkspaceInfo>, current_only: bool) -> Reply {
    let infos: Vec<_> = infos
        .into_iter()
        .filter(|info| !current_only || info.active)
        .collect();
    if current_only && infos.is_empty() {
        return Err("No current workspace".into());
    }
    Ok(Output::Workspaces(infos))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_words(words: &[&str]) -> Result<Parsed, String> {
        parse(&words.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_paths_and_json() {
        assert_eq!(
            parse_words(&["create", "my project", "folder with spaces"]).unwrap(),
            Parsed::Run {
                command: Command::Create {
                    name: "my project".into(),
                    root: "folder with spaces".into(),
                },
                json: false,
            }
        );
        assert_eq!(
            parse_words(&["list", "--json"]).unwrap(),
            Parsed::Run {
                command: Command::List,
                json: true,
            }
        );
        assert!(parse_words(&["switch"]).is_err());
        assert!(parse_words(&["list", "--typo"]).is_err());
    }

    #[test]
    fn remove_requires_explicit_confirmation() {
        assert_eq!(
            parse_words(&["remove", "demo"]).unwrap(),
            Parsed::Run {
                command: Command::Remove {
                    name: "demo".into(),
                    force: false,
                },
                json: false,
            }
        );
        assert!(!valid_name(" "));
        assert!(!valid_name("bad\nname"));
        assert!(valid_name("proyecto español"));
    }

    #[test]
    fn protocol_round_trip() {
        let command = Command::SetRoot {
            name: "demo".into(),
            root: "/project".into(),
        };
        assert_eq!(
            serde_json::from_str::<Command>(&serde_json::to_string(&command).unwrap()).unwrap(),
            command
        );
        let reply: Reply = Err("not found".into());
        assert_eq!(
            serde_json::from_str::<Reply>(&serde_json::to_string(&reply).unwrap()).unwrap(),
            reply
        );
    }

    #[test]
    fn current_selects_only_active_workspace() {
        let active = WorkspaceInfo {
            name: "active".into(),
            root: "/active".into(),
            active: true,
            tabs: 2,
        };
        let inactive = WorkspaceInfo {
            name: "inactive".into(),
            root: "/inactive".into(),
            active: false,
            tabs: 1,
        };
        assert_eq!(
            select(vec![inactive.clone(), active.clone()], true),
            Ok(Output::Workspaces(vec![active]))
        );
        assert!(select(vec![inactive], true).is_err());
    }
}
