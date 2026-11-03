//! The menu bar icon. Renders `menu::build` into an NSMenu each time it opens, and applies the
//! chosen `Action` by editing the config file, the one place state lives. Never touches the
//! router: the agent picks up the edit on the next click through its (mtime, len) check.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu,
    NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSObjectProtocol, NSString};

use crate::OWN_BUNDLE_ID;
use crate::agent::{Log, config_path};
use crate::browsers::{KNOWN, Target};
use crate::config_edit::{ConfigDoc, Setting};
use crate::menu::{self, Action, ConfigView, Entry, InstalledBrowser, MenuState};
use crate::rules::Ruleset;
use crate::{browsers, install, macos};

/// Posted by the CLI after it edits `[ui]`, so the icon appears or disappears without a click.
pub const RELOAD_NOTIFICATION: &str = "dev.bopen.reload";

pub struct Ivars {
    log: Log,
    status_item: RefCell<Option<Retained<NSStatusItem>>>,
    /// `actions[item.tag]` is what a menu item does; rebuilt with the menu.
    actions: RefCell<Vec<Action>>,
    /// The `Action::MakeDefaultBrowser` dialog is modal-ish; ignore re-entry while it's up.
    busy: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BopenMenuController"]
    #[ivars = Ivars]
    pub struct MenuController;

    unsafe impl NSObjectProtocol for MenuController {}

    unsafe impl NSMenuDelegate for MenuController {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            self.render(menu);
        }
    }

    impl MenuController {
        #[unsafe(method(menuAction:))]
        fn menu_action(&self, item: &NSMenuItem) {
            let action = self.ivars().actions.borrow().get(item.tag() as usize).cloned();
            if let Some(action) = action {
                self.perform(action);
            }
        }

        #[unsafe(method(reload:))]
        fn reload(&self, _note: &NSNotification) {
            self.apply_settings();
        }

    }

    unsafe impl NSApplicationDelegate for MenuController {
        /// Double-clicking bopen.app (or `open` on it) while it runs.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _visible: bool) -> bool {
            self.apply_settings();
            false
        }
    }
);

impl MenuController {
    /// Create the controller, show the icon if `[ui] menu_bar`, and listen for reload requests.
    pub fn install(mtm: MainThreadMarker, log: Log) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            log,
            status_item: RefCell::new(None),
            actions: RefCell::new(vec![]),
            busy: Cell::new(false),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        unsafe {
            NSDistributedNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &this,
                sel!(reload:),
                Some(&NSString::from_str(RELOAD_NOTIFICATION)),
                None,
            );
        }
        NSApplication::sharedApplication(mtm).setDelegate(Some(ProtocolObject::from_ref(&*this)));
        this.apply_settings();
        this
    }

    /// Show or hide the icon to match the file. A broken file keeps the icon: it's how the user
    /// learns about the error.
    fn apply_settings(&self) {
        let show = Ruleset::load(&config_path()).map_or(true, |r| r.is_none_or(|r| r.settings().menu_bar));
        let mut slot = self.ivars().status_item.borrow_mut();
        match (show, slot.is_some()) {
            (true, false) => *slot = Some(self.status_item()),
            (false, true) => {
                if let Some(item) = slot.take() {
                    NSStatusBar::systemStatusBar().removeStatusItem(&item);
                }
            }
            _ => {}
        }
    }

    fn status_item(&self) -> Retained<NSStatusItem> {
        let mtm = self.mtm();
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(mtm) {
            let symbol = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str("arrow.triangle.branch"),
                Some(&NSString::from_str("bopen")),
            );
            match symbol {
                Some(img) => {
                    img.setTemplate(true);
                    button.setImage(Some(&img));
                }
                None => button.setTitle(&NSString::from_str("bopen")),
            }
        }
        let menu = NSMenu::new(mtm);
        menu.setDelegate(Some(ProtocolObject::from_ref(self)));
        item.setMenu(Some(&menu));
        item
    }

    fn render(&self, menu: &NSMenu) {
        menu.removeAllItems();
        let mut actions = vec![];
        for entry in menu::build(&snapshot()) {
            menu.addItem(&self.item(entry, &mut actions));
        }
        *self.ivars().actions.borrow_mut() = actions;
    }

    fn item(&self, entry: Entry, actions: &mut Vec<Action>) -> Retained<NSMenuItem> {
        let mtm = self.mtm();
        let titled = |title: &str, action: Option<objc2::runtime::Sel>| unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                action,
                &NSString::new(),
            )
        };
        let check = |item: &NSMenuItem, on: bool| {
            item.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff })
        };
        match entry {
            Entry::Separator => NSMenuItem::separatorItem(mtm),
            Entry::Header(title) | Entry::Note(title) => titled(&title, None),
            Entry::Item { title, action, checked } => {
                let item = titled(&title, Some(sel!(menuAction:)));
                unsafe { item.setTarget(Some(self)) };
                item.setTag(actions.len() as isize);
                actions.push(action);
                check(&item, checked);
                item
            }
            Entry::Submenu { title, checked, children } => {
                let item = titled(&title, None);
                let sub = NSMenu::new(mtm);
                for child in children {
                    sub.addItem(&self.item(child, actions));
                }
                item.setSubmenu(Some(&sub));
                check(&item, checked);
                item
            }
        }
    }

    fn perform(&self, action: Action) {
        let log = &self.ivars().log;
        let edit = |f: &dyn Fn(&mut ConfigDoc)| {
            let result = ConfigDoc::load(&config_path()).and_then(|mut doc| {
                f(&mut doc);
                doc.save()
            });
            if let Err(e) = result {
                log.line(format!("ERROR menu: {e}"));
                crate::agent::notify("bopen: setting not saved", &e);
            }
        };
        match action {
            Action::SetDefault(target) => {
                log.line(format!("menu: default = {target:?}"));
                edit(&|doc| doc.set_default(&target));
            }
            Action::SetPaused(on) => {
                log.line(format!("menu: paused = {on}"));
                edit(&|doc| doc.set(Setting::Paused, on));
            }
            Action::MakeDefaultBrowser if !self.ivars().busy.get() => {
                self.ivars().busy.set(true);
                if let Err(e) = macos::set_default_browser(&install::app_path()) {
                    log.line(format!("ERROR set default browser: {e}"));
                }
                self.ivars().busy.set(false);
            }
            Action::MakeDefaultBrowser => {}
            Action::OpenConfig => {
                if let Err(e) = install::ensure_config().and_then(|path| open_with(&["-t"], &path)) {
                    log.line(format!("ERROR open config: {e}"));
                }
            }
            Action::ShowLog => {
                if let Err(e) = open_with(&[], log.path()) {
                    log.line(format!("ERROR show log: {e}"));
                }
            }
            Action::Quit => NSApplication::sharedApplication(self.mtm()).terminate(None),
        }
    }
}

/// Everything the menu shows, read fresh each time it opens (a few `stat`s and two small reads).
fn snapshot() -> MenuState {
    let browsers = KNOWN
        .iter()
        .filter_map(|known| {
            let path = macos::app_path(&Target::parse(known.key).ok()?.app())?;
            let label = path.file_stem().map_or(known.key.to_string(), |s| s.to_string_lossy().into_owned());
            Some(InstalledBrowser { known, label, profiles: browsers::profiles(known) })
        })
        .collect();
    let config = match Ruleset::load(&config_path()) {
        Ok(rules) => {
            let rules = rules.unwrap_or_else(Ruleset::builtin);
            Ok(ConfigView { default: rules.default_target().clone(), paused: rules.settings().paused })
        }
        Err(e) => Err(e.to_string()),
    };
    MenuState { browsers, config, is_default_browser: macos::default_browser().as_deref() == Some(OWN_BUNDLE_ID) }
}

fn open_with(flags: &[&str], path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new("/usr/bin/open")
        .args(flags)
        .arg(path)
        .spawn()
        .map(drop)
        .map_err(|e| format!("open {}: {e}", path.display()))
}
