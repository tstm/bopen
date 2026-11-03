//! The settings window: a Rules tab (list, editor, live tester) and a General tab. Like the menu,
//! it only edits the config file through `ConfigDoc`; each change is validated and saved at once,
//! and an invalid edit stays in the form with the error shown instead of being written.

use std::cell::{Cell, OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType, NSBorderType,
    NSButton, NSColor, NSComboBox, NSControlStateValueOff, NSControlStateValueOn, NSControlTextEditingDelegate,
    NSEventModifierFlags, NSFont, NSMenu, NSMenuItem, NSScrollView, NSTabView, NSTabViewItem, NSTableColumn,
    NSTableView, NSTableViewDataSource, NSTableViewDelegate, NSTextField, NSView, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_app_kit::{
    NSDragOperation, NSDraggingInfo, NSMenuDelegate, NSModalResponseOK, NSOpenPanel, NSPasteboardItem,
    NSPasteboardWriting, NSPopUpButton, NSTableViewDropOperation, NSTextFieldDelegate, NSTokenField,
    NSTokenFieldDelegate, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSBundle, NSIndexSet, NSInteger, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    NSURL,
};

use crate::OWN_BUNDLE_ID;
use crate::agent::{Log, config_path};
use crate::browsers::{self, AppId, Launch};
use crate::config_edit::{ConfigDoc, RuleDraft, Setting};
use crate::rules::{Link, Modifiers, Request, Ruleset, SourceApp, SourceVia};
use crate::{install, macos, ui};

/// Pasteboard type for reordering rows; private to the rule list.
const ROW_DRAG_TYPE: &str = "dev.bopen.rule-row";

const MODIFIERS: [(&str, &str); 4] =
    [("shift", "⇧ Shift"), ("control", "⌃ Control"), ("option", "⌥ Option"), ("command", "⌘ Command")];

struct Widgets {
    window: Retained<NSWindow>,
    table: Retained<NSTableView>,
    name: Retained<NSTextField>,
    enabled: Retained<NSButton>,
    /// Tokens are the config's strings; bundle ids display as the app's name.
    from: Retained<NSTokenField>,
    add_app: Retained<NSPopUpButton>,
    host: Retained<NSTextField>,
    path: Retained<NSTextField>,
    url: Retained<NSTextField>,
    mods: Vec<Retained<NSButton>>,
    open: Retained<NSComboBox>,
    rewrite: Retained<NSTextField>,
    rule_status: Retained<NSTextField>,
    remove: Retained<NSButton>,
    up: Retained<NSButton>,
    down: Retained<NSButton>,
    test_url: Retained<NSTextField>,
    test_from: Retained<NSTextField>,
    test_result: Retained<NSTextField>,
    default: Retained<NSComboBox>,
    default_status: Retained<NSTextField>,
    make_default: Retained<NSButton>,
    menu_bar: Retained<NSButton>,
    paused: Retained<NSButton>,
    login: Retained<NSButton>,
    general_status: Retained<NSTextField>,
}

impl Widgets {
    fn form_controls(&self) -> Vec<&NSView> {
        let mut v: Vec<&NSView> = vec![
            &self.name,
            &self.enabled,
            &self.from,
            &self.add_app,
            &self.host,
            &self.path,
            &self.url,
            &self.open,
            &self.rewrite,
        ];
        v.extend(self.mods.iter().map(|m| -> &NSView { m }));
        v
    }
}

pub struct Ivars {
    log: Log,
    widgets: OnceCell<Widgets>,
    /// None when the file isn't valid TOML; the status lines then say why and nothing is editable.
    doc: RefCell<Option<ConfigDoc>>,
    /// Mirrors the document's rules for the table's data source.
    rules: RefCell<Vec<RuleDraft>>,
    selected: Cell<Option<usize>>,
    /// Bundle ids behind the + menu's items, by tag.
    app_choices: RefCell<Vec<String>>,
    /// The row being dragged in the rule list.
    dragging: Cell<Option<usize>>,
    /// Set while the form is filled from the document, so the fill doesn't echo back as edits.
    filling: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BopenSettingsController"]
    #[ivars = Ivars]
    pub struct SettingsController;

    unsafe impl NSObjectProtocol for SettingsController {}

    unsafe impl NSTableViewDataSource for SettingsController {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().rules.borrow().len() as NSInteger
        }

        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn object_value(
            &self,
            _table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<AnyObject>> {
            let title = self.ivars().rules.borrow().get(row as usize).map(RuleDraft::title);
            title.map(|t| Retained::into_super(Retained::into_super(NSString::from_str(&t))))
        }

        #[unsafe(method_id(tableView:pasteboardWriterForRow:))]
        fn drag_row(&self, _table: &NSTableView, row: NSInteger) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
            self.ivars().dragging.set(Some(row as usize));
            let item = NSPasteboardItem::new();
            item.setString_forType(&NSString::from_str(&row.to_string()), &NSString::from_str(ROW_DRAG_TYPE));
            Some(ProtocolObject::from_retained(item))
        }

        /// Rows drop between rows, never onto one.
        #[unsafe(method(tableView:validateDrop:proposedRow:proposedDropOperation:))]
        fn validate_drop(
            &self,
            table: &NSTableView,
            _info: &ProtocolObject<dyn NSDraggingInfo>,
            row: NSInteger,
            operation: NSTableViewDropOperation,
        ) -> NSDragOperation {
            if self.ivars().dragging.get().is_none() {
                return NSDragOperation::None;
            }
            if operation == NSTableViewDropOperation::On {
                table.setDropRow_dropOperation(row, NSTableViewDropOperation::Above);
            }
            NSDragOperation::Move
        }

        #[unsafe(method(tableView:acceptDrop:row:dropOperation:))]
        fn accept_drop(
            &self,
            _table: &NSTableView,
            _info: &ProtocolObject<dyn NSDraggingInfo>,
            row: NSInteger,
            _operation: NSTableViewDropOperation,
        ) -> bool {
            let Some(from) = self.ivars().dragging.take() else { return false.into() };
            // Dropping above row `row` removes `from` first, so later targets shift up by one.
            let to = if row as usize > from { row as usize - 1 } else { row as usize };
            if to != from {
                self.edit(None, |doc| doc.move_rule(from, to));
                self.select(Some(to));
            }
            true
        }
    }

    unsafe impl NSControlTextEditingDelegate for SettingsController {}

    unsafe impl NSTextFieldDelegate for SettingsController {}

    unsafe impl NSTokenFieldDelegate for SettingsController {
        #[unsafe(method_id(tokenField:displayStringForRepresentedObject:))]
        fn token_display(&self, _field: &NSTokenField, object: &AnyObject) -> Option<Retained<NSString>> {
            object.downcast_ref::<NSString>().map(|s| NSString::from_str(&app_display_name(&s.to_string())))
        }
    }

    /// The + menu next to "is opened from", rebuilt each time it opens so it lists what's running now.
    unsafe impl NSMenuDelegate for SettingsController {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            self.fill_app_menu(menu);
        }
    }

    unsafe impl NSTableViewDelegate for SettingsController {
        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_changed(&self, _note: &NSNotification) {
            let row = self.w().table.selectedRow();
            self.ivars().selected.set((row >= 0).then_some(row as usize));
            self.fill_form();
        }
    }

    unsafe impl NSWindowDelegate for SettingsController {
        /// Coming back to the window after editing the file by hand shows the hand edit.
        #[unsafe(method(windowDidBecomeKey:))]
        fn did_become_key(&self, _note: &NSNotification) {
            if self.ivars().doc.borrow().as_ref().is_none_or(ConfigDoc::changed_on_disk) {
                self.reload();
            }
        }

        #[unsafe(method(windowWillClose:))]
        fn will_close(&self, _note: &NSNotification) {
            NSApplication::sharedApplication(self.mtm()).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
    }

    impl SettingsController {
        #[unsafe(method(formChanged:))]
        fn form_changed(&self, _sender: Option<&AnyObject>) {
            self.save_form();
        }

        #[unsafe(method(addRule:))]
        fn add_rule(&self, _sender: Option<&AnyObject>) {
            let mut at = None;
            self.edit(None, |doc| at = Some(doc.insert_rule(self.ivars().selected.get(), &RuleDraft::new_rule())));
            self.select(at);
            self.w().window.makeFirstResponder(Some(&self.w().name));
        }

        #[unsafe(method(removeRule:))]
        fn remove_rule(&self, _sender: Option<&AnyObject>) {
            let Some(i) = self.ivars().selected.get() else { return };
            self.edit(None, |doc| doc.remove_rule(i));
            let len = self.ivars().rules.borrow().len();
            self.select((len > 0).then(|| i.min(len - 1)));
        }

        #[unsafe(method(moveUp:))]
        fn move_up(&self, _sender: Option<&AnyObject>) {
            self.move_by(-1);
        }

        #[unsafe(method(moveDown:))]
        fn move_down(&self, _sender: Option<&AnyObject>) {
            self.move_by(1);
        }

        #[unsafe(method(appPicked:))]
        fn app_picked(&self, sender: &NSPopUpButton) {
            let tag = sender.selectedItem().map_or(-1, |i| i.tag());
            let picked = match usize::try_from(tag) {
                Ok(i) => self.ivars().app_choices.borrow().get(i).cloned(),
                Err(_) => choose_app(self.mtm()),
            };
            if let Some(id) = picked {
                let mut current = tokens(&self.w().from);
                if !current.iter().any(|t| t.eq_ignore_ascii_case(&id)) {
                    current.push(id);
                }
                let current: Vec<_> = current.iter().map(|s| NSString::from_str(s)).collect();
                unsafe { self.w().from.setObjectValue(Some(&NSArray::from_retained_slice(&current))) };
                self.save_form();
            }
        }

        #[unsafe(method(testChanged:))]
        fn test_changed(&self, _sender: Option<&AnyObject>) {
            self.run_test();
        }

        #[unsafe(method(defaultChanged:))]
        fn default_changed(&self, _sender: Option<&AnyObject>) {
            if self.ivars().filling.get() {
                return;
            }
            let target = self.w().default.stringValue().to_string();
            self.edit(None, |doc| doc.set_default(target.trim()));
        }

        #[unsafe(method(menuBarChanged:))]
        fn menu_bar_changed(&self, sender: &NSButton) {
            self.edit(None, |doc| doc.set(Setting::MenuBar, is_on(sender)));
            macos::notify_agent_reload();
        }

        #[unsafe(method(pausedChanged:))]
        fn paused_changed(&self, sender: &NSButton) {
            self.edit(None, |doc| doc.set(Setting::Paused, is_on(sender)));
        }

        #[unsafe(method(loginChanged:))]
        fn login_changed(&self, sender: &NSButton) {
            if let Err(e) = install::set_login_item(is_on(sender)) {
                self.status(&self.w().general_status, Some(&e));
            }
            self.fill_general();
        }

        #[unsafe(method(makeDefault:))]
        fn make_default(&self, _sender: Option<&AnyObject>) {
            let result = macos::set_default_browser(&install::app_path());
            self.status(&self.w().general_status, result.err().as_deref());
            self.fill_general();
        }

        #[unsafe(method(openConfig:))]
        fn open_config(&self, _sender: Option<&AnyObject>) {
            ui::open_config(&self.ivars().log);
        }
    }
);

impl SettingsController {
    pub fn new(mtm: MainThreadMarker, log: Log) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            log,
            widgets: OnceCell::new(),
            doc: RefCell::new(None),
            rules: RefCell::new(vec![]),
            selected: Cell::new(None),
            dragging: Cell::new(None),
            app_choices: RefCell::new(vec![]),
            filling: Cell::new(false),
        });
        unsafe { msg_send![super(this), init] }
    }

    /// Bring the window up (building it the first time). While it's open bopen is a regular app,
    /// so it has a menu bar with Edit shortcuts and shows in ⌘-Tab.
    pub fn show(&self) {
        let mtm = self.mtm();
        if self.ivars().widgets.get().is_none() {
            let _ = self.ivars().widgets.set(self.build());
            self.reload();
            self.select(self.ivars().rules.borrow().first().map(|_| 0));
        } else {
            self.reload();
        }
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        if app.mainMenu().is_none() {
            app.setMainMenu(Some(&main_menu(mtm)));
        }
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        self.w().window.makeKeyAndOrderFront(None);
        // Don't open with the default browser's text selected, one keystroke from replaced.
        self.w().window.makeFirstResponder(None);
    }

    fn w(&self) -> &Widgets {
        self.ivars().widgets.get().expect("window built before use")
    }

    /// Re-read the file and refresh everything shown. Keeps the selection when it still exists.
    fn reload(&self) {
        let loaded = ConfigDoc::load(&config_path());
        let error = loaded.as_ref().err().cloned();
        *self.ivars().doc.borrow_mut() = loaded.ok();
        self.after_change(error);
        if let Some(i) = self.ivars().selected.get() {
            let len = self.ivars().rules.borrow().len();
            self.select((len > 0).then(|| i.min(len - 1)));
        }
    }

    /// Apply `f` to the document and save. A save conflict (the file changed underneath) reloads
    /// instead; an invalid result keeps the edit in memory and shows why it wasn't saved.
    fn edit(&self, keep_form: Option<usize>, f: impl FnOnce(&mut ConfigDoc)) {
        let result = {
            let mut slot = self.ivars().doc.borrow_mut();
            let Some(doc) = slot.as_mut() else { return };
            if doc.changed_on_disk() {
                Err(None)
            } else {
                f(doc);
                doc.save().map_err(Some)
            }
        };
        match result {
            Err(None) => {
                self.reload();
                self.status(
                    &self.w().rule_status,
                    Some("The config file changed on disk; reloaded it. Redo the edit."),
                );
            }
            Ok(()) => self.after_change(None),
            Err(Some(e)) => self.after_change(Some(e)),
        }
        // A form edit must not refill the form it came from (that would move the cursor).
        if keep_form.is_none() {
            self.fill_form();
        }
    }

    fn after_change(&self, error: Option<String>) {
        let rules = self.ivars().doc.borrow().as_ref().map_or(vec![], ConfigDoc::rules);
        *self.ivars().rules.borrow_mut() = rules;
        let w = self.w();
        let selected = self.ivars().selected.get();
        w.table.reloadData();
        if let Some(i) = selected.filter(|&i| i < self.ivars().rules.borrow().len()) {
            w.table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(i as _), false);
        }
        self.status(&w.rule_status, error.as_deref());
        self.status(&w.general_status, error.as_deref());
        self.fill_targets();
        self.fill_general();
        self.run_test();
    }

    fn select(&self, index: Option<usize>) {
        self.ivars().selected.set(index);
        let w = self.w();
        match index {
            Some(i) => w.table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(i as _), false),
            None => unsafe { w.table.deselectAll(None) },
        }
        self.fill_form();
    }

    fn move_by(&self, delta: isize) {
        let Some(i) = self.ivars().selected.get() else { return };
        let len = self.ivars().rules.borrow().len();
        let Some(j) = i.checked_add_signed(delta).filter(|&j| j < len) else { return };
        self.edit(None, |doc| doc.swap_rules(i, j));
        self.select(Some(j));
    }

    fn fill_form(&self) {
        let w = self.w();
        let rule = self.ivars().selected.get().and_then(|i| self.ivars().rules.borrow().get(i).cloned());
        let editable = rule.is_some();
        let r = rule.unwrap_or_default();
        self.ivars().filling.set(true);
        set_text(&w.name, &r.name);
        set_on(&w.enabled, r.enabled || !editable);
        let tokens: Vec<_> = r.from.iter().map(|s| NSString::from_str(s)).collect();
        unsafe { w.from.setObjectValue(Some(&NSArray::from_retained_slice(&tokens))) };
        set_text(&w.host, &r.host.join(", "));
        set_text(&w.path, &r.path.join(", "));
        set_text(&w.url, &r.url.join(", "));
        for (button, (key, _)) in w.mods.iter().zip(MODIFIERS) {
            set_on(button, r.modifiers.iter().any(|m| m.eq_ignore_ascii_case(key)));
        }
        set_text(&w.open, &r.open);
        set_text(&w.rewrite, &r.rewrite);
        self.ivars().filling.set(false);
        for control in w.form_controls() {
            set_enabled(control, editable);
        }
        let i = self.ivars().selected.get();
        let len = self.ivars().rules.borrow().len();
        w.remove.setEnabled(editable);
        w.up.setEnabled(i.is_some_and(|i| i > 0));
        w.down.setEnabled(i.is_some_and(|i| i + 1 < len));
    }

    /// Title item, then running regular apps by name, then "Choose app…" (tag -1).
    fn fill_app_menu(&self, menu: &NSMenu) {
        let mtm = self.mtm();
        menu.removeAllItems();
        let item = |title: &str, tag: isize| {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    None,
                    &NSString::new(),
                )
            };
            item.setTag(tag);
            item
        };
        menu.addItem(&item("+", -2));
        let mut apps: Vec<(String, String)> = NSWorkspace::sharedWorkspace()
            .runningApplications()
            .iter()
            .filter(|a| a.activationPolicy() == NSApplicationActivationPolicy::Regular)
            .filter_map(|a| Some((a.localizedName()?.to_string(), a.bundleIdentifier()?.to_string())))
            .filter(|(_, id)| id != OWN_BUNDLE_ID)
            .collect();
        apps.sort_by_key(|(name, _)| name.to_lowercase());
        apps.dedup_by(|a, b| a.1 == b.1);
        for (i, (name, _)) in apps.iter().enumerate() {
            menu.addItem(&item(name, i as isize));
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&item("Choose app…", -1));
        *self.ivars().app_choices.borrow_mut() = apps.into_iter().map(|(_, id)| id).collect();
    }

    fn save_form(&self) {
        if self.ivars().filling.get() {
            return;
        }
        let Some(index) = self.ivars().selected.get() else { return };
        let draft = self.read_form();
        self.edit(Some(index), |doc| doc.replace_rule(index, &draft));
    }

    fn read_form(&self) -> RuleDraft {
        let w = self.w();
        let list = |f: &NSTextField| -> Vec<String> {
            f.stringValue().to_string().split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
        };
        RuleDraft {
            name: w.name.stringValue().to_string(),
            enabled: is_on(&w.enabled),
            from: tokens(&w.from),
            host: list(&w.host),
            path: list(&w.path),
            url: list(&w.url),
            modifiers: w.mods.iter().zip(MODIFIERS).filter(|(b, _)| is_on(b)).map(|(_, (k, _))| k.into()).collect(),
            rewrite: w.rewrite.stringValue().to_string(),
            open: w.open.stringValue().to_string(),
        }
    }

    /// The `open` and default-browser choices: installed browsers and profiles, plus every
    /// target the file already uses (apps like `us.zoom.xos`).
    fn fill_targets(&self) {
        let mut targets: Vec<String> = vec![];
        for b in ui::installed_browsers() {
            targets.push(b.known.key.into());
            if let Ok(profiles) = &b.profiles {
                targets.extend(profiles.iter().map(|p| format!("{}:{}", b.known.key, p.name)));
            }
        }
        for r in self.ivars().rules.borrow().iter() {
            if !r.open.is_empty() && !targets.contains(&r.open) {
                targets.push(r.open.clone());
            }
        }
        let w = self.w();
        for combo in [&w.open, &w.default] {
            combo.removeAllItems();
            for t in &targets {
                unsafe { combo.addItemWithObjectValue(&NSString::from_str(t)) };
            }
        }
    }

    fn fill_general(&self) {
        let w = self.w();
        let text = self.ivars().doc.borrow().as_ref().map(ConfigDoc::text);
        if let Some(Ok(rules)) = text.as_deref().map(Ruleset::parse) {
            self.ivars().filling.set(true);
            set_text(&w.default, &rules.default_target().to_string());
            self.ivars().filling.set(false);
            set_on(&w.menu_bar, rules.settings().menu_bar);
            set_on(&w.paused, rules.settings().paused);
        }
        set_on(&w.login, install::login_item_enabled());
        let is_default = macos::default_browser().as_deref() == Some(OWN_BUNDLE_ID);
        set_text(
            &w.default_status,
            if is_default {
                "Bopen is the default browser, so it sees every link you click."
            } else {
                "Bopen is not the default browser, so links bypass it."
            },
        );
        w.make_default.setEnabled(!is_default);
    }

    /// Route the tester's URL with the document as it is now (saved or not), through the same
    /// `route` + `plan` as a click and `bopen test`.
    fn run_test(&self) {
        let w = self.w();
        let url = w.test_url.stringValue().to_string();
        let from = w.test_from.stringValue().to_string();
        let text = self.ivars().doc.borrow().as_ref().map(ConfigDoc::text).unwrap_or_default();
        let result = match (url.trim(), Ruleset::parse(&text)) {
            ("", _) => "Type a URL to see which rule opens it.".to_string(),
            (_, Err(e)) => format!("Can't test until the config is valid: {e}"),
            (url, Ok(rules)) => {
                let source = Some(from.trim()).filter(|f| !f.is_empty()).map(|f| SourceApp {
                    bundle_id: Some(f.into()),
                    name: Some(f.into()),
                    via: SourceVia::Cli,
                });
                let req = Request { link: Link::parse(url), source, modifiers: Modifiers::default() };
                let route = rules.route(&req);
                let (launch, warning) = browsers::plan(route.target, route.url.clone());
                let mut out = format!("{} → {}", route.rule, route.target);
                if route.url != url {
                    out += &format!("\nopens {}", route.url);
                }
                if let Launch::NewInstance { args, .. } = &launch {
                    out += &format!("  ({})", args.join(" "));
                }
                if let Some(w) = warning {
                    out += &format!("\n⚠ {w}");
                }
                out
            }
        };
        set_text(&w.test_result, &result);
    }

    fn status(&self, field: &NSTextField, error: Option<&str>) {
        match error {
            Some(e) => {
                field.setTextColor(Some(&NSColor::systemRedColor()));
                set_text(field, &format!("Not saved: {e}"));
            }
            None => {
                field.setTextColor(Some(&NSColor::secondaryLabelColor()));
                set_text(
                    field,
                    "Rules are checked top to bottom; the first match wins. Changes save as you make them.",
                );
            }
        }
    }

    fn build(&self) -> Widgets {
        let mtm = self.mtm();
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, 780.0, 590.0),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str("Bopen Settings"));
        window.setDelegate(Some(ProtocolObject::from_ref(self)));

        let tabs = NSTabView::initWithFrame(NSTabView::alloc(mtm), rect(10.0, 10.0, 760.0, 570.0));
        let rules_view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 740.0, 520.0));
        let general_view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 740.0, 520.0));
        for (label, view) in [("General", &general_view), ("Rules", &rules_view)] {
            let item = NSTabViewItem::new();
            item.setLabel(&NSString::from_str(label));
            item.setView(Some(view));
            tabs.addTabViewItem(&item);
        }
        window.contentView().expect("window has a content view").addSubview(&tabs);

        let b = Builder { mtm, target: self };

        // Rules: list on the left.
        let table = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, 236.0, 340.0));
        let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &NSString::from_str("rule"));
        column.setWidth(230.0);
        table.addTableColumn(&column);
        table.setHeaderView(None);
        table.registerForDraggedTypes(&NSArray::from_retained_slice(&[NSString::from_str(ROW_DRAG_TYPE)]));
        table.setDraggingSourceOperationMask_forLocal(NSDragOperation::Move, true);
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(self)));
            table.setDelegate(Some(ProtocolObject::from_ref(self)));
        }
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(12.0, 134.0, 240.0, 352.0));
        scroll.setDocumentView(Some(&table));
        scroll.setHasVerticalScroller(true);
        scroll.setBorderType(NSBorderType::BezelBorder);
        add(&rules_view, &scroll);
        let add_button = b.button("+", sel!(addRule:), rect(12.0, 98.0, 40.0, 28.0));
        let remove = b.button("−", sel!(removeRule:), rect(54.0, 98.0, 40.0, 28.0));
        let up = b.button("↑", sel!(moveUp:), rect(104.0, 98.0, 40.0, 28.0));
        let down = b.button("↓", sel!(moveDown:), rect(146.0, 98.0, 40.0, 28.0));
        for button in [&add_button, &remove, &up, &down] {
            add(&rules_view, button);
        }

        // Rules: the selected rule's form on the right.
        let (lx, fx, fw) = (266.0, 376.0, 350.0);
        let y = Cell::new(462.0);
        let row = |label: &str| {
            let at = y.get();
            add(&rules_view, &b.label(label, rect(lx, at + 3.0, 104.0, 18.0), true));
            y.set(at - 30.0);
            at
        };
        let heading = |text: &str| {
            add(&rules_view, &b.label(text, rect(lx, y.get() + 4.0, 200.0, 18.0), false));
            y.set(y.get() - 24.0);
        };
        let form_change = sel!(formChanged:);
        let name = b.field("Shown in the list", form_change, rect(fx, row("Name"), fw, 24.0));
        let enabled = b.checkbox("Rule is on", form_change, rect(fx, row(""), fw, 22.0));
        heading("When the link…");
        let from_y = row("is opened from");
        let from = b.token_field("Slack, com.microsoft.Outlook", form_change, rect(fx, from_y, fw - 44.0, 24.0));
        let add_app = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            rect(fx + fw - 40.0, from_y - 1.0, 40.0, 26.0),
            true,
        );
        add_app.addItemWithTitle(&NSString::from_str("+"));
        add_app.setToolTip(Some(&NSString::from_str("Add a running app, or choose one from Applications")));
        if let Some(menu) = add_app.menu() {
            menu.setDelegate(Some(ProtocolObject::from_ref(self)));
        }
        b.wire(&add_app, sel!(appPicked:));
        add(&rules_view, &add_app);
        let host = b.field("acme.com, *.atlassian.net, =exact.host", form_change, rect(fx, row("host is"), fw, 24.0));
        let path = b.field("/j/*   (each * becomes {1}, {2}, …)", form_change, rect(fx, row("path matches"), fw, 24.0));
        let url = b.field("https://github.com/acme/*", form_change, rect(fx, row("URL matches"), fw, 24.0));
        let keys_y = row("keys held");
        let mods: Vec<_> = MODIFIERS
            .iter()
            .scan(fx, |x, (_, title)| {
                let checkbox = b.checkbox(title, form_change, rect(*x, keys_y, 0.0, 22.0));
                checkbox.sizeToFit();
                *x += checkbox.frame().size.width + 10.0;
                Some(checkbox)
            })
            .collect();
        heading("Then…");
        let open = b.combo(form_change, rect(fx, row("open in"), fw, 26.0));
        let rewrite =
            b.field("optional: zoommtg://zoom.us/join?confno={1}", form_change, rect(fx, row("rewrite to"), fw, 24.0));
        add(
            &rules_view,
            &b.small_label("Lists are comma-separated; any item may match.", rect(fx, y.get() + 10.0, fw, 16.0)),
        );
        let form: [&NSView; 8] = [&name, &enabled, &from, &host, &path, &url, &open, &rewrite];
        form.into_iter().for_each(|v| add(&rules_view, v));
        mods.iter().for_each(|m| add(&rules_view, m));
        let rule_status = b.wrapping_label(rect(lx, 96.0, 460.0, 44.0));
        add(&rules_view, &rule_status);

        // Rules: the tester along the bottom.
        add(&rules_view, &b.label("Test a link", rect(12.0, 62.0, 80.0, 18.0), false));
        let test = sel!(testChanged:);
        let test_url = b.field("https://github.com/acme/api", test, rect(96.0, 58.0, 400.0, 24.0));
        add(&rules_view, &b.label("from", rect(500.0, 62.0, 40.0, 18.0), true));
        let test_from = b.field("app name or bundle id", test, rect(544.0, 58.0, 182.0, 24.0));
        let test_result = b.wrapping_label(rect(12.0, 8.0, 714.0, 46.0));
        test_result.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(11.0, 0.0)));
        [&test_url, &test_from, &test_result].into_iter().for_each(|v| add(&rules_view, v));

        // General.
        let mut y = 460.0;
        let mut grow = |h: f64| {
            let at = y;
            y -= h;
            at
        };
        add(&general_view, &b.label("Default browser", rect(20.0, grow(0.0) + 3.0, 150.0, 18.0), true));
        let default = b.combo(sel!(defaultChanged:), rect(180.0, grow(28.0), 320.0, 26.0));
        add(
            &general_view,
            &b.small_label(
                "Used when no rule matches (same as the menu bar list).",
                rect(180.0, grow(36.0), 520.0, 16.0),
            ),
        );
        let default_status = b.label("", rect(180.0, grow(30.0), 520.0, 18.0), false);
        let make_default =
            b.button("Make Bopen the default browser", sel!(makeDefault:), rect(176.0, grow(48.0), 260.0, 28.0));
        let menu_bar =
            b.checkbox("Show the menu bar icon", sel!(menuBarChanged:), rect(180.0, grow(30.0), 400.0, 22.0));
        let paused = b.checkbox(
            "Pause rules (every link opens in the default browser)",
            sel!(pausedChanged:),
            rect(180.0, grow(30.0), 500.0, 22.0),
        );
        let login = b.checkbox("Start Bopen at login", sel!(loginChanged:), rect(180.0, grow(48.0), 400.0, 22.0));
        let open_config = b.button("Open config file", sel!(openConfig:), rect(176.0, grow(40.0), 180.0, 28.0));
        let general_status = b.wrapping_label(rect(20.0, 20.0, 700.0, 60.0));
        let general: [&NSView; 8] =
            [&default, &default_status, &make_default, &menu_bar, &paused, &login, &open_config, &general_status];
        general.into_iter().for_each(|v| add(&general_view, v));

        window.center();
        Widgets {
            window,
            table,
            name,
            enabled,
            from,
            add_app,
            host,
            path,
            url,
            mods,
            open,
            rewrite,
            rule_status,
            remove,
            up,
            down,
            test_url,
            test_from,
            test_result,
            default,
            default_status,
            make_default,
            menu_bar,
            paused,
            login,
            general_status,
        }
    }
}

/// Control factories with the controller as target.
struct Builder<'a> {
    mtm: MainThreadMarker,
    target: &'a SettingsController,
}

impl Builder<'_> {
    fn label(&self, text: &str, frame: NSRect, right: bool) -> Retained<NSTextField> {
        let l = NSTextField::labelWithString(&NSString::from_str(text), self.mtm);
        l.setFrame(frame);
        if right {
            l.setAlignment(objc2_app_kit::NSTextAlignment::Right);
        }
        l
    }

    fn small_label(&self, text: &str, frame: NSRect) -> Retained<NSTextField> {
        let l = self.label(text, frame, false);
        l.setFont(Some(&NSFont::systemFontOfSize(NSFont::smallSystemFontSize())));
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
        l
    }

    fn wrapping_label(&self, frame: NSRect) -> Retained<NSTextField> {
        let l = NSTextField::wrappingLabelWithString(&NSString::new(), self.mtm);
        l.setFrame(frame);
        l.setSelectable(true);
        l
    }

    fn field(&self, placeholder: &str, action: Sel, frame: NSRect) -> Retained<NSTextField> {
        let f = NSTextField::textFieldWithString(&NSString::new(), self.mtm);
        f.setFrame(frame);
        f.setPlaceholderString(Some(&NSString::from_str(placeholder)));
        self.wire(&f, action);
        // Act when the field loses focus too, not only on Return.
        if let Some(cell) = f.cell() {
            let _: () = unsafe { msg_send![&cell, setSendsActionOnEndEditing: true] };
        }
        f
    }

    fn token_field(&self, placeholder: &str, action: Sel, frame: NSRect) -> Retained<NSTokenField> {
        let f = NSTokenField::initWithFrame(NSTokenField::alloc(self.mtm), frame);
        f.setPlaceholderString(Some(&NSString::from_str(placeholder)));
        unsafe { f.setDelegate(Some(ProtocolObject::from_ref(self.target))) };
        self.wire(&f, action);
        // One scrolling line, like the other fields, instead of wrapping out of view.
        if let Some(cell) = f.cell() {
            cell.setWraps(false);
            cell.setScrollable(true);
            let _: () = unsafe { msg_send![&cell, setSendsActionOnEndEditing: true] };
        }
        f
    }

    fn combo(&self, action: Sel, frame: NSRect) -> Retained<NSComboBox> {
        let c = NSComboBox::initWithFrame(NSComboBox::alloc(self.mtm), frame);
        c.setCompletes(true);
        c.setNumberOfVisibleItems(14);
        self.wire(&c, action);
        if let Some(cell) = c.cell() {
            let _: () = unsafe { msg_send![&cell, setSendsActionOnEndEditing: true] };
        }
        c
    }

    fn checkbox(&self, title: &str, action: Sel, frame: NSRect) -> Retained<NSButton> {
        let c = unsafe {
            NSButton::checkboxWithTitle_target_action(
                &NSString::from_str(title),
                Some(self.target),
                Some(action),
                self.mtm,
            )
        };
        c.setFrame(frame);
        c
    }

    fn button(&self, title: &str, action: Sel, frame: NSRect) -> Retained<NSButton> {
        let b = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(self.target),
                Some(action),
                self.mtm,
            )
        };
        b.setFrame(frame);
        b
    }

    fn wire(&self, control: &objc2_app_kit::NSControl, action: Sel) {
        unsafe {
            control.setTarget(Some(self.target));
            control.setAction(Some(action));
        }
    }
}

/// Subviews are laid out from the top: if the tab view gives its pages a different height than
/// sketched, they keep their distance to the top edge.
fn add(parent: &NSView, child: &NSView) {
    child.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
    parent.addSubview(child);
}

/// The menu bar while the settings window is open: copy/paste and ⌘W need a main menu.
fn main_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let item = |title: &str, action: Option<Sel>, key: &str| unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    };
    let submenu = |title: &str, items: Vec<Retained<NSMenuItem>>| {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
        items.iter().for_each(|i| menu.addItem(i));
        let holder = item(title, None, "");
        holder.setSubmenu(Some(&menu));
        holder
    };
    let redo = item("Redo", Some(sel!(redo:)), "z");
    redo.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
    let bar = NSMenu::new(mtm);
    bar.addItem(&submenu("Bopen", vec![item("Quit Bopen", Some(sel!(terminate:)), "q")]));
    bar.addItem(&submenu(
        "Edit",
        vec![
            item("Undo", Some(sel!(undo:)), "z"),
            redo,
            NSMenuItem::separatorItem(mtm),
            item("Cut", Some(sel!(cut:)), "x"),
            item("Copy", Some(sel!(copy:)), "c"),
            item("Paste", Some(sel!(paste:)), "v"),
            item("Select All", Some(sel!(selectAll:)), "a"),
        ],
    ));
    bar.addItem(&submenu("Window", vec![item("Close", Some(sel!(performClose:)), "w")]));
    bar
}

/// "Slack" for `com.tinyspeck.slackmacgap` when that app is installed; anything else as written.
fn app_display_name(token: &str) -> String {
    let app =
        Some(token).filter(|t| t.contains('.')).and_then(|t| AppId::new(t).ok()).and_then(|id| macos::app_path(&id));
    app.and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned())).unwrap_or_else(|| token.to_string())
}

/// An Open panel on /Applications; the chosen app's bundle id.
fn choose_app(mtm: MainThreadMarker) -> Option<String> {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setDirectoryURL(Some(&NSURL::fileURLWithPath(&NSString::from_str("/Applications"))));
    panel.setPrompt(Some(&NSString::from_str("Add")));
    if panel.runModal() != NSModalResponseOK {
        return None;
    }
    let url = panel.URLs().firstObject()?;
    NSBundle::bundleWithURL(&url)?.bundleIdentifier().map(|s| s.to_string())
}

fn tokens(field: &NSTokenField) -> Vec<String> {
    let Some(value) = field.objectValue() else { return vec![] };
    let Some(array) = value.downcast_ref::<NSArray>() else { return vec![] };
    array
        .iter()
        .filter_map(|o| o.downcast_ref::<NSString>().map(|s| s.to_string().trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect()
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn set_text(field: &NSTextField, text: &str) {
    field.setStringValue(&NSString::from_str(text));
}

fn is_on(button: &NSButton) -> bool {
    button.state() == NSControlStateValueOn
}

fn set_on(button: &NSButton, on: bool) {
    button.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
}

fn set_enabled(view: &NSView, on: bool) {
    let _: () = unsafe { msg_send![view, setEnabled: on] };
}
