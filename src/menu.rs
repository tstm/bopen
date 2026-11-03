//! What the menu bar menu shows, as data. `ui.rs` renders `Entry` into NSMenu and sends the chosen
//! `Action` back; everything decided here is testable without AppKit.

use crate::browsers::{Family, KnownBrowser, ProfileInfo, Target};
use crate::rules::Link;

/// Snapshot taken each time the menu opens.
pub struct MenuState {
    /// Installed known browsers, in `KNOWN` order, with their display names.
    pub browsers: Vec<InstalledBrowser>,
    /// The config's `default`, or the error that kept the config from loading.
    pub config: Result<ConfigView, String>,
    pub is_default_browser: bool,
    /// Recently routed links, newest first.
    pub recent: Vec<String>,
}

pub struct InstalledBrowser {
    pub known: &'static KnownBrowser,
    pub label: String,
    /// Empty for `NoProfiles` browsers.
    pub profiles: Result<Vec<ProfileInfo>, String>,
}

pub struct ConfigView {
    pub default: Target,
    pub paused: bool,
}

#[derive(Debug, PartialEq)]
pub enum Entry {
    Header(String),
    Item {
        title: String,
        action: Action,
        checked: bool,
    },
    /// Clicking the parent itself also acts (Chrome with no profile), like Velja's submenus.
    Submenu {
        title: String,
        checked: bool,
        children: Vec<Entry>,
    },
    Note(String),
    Separator,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Config spelling, e.g. `chrome:Work`.
    SetDefault(String),
    SetPaused(bool),
    /// Re-open a recent link with a target, bypassing the rules.
    OpenIn {
        url: String,
        target: String,
    },
    MakeDefaultBrowser,
    OpenSettings,
    OpenConfig,
    ShowLog,
    Quit,
}

pub fn build(state: &MenuState) -> Vec<Entry> {
    let mut menu = vec![];
    if let Err(e) = &state.config {
        menu.push(Entry::Item {
            title: format!("⚠ Config not loaded: {}", shorten(e, 60)),
            action: Action::OpenConfig,
            checked: false,
        });
        menu.push(Entry::Separator);
    }
    if !state.is_default_browser {
        menu.push(Entry::Item {
            title: "⚠ Bopen is not the default browser…".into(),
            action: Action::MakeDefaultBrowser,
            checked: false,
        });
        menu.push(Entry::Separator);
    }

    let current = state.config.as_ref().ok().map(|c| &c.default);
    menu.push(Entry::Header("Default browser".into()));
    menu.extend(state.browsers.iter().map(|b| browser_entry(b, current)));

    menu.push(Entry::Separator);
    if let Some((last, earlier)) = state.recent.split_first() {
        // The newest link's browsers come first; older links nest below it.
        let mut children = vec![Entry::Header(shorten(Link::parse(last).redacted(), 50))];
        children.extend(open_in_entries(&state.browsers, last));
        if !earlier.is_empty() {
            children.push(Entry::Separator);
            children.push(Entry::Header("Earlier links".into()));
            children.extend(earlier.iter().map(|url| Entry::Submenu {
                title: shorten(Link::parse(url).redacted(), 50),
                checked: false,
                children: open_in_entries(&state.browsers, url),
            }));
        }
        menu.push(Entry::Submenu { title: "Open last link in".into(), checked: false, children });
    }
    let paused = state.config.as_ref().is_ok_and(|c| c.paused);
    menu.push(Entry::Item { title: "Pause rules".into(), action: Action::SetPaused(!paused), checked: paused });
    menu.push(Entry::Separator);
    menu.push(Entry::Item { title: "Settings…".into(), action: Action::OpenSettings, checked: false });
    menu.push(Entry::Item { title: "Open config file".into(), action: Action::OpenConfig, checked: false });
    menu.push(Entry::Item { title: "Show log".into(), action: Action::ShowLog, checked: false });
    menu.push(Entry::Item { title: "Quit Bopen".into(), action: Action::Quit, checked: false });
    menu
}

/// Menus don't wrap; the full error is one click away in the config file and the log.
fn shorten(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", s[..cut].trim_end()),
        None => s.to_string(),
    }
}

/// Every installed browser and profile, each re-opening `url` there.
fn open_in_entries(browsers: &[InstalledBrowser], url: &str) -> Vec<Entry> {
    let item = |title: String, target: String| Entry::Item {
        title,
        action: Action::OpenIn { url: url.into(), target },
        checked: false,
    };
    let mut entries = vec![];
    for b in browsers {
        entries.push(item(b.label.clone(), b.known.key.into()));
        if let Ok(profiles) = &b.profiles {
            entries.extend(
                profiles
                    .iter()
                    .map(|p| item(format!("{} — {}", b.label, p.name), format!("{}:{}", b.known.key, p.name))),
            );
        }
    }
    entries
}

fn browser_entry(b: &InstalledBrowser, current: Option<&Target>) -> Entry {
    let this_browser = |t: &Target| matches!(t, Target::Browser { browser, .. } if browser.key == b.known.key);
    let wanted = match current {
        Some(t @ Target::Browser { profile, .. }) if this_browser(t) => Some(profile.as_deref()),
        _ => None,
    };
    let is_plain = wanted == Some(None);
    let is_profile = |p: &ProfileInfo| wanted.flatten().is_some_and(|w| p.matches(w));
    let plain =
        Entry::Item { title: b.label.clone(), action: Action::SetDefault(b.known.key.into()), checked: is_plain };
    if b.known.family == Family::NoProfiles {
        return plain;
    }
    let mut children = vec![Entry::Item {
        title: "No specific profile".into(),
        action: Action::SetDefault(b.known.key.into()),
        checked: is_plain,
    }];
    match &b.profiles {
        Ok(ps) => children.extend(ps.iter().map(|p| Entry::Item {
            title: match &p.email {
                Some(e) => format!("{}  ({e})", p.name),
                None => p.name.clone(),
            },
            action: Action::SetDefault(format!("{}:{}", b.known.key, p.name)),
            checked: is_profile(p),
        })),
        Err(e) => children.push(Entry::Note(format!("Profiles unavailable: {e}"))),
    }
    Entry::Submenu { title: b.label.clone(), checked: wanted.is_some(), children }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browsers::KNOWN;

    fn known(key: &str) -> &'static KnownBrowser {
        KNOWN.iter().find(|b| b.key == key).unwrap()
    }

    fn state(default: &str) -> MenuState {
        let profile = |name: &str, dir: &str| ProfileInfo { name: name.into(), dir: dir.into(), email: None };
        MenuState {
            browsers: vec![
                InstalledBrowser { known: known("safari"), label: "Safari".into(), profiles: Ok(vec![]) },
                InstalledBrowser {
                    known: known("chrome"),
                    label: "Google Chrome".into(),
                    profiles: Ok(vec![profile("Personal", "Default"), profile("Work", "Profile 1")]),
                },
                InstalledBrowser { known: known("brave"), label: "Brave".into(), profiles: Err("blocked".into()) },
            ],
            config: Ok(ConfigView { default: Target::parse(default).unwrap(), paused: false }),
            is_default_browser: true,
            recent: vec![],
        }
    }

    fn checked_actions(entries: &[Entry]) -> Vec<Action> {
        entries
            .iter()
            .flat_map(|e| match e {
                Entry::Item { action, checked: true, .. } => vec![action.clone()],
                Entry::Submenu { children, .. } => checked_actions(children),
                _ => vec![],
            })
            .collect()
    }

    #[test]
    fn checks_exactly_the_configured_default_by_name_or_dir() {
        assert_eq!(checked_actions(&build(&state("safari"))), vec![Action::SetDefault("safari".into())]);
        assert_eq!(checked_actions(&build(&state("chrome:work"))), vec![Action::SetDefault("chrome:Work".into())]);
        assert_eq!(checked_actions(&build(&state("chrome:Profile 1"))), vec![Action::SetDefault("chrome:Work".into())]);
    }

    #[test]
    fn last_link_offers_every_browser_and_profile_then_earlier_links() {
        let mut s = state("safari");
        assert!(!build(&s).iter().any(|e| matches!(e, Entry::Submenu { title, .. } if title == "Open last link in")));
        s.recent = vec!["https://new.com/a?token=x".into(), "https://old.com/b".into()];
        let menu = build(&s);
        let Some(Entry::Submenu { children, .. }) =
            menu.iter().find(|e| matches!(e, Entry::Submenu { title, .. } if title == "Open last link in"))
        else {
            panic!("no recent submenu")
        };
        let open_in = |url: &str, target: &str| Action::OpenIn { url: url.into(), target: target.into() };
        assert_eq!(children[0], Entry::Header("https://new.com/a".into()));
        assert!(matches!(&children[1], Entry::Item { title, action, .. }
            if title == "Safari" && *action == open_in("https://new.com/a?token=x", "safari")));
        assert!(children.iter().any(|c| matches!(c, Entry::Item { action, .. }
            if *action == open_in("https://new.com/a?token=x", "chrome:Work"))));
        let Some(Entry::Submenu { title, .. }) = children.last() else { panic!("{children:?}") };
        assert_eq!(title, "https://old.com/b");
    }

    #[test]
    fn unreadable_profiles_still_offer_the_browser() {
        let menu = build(&state("brave"));
        let brave = menu.iter().find(|e| matches!(e, Entry::Submenu { title, .. } if title == "Brave")).unwrap();
        let Entry::Submenu { checked: true, children, .. } = brave else { panic!("{brave:?}") };
        assert!(matches!(&children[1], Entry::Note(n) if n.contains("blocked")));
    }

    #[test]
    fn warnings_lead_the_menu_and_pause_toggles() {
        let mut s = state("safari");
        s.is_default_browser = false;
        s.config = Err("line 3: oops".into());
        let menu = build(&s);
        assert!(matches!(&menu[0], Entry::Item { action: Action::OpenConfig, title, .. } if title.contains("line 3")));
        assert_eq!(shorten("abcdef", 3), "abc…");
        assert_eq!(shorten("abc", 3), "abc");
        assert!(matches!(&menu[2], Entry::Item { action: Action::MakeDefaultBrowser, .. }));
        assert!(menu.contains(&Entry::Item {
            title: "Pause rules".into(),
            action: Action::SetPaused(true),
            checked: false
        }));
    }
}
