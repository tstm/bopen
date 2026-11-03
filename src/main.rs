//! bopen: macOS URL router. One binary, two modes.
//!
//! - **Agent mode**: started by LaunchServices from `~/Applications/Bopen.app` with no subcommand
//!   (possibly a legacy `-psn_*` arg). Runs the NSApplication loop forever and routes every URL.
//! - **CLI mode**: anything else. See `cli.rs`.

mod agent;
mod browsers;
mod cli;
mod config_edit;
mod install;
mod macos;
mod menu;
mod rules;
mod settings;
mod ui;

/// Single source of truth for our identity. Info.plist (install.rs) and the self-open guard
/// (`browsers::AppId::new`) both derive from it. `BOPEN_BUNDLE_ID=dev.bopen.test cargo build`
/// makes a test copy that can run beside a real install without either one stopping the other.
pub const OWN_BUNDLE_ID: &str = match option_env!("BOPEN_BUNDLE_ID") {
    Some(id) => id,
    None => "dev.bopen",
};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("-psn_")).collect();
    if args.is_empty() && running_from_bundle() {
        macos::run_agent(agent::Agent::new(agent::Log::open(false)));
    }
    restore_sigpipe();
    cli::run(args)
}

/// Rust ignores SIGPIPE, so `bopen browsers | head` would panic on the closed pipe. A CLI should
/// just exit quietly, as `cat` does.
fn restore_sigpipe() {
    unsafe extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    unsafe { signal(SIGPIPE, SIG_DFL) };
}

/// Typing `bopen` in a terminal prints help instead of starting a headless app.
fn running_from_bundle() -> bool {
    std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
}
