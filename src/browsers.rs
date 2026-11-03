//! What to open with: known browsers, `Target`, profile discovery, and `plan`, which turns a
//! `Target` + URL into a concrete `Launch`. No AppKit. The only I/O is reading profile metadata.

use std::fmt;

use crate::OWN_BUNDLE_ID;

// ---------- identity ----------

/// A bundle id we may open URLs with. Invariant: never `OWN_BUNDLE_ID`, so no `Launch` can
/// loop back into bopen. Constructed only through `new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppId(String);

impl AppId {
    pub fn new(bundle_id: &str) -> Result<Self, String> {
        match bundle_id.eq_ignore_ascii_case(OWN_BUNDLE_ID) {
            true => Err(format!("{OWN_BUNDLE_ID} is bopen itself; opening with it would loop forever")),
            false => Ok(AppId(bundle_id.to_string())),
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Last resort for every failure path: always installed, never bopen.
    pub fn safari() -> Self {
        AppId("com.apple.Safari".into())
    }
}

// ---------- the one browser table ----------

#[derive(Debug, PartialEq, Eq)]
pub struct KnownBrowser {
    /// Config name: "chrome", "brave", ...
    pub key: &'static str,
    pub bundle_id: &'static str,
    pub family: Family,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Family {
    /// No command-line profile selection; `safari:X` is a parse error.
    NoProfiles,
    /// `--profile-directory=<dir>`; dirs listed in `~/Library/Application Support/<support_dir>/Local State`.
    Chromium { support_dir: &'static str },
    /// `-P <name>`; names listed in `~/Library/Application Support/<support_dir>/profiles.ini`.
    Firefox { support_dir: &'static str },
}

/// The single browser list. Parse errors, `bopen browsers`, and profile discovery all iterate it.
pub static KNOWN: &[KnownBrowser] = &[
    KnownBrowser { key: "safari", bundle_id: "com.apple.Safari", family: Family::NoProfiles },
    KnownBrowser { key: "safari-preview", bundle_id: "com.apple.SafariTechnologyPreview", family: Family::NoProfiles },
    KnownBrowser {
        key: "chrome",
        bundle_id: "com.google.Chrome",
        family: Family::Chromium { support_dir: "Google/Chrome" },
    },
    KnownBrowser {
        key: "chrome-beta",
        bundle_id: "com.google.Chrome.beta",
        family: Family::Chromium { support_dir: "Google/Chrome Beta" },
    },
    KnownBrowser {
        key: "chrome-canary",
        bundle_id: "com.google.Chrome.canary",
        family: Family::Chromium { support_dir: "Google/Chrome Canary" },
    },
    KnownBrowser {
        key: "chromium",
        bundle_id: "org.chromium.Chromium",
        family: Family::Chromium { support_dir: "Chromium" },
    },
    KnownBrowser {
        key: "brave",
        bundle_id: "com.brave.Browser",
        family: Family::Chromium { support_dir: "BraveSoftware/Brave-Browser" },
    },
    KnownBrowser {
        key: "edge",
        bundle_id: "com.microsoft.edgemac",
        family: Family::Chromium { support_dir: "Microsoft Edge" },
    },
    KnownBrowser {
        key: "vivaldi",
        bundle_id: "com.vivaldi.Vivaldi",
        family: Family::Chromium { support_dir: "Vivaldi" },
    },
    KnownBrowser { key: "arc", bundle_id: "company.thebrowser.Browser", family: Family::NoProfiles },
    KnownBrowser {
        key: "firefox",
        bundle_id: "org.mozilla.firefox",
        family: Family::Firefox { support_dir: "Firefox" },
    },
    KnownBrowser {
        key: "firefox-dev",
        bundle_id: "org.mozilla.firefoxdeveloperedition",
        family: Family::Firefox { support_dir: "Firefox" },
    },
    KnownBrowser { key: "zen", bundle_id: "app.zen-browser.zen", family: Family::Firefox { support_dir: "zen" } },
    KnownBrowser { key: "orion", bundle_id: "com.kagi.kagimacOS", family: Family::NoProfiles },
];

pub fn known_by_bundle_id(bundle_id: &str) -> Option<&'static KnownBrowser> {
    KNOWN.iter().find(|b| b.bundle_id.eq_ignore_ascii_case(bundle_id))
}

// ---------- targets ----------

/// What a rule (or `default`) says to open with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Browser {
        browser: &'static KnownBrowser,
        profile: Option<String>,
    },
    /// Any other app by bundle id (Zoom, Teams, Figma, or a browser not in KNOWN; no profiles).
    App(AppId),
}

impl Target {
    /// `"safari"`, `"chrome:Work"`, `"chrome:Profile 1"`, `"us.zoom.xos"`, `"com.google.Chrome:Work"`.
    /// Split on the FIRST ':' (bundle ids never contain one; profile names may).
    pub fn parse(s: &str) -> Result<Self, String> {
        let (app, profile) = match s.split_once(':') {
            Some((a, p)) => (a.trim(), Some(p.trim().to_string()).filter(|p| !p.is_empty())),
            None => (s.trim(), None),
        };
        let browser = KNOWN.iter().find(|b| b.key.eq_ignore_ascii_case(app)).or_else(|| known_by_bundle_id(app));
        match (browser, profile) {
            (Some(b), Some(p)) if b.family == Family::NoProfiles => {
                Err(format!("{} has no profile selection; remove `:{p}`", b.key))
            }
            (Some(browser), profile) => Ok(Target::Browser { browser, profile }),
            (None, None) if app.contains('.') => AppId::new(app).map(Target::App),
            (None, Some(p)) if app.contains('.') => {
                Err(format!("profiles (`:{p}`) only work with known browsers: {}", known_keys()))
            }
            (None, _) => Err(format!("unknown browser {app:?}; use one of {} or an app's bundle id", known_keys())),
        }
    }

    pub fn app(&self) -> AppId {
        match self {
            Target::Browser { browser, .. } => AppId(browser.bundle_id.to_string()),
            Target::App(id) => id.clone(),
        }
    }
}

fn known_keys() -> String {
    KNOWN.iter().map(|b| b.key).collect::<Vec<_>>().join(", ")
}

impl fmt::Display for Target {
    /// Canonical form: `chrome:Work`, `us.zoom.xos`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Browser { browser, profile: Some(p) } => write!(f, "{}:{p}", browser.key),
            Target::Browser { browser, profile: None } => f.write_str(browser.key),
            Target::App(id) => f.write_str(id.as_str()),
        }
    }
}

// ---------- launch plan ----------

/// Exactly what `macos::open` executes. Holds only `AppId`, so it can't target bopen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// Hand the URL to the app (NSWorkspace openURLs). Fast path; no new process.
    OpenUrl { app: AppId, url: String },
    /// Start a short-lived new instance with these arguments. It hands the URL to the
    /// already-running browser (in the chosen profile) and exits.
    NewInstance { app: AppId, args: Vec<String> },
}

impl Launch {
    pub fn fallback(url: &str) -> Self {
        Launch::OpenUrl { app: AppId::safari(), url: url.to_string() }
    }
    pub fn app(&self) -> &AppId {
        match self {
            Launch::OpenUrl { app, .. } | Launch::NewInstance { app, .. } => app,
        }
    }
}

impl fmt::Display for Launch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Launch::OpenUrl { app, .. } => write!(f, "{}", app.as_str()),
            // The URL is the last argument; leave it out so logs stay redacted.
            Launch::NewInstance { app, args } => write!(f, "{} {:?}", app.as_str(), &args[..args.len() - 1]),
        }
    }
}

/// Turn a routing decision into a launch. Never fails: an unknown profile degrades to the
/// browser without a profile and says why. "App not installed" is detected by `macos::open`
/// (only LaunchServices knows) and handled by the agent's fallback chain.
pub fn plan(target: &Target, url: String) -> (Launch, Option<String>) {
    let (browser, wanted) = match target {
        Target::Browser { browser, profile: Some(p) } => (*browser, p),
        _ => return (Launch::OpenUrl { app: target.app(), url }, None),
    };
    let app = target.app();
    let listed = profiles(browser);
    let found = listed.as_ref().ok().and_then(|ps| ps.iter().find(|p| p.matches(wanted)).cloned());
    match (&browser.family, found) {
        (Family::Chromium { .. }, Some(p)) => {
            (Launch::NewInstance { app, args: vec![format!("--profile-directory={}", p.dir), url] }, None)
        }
        // Chromium creates a new profile for an unknown directory, so only pass names that are
        // certainly directories, and only when there is no list to check them against.
        (Family::Chromium { .. }, None) if listed.as_ref().map_or(true, Vec::is_empty) && is_chromium_dir(wanted) => {
            (Launch::NewInstance { app, args: vec![format!("--profile-directory={wanted}"), url] }, None)
        }
        (Family::Firefox { .. }, Some(p)) => {
            (Launch::NewInstance { app, args: vec!["-P".into(), p.name, "-new-tab".into(), url] }, None)
        }
        // NoProfiles never carries a profile (Target::parse rejects it), so this is "not found".
        _ => {
            let why = match listed {
                Ok(ps) if ps.is_empty() => format!("{} has no profiles yet", browser.key),
                Ok(ps) => format!(
                    "no {} profile matches {wanted:?} (have: {})",
                    browser.key,
                    ps.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
                ),
                Err(e) => format!("{e}; name the profile by directory instead (e.g. `{}:Profile 1`)", browser.key),
            };
            (Launch::OpenUrl { app, url }, Some(format!("{why}; opening without a profile")))
        }
    }
}

/// "Default", "Profile 3", "Guest Profile": Chromium's own directory names.
fn is_chromium_dir(s: &str) -> bool {
    s == "Default" || s == "Guest Profile" || s.strip_prefix("Profile ").is_some_and(|n| n.parse::<u32>().is_ok())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileInfo {
    /// Chromium: "Profile 1". Firefox: the profile's path.
    pub dir: String,
    pub name: String,
    pub email: Option<String>,
}

impl ProfileInfo {
    /// Display name, directory, or account email, case-insensitive.
    pub fn matches(&self, wanted: &str) -> bool {
        [Some(&self.name), Some(&self.dir), self.email.as_ref()]
            .into_iter()
            .flatten()
            .any(|v| v.eq_ignore_ascii_case(wanted))
    }
}

/// Profiles on disk for a browser. Ok(empty) if the browser has none or was never run;
/// Err if the list exists but can't be read (typically macOS's app-data privacy protection).
/// Read fresh each time (~1 ms) so profiles created or renamed later just work.
pub fn profiles(browser: &KnownBrowser) -> Result<Vec<ProfileInfo>, String> {
    let (support_dir, file, parse): (_, _, fn(&str) -> Vec<ProfileInfo>) = match browser.family {
        Family::NoProfiles => return Ok(vec![]),
        Family::Chromium { support_dir } => (support_dir, "Local State", parse_local_state),
        Family::Firefox { support_dir } => (support_dir, "profiles.ini", parse_profiles_ini),
    };
    let path = crate::agent::home().join("Library/Application Support").join(support_dir).join(file);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(parse(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(1) => Err(format!(
            "macOS blocked reading {} profiles ({e}); allow it under System Settings > Privacy & Security",
            browser.key
        )),
        Err(e) => Err(format!("can't read {}: {e}", path.display())),
    }
}

/// `.profile.info_cache.<dir>.{name,user_name}` -> list, "Default" first then by dir.
fn parse_local_state(json: &str) -> Vec<ProfileInfo> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return vec![] };
    let Some(cache) = v.pointer("/profile/info_cache").and_then(|c| c.as_object()) else { return vec![] };
    let mut out: Vec<_> = cache
        .iter()
        .map(|(dir, info)| ProfileInfo {
            dir: dir.clone(),
            name: info.get("name").and_then(|n| n.as_str()).unwrap_or(dir).to_string(),
            email: info.get("user_name").and_then(|n| n.as_str()).filter(|s| !s.is_empty()).map(str::to_string),
        })
        .collect();
    out.sort_by_key(|p| (p.dir != "Default", p.dir.clone()));
    out
}

/// `[ProfileN]` sections of profiles.ini with `Name=` and `Path=`.
fn parse_profiles_ini(ini: &str) -> Vec<ProfileInfo> {
    let mut out = vec![];
    let mut cur: Option<(Option<String>, Option<String>)> = None;
    let mut flush = |cur: &mut Option<(Option<String>, Option<String>)>| {
        if let Some((Some(name), path)) = cur.take() {
            out.push(ProfileInfo { dir: path.unwrap_or_else(|| name.clone()), name, email: None });
        }
    };
    for line in ini.lines().map(str::trim) {
        if line.starts_with('[') {
            flush(&mut cur);
            cur = line.starts_with("[Profile").then_some((None, None));
        } else if let (Some((name, path)), Some((k, v))) = (cur.as_mut(), line.split_once('=')) {
            match k.trim() {
                "Name" => *name = Some(v.trim().to_string()),
                "Path" => *path = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    flush(&mut cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_parse() {
        assert_eq!(Target::parse("Chrome:Work").unwrap().to_string(), "chrome:Work");
        assert_eq!(Target::parse("com.google.Chrome:Profile 1").unwrap().to_string(), "chrome:Profile 1");
        assert_eq!(Target::parse("us.zoom.xos").unwrap(), Target::App(AppId("us.zoom.xos".into())));
        assert!(Target::parse("safari:Work").is_err());
        assert!(Target::parse("us.zoom.xos:x").is_err());
        assert!(Target::parse("netscape").unwrap_err().contains("chrome"));
        assert!(Target::parse("DEV.BOPEN").is_err());
    }

    #[test]
    fn every_known_key_is_documented_in_example() {
        // The example's comment block is the one hand-written copy of browser names users see.
        for key in ["safari", "chrome", "brave", "firefox"] {
            assert!(crate::rules::EXAMPLE_CONFIG.contains(&format!("\"{key}")), "{key}");
        }
        assert!(KNOWN.iter().all(|b| Target::parse(b.key).is_ok()));
    }

    #[test]
    fn local_state_parsing() {
        let json = r#"{"profile":{"info_cache":{
            "Profile 1":{"name":"Work","user_name":"me@acme.com"},
            "Default":{"name":"Personal","user_name":""}}}}"#;
        let p = parse_local_state(json);
        assert_eq!(p[0], ProfileInfo { dir: "Default".into(), name: "Personal".into(), email: None });
        assert_eq!(p[1].email.as_deref(), Some("me@acme.com"));
        assert!(p[1].matches("work") && p[1].matches("profile 1") && p[1].matches("ME@acme.com"));
        assert!(parse_local_state("not json").is_empty());
    }

    #[test]
    fn profiles_ini_parsing() {
        let ini = "[General]\nStartWithLastProfile=1\n\n[Profile0]\nName=default-release\nPath=Profiles/abc.default-release\n\n[Profile1]\nName=dev\nPath=Profiles/x.dev\n[Install123]\nDefault=x\n";
        let p = parse_profiles_ini(ini);
        assert_eq!(p.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["default-release", "dev"]);
    }

    #[test]
    fn chromium_dir_names() {
        assert!(is_chromium_dir("Default") && is_chromium_dir("Profile 12"));
        assert!(!is_chromium_dir("Work") && !is_chromium_dir("Profile x"));
    }

    #[test]
    fn plan_without_profile_is_plain_open() {
        let (l, w) = plan(&Target::parse("us.zoom.xos").unwrap(), "zoommtg://x".into());
        assert_eq!(l, Launch::OpenUrl { app: AppId("us.zoom.xos".into()), url: "zoommtg://x".into() });
        assert!(w.is_none());
    }
}
