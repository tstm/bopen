//! bopen: macOS URL router. One binary, two modes.
//!
//! - **Agent mode**: started by LaunchServices from `~/Applications/bopen.app` with no subcommand
//!   (possibly a legacy `-psn_*` arg). Runs the NSApplication loop forever and routes every URL.
//! - **CLI mode**: anything else. See `cli.rs`.

mod agent;
mod browsers;
mod cli;
mod install;
mod macos;
mod rules;

/// Single source of truth for our identity. Info.plist (install.rs) and the self-open guard
/// (`browsers::AppId::new`) both derive from it.
pub const OWN_BUNDLE_ID: &str = "dev.bopen";

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("-psn_")).collect();
    if args.is_empty() && running_from_bundle() {
        macos::run_agent(agent::Agent::new(agent::Log::open(false)));
    }
    cli::run(args)
}

/// Typing `bopen` in a terminal prints help instead of starting a headless app.
fn running_from_bundle() -> bool {
    std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
}
