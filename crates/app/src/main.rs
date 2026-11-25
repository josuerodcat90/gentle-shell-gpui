#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod cli;
mod features;
mod platform;
mod shared;
mod workspace_cli;

use features::setup;
use platform::instance;

fn main() {
    // `gentle-device …`, `gentle-browser …` and `gentle-screen …` (the
    // agents' commands) and `--diagnostics-report` run here and exit.
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("workspace") => std::process::exit(workspace_cli::run(args.collect())),
        Some("--device") => std::process::exit(features::devices::cli::run(args.collect())),
        Some("--browser") => std::process::exit(features::browser::cli::run(args.collect())),
        Some("--screen") => std::process::exit(features::screen::cli::run(args.collect())),
        Some(features::diagnostics::REPORT_FLAG) => std::process::exit(
            features::diagnostics::print_report(args.next().map(Into::into)),
        ),
        _ => {}
    }
    // An explicit folder (or `--cwd`, which `gsd` passes) selects or creates
    // the workspace rooted there; without one the last workspace is restored.
    let cli = cli::parse_cli();
    let folder = cli::requested_folder(&cli);
    let prompt = cli.prompt;
    let url = cli.url;
    if let Some(url) = url.as_deref()
        && (url.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")))
    {
        eprintln!("gentle-shell-gpui: --open-url expects an http(s) URL");
        std::process::exit(2);
    }
    // Only one app per user: a second launch hands its request over and exits.
    let request = instance::OpenRequest {
        folder: folder.clone(),
        prompt: prompt.clone(),
        url: url.clone(),
        device: None,
    };
    let requests = match instance::claim(&request) {
        instance::Role::Forwarded => {
            if shared::diagnostics::requested_at_launch() {
                eprintln!(
                    "gentle-shell-gpui: already running, so this launch records nothing; \
                     quit the app and launch it again to record diagnostics"
                );
            }
            return;
        }
        instance::Role::Primary(requests) => requests,
    };
    // Windows can't rename a folder a process works in, and a shortcut may
    // start the app in its install folder: work from a neutral folder so an
    // update can replace the install. The first tab still opens where the
    // app was launched (`cli::default_cwd`).
    cli::remember_launch_dir();
    #[cfg(windows)]
    {
        if let Some(dir) = pi_rpc::neutral_dir()
            && let Err(error) = std::env::set_current_dir(dir)
        {
            eprintln!(
                "Could not leave the launch folder for {}: {error}",
                dir.display()
            );
        }
    }
    // `gentle-open` and `gentle-device` live in the toolchain folder the
    // agents get on PATH; create them before that PATH is computed.
    platform::shims::install();
    platform::gsd::refresh();
    // What every agent is told about opening pages and devices in the app.
    pi_rpc::set_system_prompt(features::devices::agent_instructions(
        setup::devices::prefs(),
    ));
    // The generative UI extension (`gentle_ui` tool) every agent loads, the
    // `browser` and `screen` tools, and the canvas tools (active only in an
    // agent opened beside a canvas).
    pi_rpc::set_extensions(
        features::genui::install()
            .into_iter()
            .chain(features::browser::install_extension())
            .chain(features::screen::install_extension())
            .chain(features::canvas::install_extension())
            .collect(),
    );
    // Toolchains onboarding installed into the app's own folders.
    setup::apply_toolchain_path();
    // Gentle Shell 4.7's own install moved its config folder without copying
    // the old one: bring profiles, routing and preferences along once.
    match pi_rpc::adopt_standalone_config() {
        Ok(copied) if !copied.is_empty() => {
            eprintln!(
                "Copied {} Gentle Shell settings to its new config folder",
                copied.len()
            )
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!("Could not copy Gentle Shell's settings to its new folder: {error}")
        }
    }
    app::run(folder, prompt, url, requests);
}
