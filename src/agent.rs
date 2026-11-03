//! Resident-agent policy, with no AppKit: config cache, the infallible `handle`, and the log.
//! Lives on the main thread inside the app delegate; nothing here is shared across threads
//! except `Log::line`, which is a single O_APPEND write.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::browsers::{self, Launch};
use crate::rules::{Request, Route, Ruleset};

pub struct Agent {
    config_path: PathBuf,
    /// (mtime, len) of the file version last *attempted*, good or bad. None = file absent.
    stamp: Option<(SystemTime, u64)>,
    /// Invariant: always routable. A broken file never replaces a good ruleset.
    rules: Ruleset,
    pub log: Log,
}

impl Agent {
    pub fn new(log: Log) -> Self {
        let mut agent = Agent { config_path: config_path(), stamp: None, rules: Ruleset::builtin(), log };
        agent.refresh(true);
        agent
    }

    /// One click -> the launches to try, in order, until one succeeds: the routed target, then
    /// `default` with the original URL, then Safari. Never empty, no duplicates. Infallible by
    /// construction; writes exactly one decision line.
    pub fn handle(&mut self, req: &Request) -> Vec<Launch> {
        self.refresh(false);
        let route = self.rules.route(req);
        let (launch, warning) = browsers::plan(route.target, route.url.clone());
        if let Some(w) = warning {
            self.log.line(format!("WARN {w}"));
        }
        self.log.line(decision_line(req, &route, &launch));
        let original = req.link.as_str();
        let candidates =
            [launch, browsers::plan(self.rules.default_target(), original.into()).0, Launch::fallback(original)];
        let mut chain: Vec<Launch> = Vec::with_capacity(3);
        for l in candidates {
            if !chain.contains(&l) {
                chain.push(l);
            }
        }
        chain
    }

    /// Idempotent: same stamp -> no work. Changed -> parse; Ok replaces, Err logs, notifies, keeps.
    /// File deleted -> builtin (deleting the config is an explicit reset).
    fn refresh(&mut self, first: bool) {
        let stamp = std::fs::metadata(&self.config_path).ok().and_then(|m| Some((m.modified().ok()?, m.len())));
        if stamp == self.stamp && !first {
            return;
        }
        self.stamp = stamp;
        match Ruleset::load(&self.config_path) {
            Ok(Some(rules)) => {
                self.log.line(format!("config loaded: {} rules from {}", rules.len(), self.config_path.display()));
                self.rules = rules;
            }
            Ok(None) => {
                self.log.line(format!("no config at {}; opening everything in Safari", self.config_path.display()));
                self.rules = Ruleset::builtin();
            }
            Err(e) => {
                self.log.line(format!("ERROR config {}: {e} (keeping previous rules)", self.config_path.display()));
                notify("bopen: config not loaded", &e.to_string());
            }
        }
    }
}

/// `from=<source> mods=<mods> <rule> -> <launch> url=<redacted>`. Shared by the log and `bopen test`.
pub fn decision_line(req: &Request, route: &Route<'_>, launch: &Launch) -> String {
    let from = req.source.as_ref().map_or("unknown".to_string(), |s| s.to_string());
    let rewritten = match route.url != req.link.as_str() {
        true => format!(" rewritten={}", crate::rules::Link::parse(&route.url).redacted()),
        false => String::new(),
    };
    format!("from={from} mods={} {} -> {launch} url={}{rewritten}", req.modifiers, route.rule, req.link.redacted())
}

/// `~/.config/bopen/config.toml`, the only config path. No env override: LaunchServices starts the
/// agent without the shell's env, so an env var would make the CLI and the agent disagree.
pub fn config_path() -> PathBuf {
    home().join(".config/bopen/config.toml")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// One user-visible notification. Only used when a config save is rejected, since with no UI
/// the user would otherwise never learn their edit was ignored.
fn notify(title: &str, body: &str) {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let script = format!("display notification {} with title {}", quote(body), quote(title));
    let _ = std::process::Command::new("/usr/bin/osascript").args(["-e", &script]).spawn();
}

/// `~/Library/Logs/bopen.log`, rotated to `bopen.log.1` above 1 MiB.
#[derive(Clone)]
pub struct Log {
    path: PathBuf,
    /// Also mirror to stderr (`bopen agent` in a terminal).
    echo: bool,
}

const LOG_MAX: u64 = 1 << 20;

impl Log {
    pub fn open(echo: bool) -> Self {
        Log { path: home().join("Library/Logs/bopen.log"), echo }
    }

    /// `<RFC3339 UTC> <msg>`. Errors writing the log are swallowed: logging must never cost a click.
    pub fn line(&self, msg: impl AsRef<str>) {
        let line = format!("{} {}\n", rfc3339_utc(SystemTime::now()), msg.as_ref());
        if self.echo {
            eprint!("{line}");
        }
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > LOG_MAX) {
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = f.write_all(line.as_bytes());
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// UTC civil time from the Unix epoch without chrono (Howard Hinnant's civil_from_days).
fn rfc3339_utc(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn utc_formatting() {
        assert_eq!(rfc3339_utc(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(UNIX_EPOCH + Duration::from_secs(1_793_700_000)), "2026-11-03T10:00:00Z");
        assert_eq!(rfc3339_utc(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29T00:00:00Z");
    }
}
