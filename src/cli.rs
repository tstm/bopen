//! Subcommands. Hand-rolled argv parsing: a handful of commands and flags isn't worth clap's size.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::OWN_BUNDLE_ID;
use crate::agent::{self, config_path, decision_line};
use crate::browsers::{self, KNOWN, Launch, Target};
use crate::config_edit::{ConfigDoc, Setting};
use crate::install;
use crate::macos;
use crate::rules::{Link, Modifiers, Request, Ruleset, SourceApp, SourceVia};

const HELP: &str = "\
bopen: route every link you click to the right browser, profile, or app.

usage:
  bopen install [--no-default]   build ~/Applications/Bopen.app, write an example config,
                                 and ask macOS to make bopen the default browser
  bopen set-default              ask macOS again to make bopen the default browser
  bopen uninstall                hand the default back to your `default` browser, remove the app
  bopen check [--config PATH]    validate the config, targets, profiles, and installation
  bopen test <url> [--from APP] [--mods option,shift] [--config PATH]
                                 dry run: which rule matches and what would open
  bopen open <url> [--from APP] [--mods ...] [--config PATH]
                                 route a URL for real, as if it had been clicked

  --config tries a config file before you save over the live one; the agent
  itself always reads ~/.config/bopen/config.toml.
  bopen default <target>         set `default` (e.g. chrome:Work), keeping the file's comments
  bopen pause | resume           send every link to `default` / use the rules again
  bopen menu-bar on|off          show or hide the menu bar icon
  bopen settings                 open the settings window (rules editor)
  bopen browsers                 installed browsers and their profiles
  bopen log [-f]                 recent routing decisions (or follow them)
  bopen config                   print the config file path
  bopen example                  print the example config

config: ~/.config/bopen/config.toml   log: ~/Library/Logs/bopen.log
";

pub fn run(args: Vec<String>) -> ExitCode {
    let (cmd, rest) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => ("help", &[][..]),
    };
    let result = match cmd {
        "install" => install::install(!rest.iter().any(|a| a == "--no-default")),
        "set-default" => {
            macos::set_default_browser(&install::app_path()).map(|_| println!("bopen is now the default browser"))
        }
        "uninstall" => install::uninstall(),
        "check" => with_config(rest).and_then(|(path, rest)| match rest.as_slice() {
            [] => check(&path),
            [other, ..] => Err(format!("unexpected argument {other:?}")),
        }),
        "test" => with_config(rest).and_then(|(path, rest)| route_cmd(&path, &rest, false)),
        "open" => with_config(rest).and_then(|(path, rest)| route_cmd(&path, &rest, true)),
        "default" => match rest {
            [target] => edit(|doc| doc.set_default(target)).map(|_| println!("default = {target:?}")),
            _ => Err("usage: bopen default <target>".into()),
        },
        "pause" => {
            edit(|doc| doc.set(Setting::Paused, true)).map(|_| println!("paused: every link opens in `default`"))
        }
        "resume" => edit(|doc| doc.set(Setting::Paused, false)).map(|_| println!("rules active")),
        "menu-bar" => match rest.first().map(String::as_str) {
            Some(v @ ("on" | "off")) => edit(|doc| doc.set(Setting::MenuBar, v == "on")).map(|_| {
                macos::notify_agent_reload();
                println!("menu bar icon {v}");
            }),
            _ => Err("usage: bopen menu-bar on|off".into()),
        },
        "settings" => {
            let app = install::app_path();
            match app.exists() {
                // A running agent gets a reopen event instead; `open` refuses `--env` for it.
                true => Command::new("/usr/bin/open")
                    .args(match macos::agent_running() {
                        true => vec![],
                        false => vec!["--env".to_string(), format!("{}=1", crate::ui::SHOW_SETTINGS_ENV)],
                    })
                    .arg(&app)
                    .status()
                    .map(drop)
                    .map_err(|e| format!("open: {e}")),
                false => Err(format!("{} is not installed (`bopen install`)", app.display())),
            }
        }
        "browsers" => {
            browsers_cmd();
            Ok(())
        }
        "log" => log_cmd(rest),
        "config" => {
            println!("{}", config_path().display());
            Ok(())
        }
        "example" => {
            print!("{}", crate::rules::EXAMPLE_CONFIG);
            Ok(())
        }
        "version" | "--version" | "-V" => {
            println!("bopen {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print!("{HELP}");
            Ok(())
        }
        other => Err(format!("unknown command {other:?}\n\n{HELP}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bopen: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Edit the live config in place (comments kept), refusing any result the agent would reject.
fn edit(f: impl FnOnce(&mut ConfigDoc)) -> Result<(), String> {
    let mut doc = ConfigDoc::load(&config_path())?;
    f(&mut doc);
    doc.save().map_err(|e| format!("not saved, {}: {e}", config_path().display()))
}

/// Pull `--config PATH` out of `args`; without it, the live config path.
fn with_config(args: &[String]) -> Result<(PathBuf, Vec<String>), String> {
    match args.iter().position(|a| a == "--config") {
        None => Ok((config_path(), args.to_vec())),
        Some(i) => {
            let path = args.get(i + 1).ok_or("--config needs a path")?;
            let rest = args.iter().enumerate().filter(|(j, _)| *j != i && *j != i + 1).map(|(_, a)| a.clone());
            Ok((PathBuf::from(path), rest.collect()))
        }
    }
}

/// The ruleset the agent would use for this file. A missing file is the builtin, as in the agent.
fn load_rules(path: &Path) -> Result<Ruleset, String> {
    match Ruleset::load(path) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => {
            eprintln!("note: no config at {}; using builtin (everything opens in Safari)", path.display());
            Ok(Ruleset::builtin())
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// `test` and `open` share this, so a dry run is the same computation as a real click.
fn route_cmd(config: &Path, args: &[String], really: bool) -> Result<(), String> {
    let mut url = None;
    let mut source = None;
    let mut modifiers = Modifiers::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--from" => {
                let app = it.next().ok_or("--from needs an app name or bundle id")?;
                source = Some(SourceApp { bundle_id: Some(app.clone()), name: Some(app.clone()), via: SourceVia::Cli });
            }
            "--mods" => modifiers = Modifiers::parse_list(it.next().ok_or("--mods needs e.g. option,shift")?)?,
            u if url.is_none() && !u.starts_with("--") => url = Some(u.to_string()),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let url = url.ok_or("missing <url>")?;
    let req = Request { link: Link::parse(&url), source, modifiers };
    let rules = load_rules(config)?;
    let route = rules.route(&req);
    let (launch, warning) = browsers::plan(route.target, route.url.clone());
    if let Some(w) = warning {
        println!("warning: {w}");
    }
    println!("{}", decision_line(&req, &route, &launch));
    println!("  rule:   {}", route.rule);
    println!("  target: {}", route.target);
    if route.url != url {
        println!("  url:    {}", route.url);
    }
    println!("  launch: {}", describe(&launch));
    if really {
        let started = std::time::Instant::now();
        match launch.try_handoff() {
            Some(Ok(())) => {
                println!("handed to the running browser in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
                return Ok(());
            }
            Some(Err(e)) => println!("hand-off failed ({e}); launching instead"),
            None => {}
        }
        macos::open_blocking(&launch)?;
        println!("opened in {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(())
}

fn describe(launch: &Launch) -> String {
    let path = macos::app_path(launch.app()).map_or("NOT INSTALLED".into(), |p| p.display().to_string());
    match launch {
        Launch::OpenUrl { app, .. } => format!("{} ({path})", app.as_str()),
        Launch::NewInstance { app, args, handoff } => {
            let via = handoff.as_ref().map_or(String::new(), |s| format!("; via {} when running", s.display()));
            format!("{} {args:?} ({path}{via})", app.as_str())
        }
    }
}

fn browsers_cmd() {
    let default = macos::default_browser();
    for b in KNOWN {
        let Some(path) = macos::app_path(&Target::parse(b.key).expect("known key").app()) else { continue };
        let mark = if default.as_deref() == Some(b.bundle_id) { "  (system default)" } else { "" };
        println!("{:<15} {}  {}{mark}", b.key, b.bundle_id, path.display());
        match browsers::profiles(b) {
            Ok(ps) => ps.into_iter().for_each(|p| {
                let email = p.email.map(|e| format!("  <{e}>")).unwrap_or_default();
                println!("    \"{}:{}\"  dir={:?}{email}", b.key, p.name, p.dir);
            }),
            Err(e) => println!("    (profiles unavailable: {e})"),
        }
    }
    if default.as_deref() == Some(OWN_BUNDLE_ID) {
        println!("\nbopen is the system default browser");
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Ok,
    /// Clicks still open somewhere (via the fallback chain), just not where the rule says.
    Warn,
    Fail,
}

/// Validate everything a click depends on. Failures make the command fail; warnings don't.
fn check(config: &Path) -> Result<(), String> {
    let mut failures = 0;
    let mut report = |status: Status, msg: String| {
        let label = match status {
            Status::Ok => "ok  ",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        };
        println!("{label} {msg}");
        failures += usize::from(status == Status::Fail);
    };
    let ok_or = |ok: bool, otherwise: Status| if ok { Status::Ok } else { otherwise };

    let app = install::app_path();
    report(ok_or(app.exists(), Status::Fail), format!("app bundle at {} (`bopen install`)", app.display()));
    let default = macos::default_browser();
    report(
        ok_or(default.as_deref() == Some(OWN_BUNDLE_ID), Status::Fail),
        format!("default browser is {} (`bopen set-default`)", default.as_deref().unwrap_or("unknown")),
    );

    match Ruleset::load(config) {
        Err(e) => report(Status::Fail, format!("config {}: {e}", config.display())),
        Ok(None) => report(Status::Fail, format!("no config at {} (`bopen example` prints one)", config.display())),
        Ok(Some(rules)) => {
            report(Status::Ok, format!("config {} parses: {} rules", config.display(), rules.len()));
            for (rule, target) in rules.targets() {
                if macos::app_path(&target.app()).is_none() {
                    report(Status::Warn, format!("{rule}: {target} is not installed"));
                    continue;
                }
                match browsers::plan(target, "https://example.com".into()) {
                    (_, Some(warning)) => report(Status::Warn, format!("{rule}: {target}: {warning}")),
                    (_, None) => report(Status::Ok, format!("{rule}: {target}")),
                }
            }
        }
    }

    match failures {
        0 => Ok(()),
        n => Err(format!("{n} problem(s)")),
    }
}

fn log_cmd(args: &[String]) -> Result<(), String> {
    let log = agent::Log::open(false);
    let follow = args.iter().any(|a| a == "-f" || a == "--follow");
    let mut cmd = Command::new("/usr/bin/tail");
    cmd.args(["-n", "50"]);
    if follow {
        cmd.arg("-F");
    }
    let status = cmd.arg(log.path()).status().map_err(|e| format!("tail: {e}"))?;
    status.success().then_some(()).ok_or_else(|| format!("no log yet at {}", log.path().display()))
}
