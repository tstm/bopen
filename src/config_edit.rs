//! Writes to the config file that keep the user's comments, ordering, and formatting. Every write
//! re-validates the whole document with `Ruleset::parse` first, so the UI can't save a config the
//! agent would reject, and lands atomically (write + rename).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::rules::Ruleset;

/// A key in the `[ui]` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    MenuBar,
    Paused,
}

impl Setting {
    fn key(self) -> &'static str {
        match self {
            Setting::MenuBar => "menu_bar",
            Setting::Paused => "paused",
        }
    }
}

pub struct ConfigDoc {
    path: PathBuf,
    doc: DocumentMut,
    /// The file as last read or written by us; a different stamp means someone else edited it.
    stamp: Option<Stamp>,
}

type Stamp = (SystemTime, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    std::fs::metadata(path).ok().and_then(|m| Some((m.modified().ok()?, m.len())))
}

/// One `[[rule]]` as the settings window edits it. Lists are the config's OR-lists; empty means
/// the condition is absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleDraft {
    pub name: String,
    pub enabled: bool,
    pub from: Vec<String>,
    pub host: Vec<String>,
    pub path: Vec<String>,
    pub url: Vec<String>,
    pub modifiers: Vec<String>,
    pub rewrite: String,
    pub open: String,
}

impl RuleDraft {
    /// What a fresh rule from the + button starts as: valid, so it can be saved at once.
    pub fn new_rule() -> Self {
        RuleDraft {
            name: "New rule".into(),
            enabled: true,
            host: vec!["example.com".into()],
            open: "safari".into(),
            ..Default::default()
        }
    }

    /// The rule list's label: the name, or a summary of the conditions.
    pub fn title(&self) -> String {
        let base = match self.name.trim() {
            "" => {
                let conds: Vec<String> =
                    [("from", &self.from), ("host", &self.host), ("path", &self.path), ("url", &self.url)]
                        .into_iter()
                        .filter(|(_, v)| !v.is_empty())
                        .map(|(k, v)| format!("{k} {}", v.join(", ")))
                        .chain((!self.modifiers.is_empty()).then(|| format!("holding {}", self.modifiers.join("+"))))
                        .collect();
                format!("{} → {}", conds.join(", "), self.open)
            }
            name => name.to_string(),
        };
        if self.enabled { base } else { format!("{base} (off)") }
    }

    fn read(t: &Table) -> Self {
        let str_of = |k: &str| t.get(k).and_then(Item::as_str).unwrap_or_default().to_string();
        let list = |k: &str| match t.get(k).and_then(Item::as_value) {
            Some(Value::String(s)) => vec![s.value().clone()],
            Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            _ => vec![],
        };
        RuleDraft {
            name: str_of("name"),
            enabled: t.get("enabled").and_then(Item::as_bool).unwrap_or(true),
            from: list("from"),
            host: list("host"),
            path: list("path"),
            url: list("url"),
            modifiers: list("modifiers"),
            rewrite: str_of("rewrite"),
            open: str_of("open"),
        }
    }

    /// Each key's wanted value, in the order a new rule is written. None = remove the key.
    fn fields(&self) -> [(&'static str, Option<Wanted<'_>>); 9] {
        fn text(s: &str) -> Option<Wanted<'_>> {
            Some(s.trim()).filter(|s| !s.is_empty()).map(Wanted::Str)
        }
        fn list(v: &[String]) -> Option<Wanted<'_>> {
            (!v.is_empty()).then_some(Wanted::List(v))
        }
        [
            ("name", text(&self.name)),
            ("enabled", (!self.enabled).then_some(Wanted::Bool(false))),
            ("from", list(&self.from)),
            ("host", list(&self.host)),
            ("path", list(&self.path)),
            ("url", list(&self.url)),
            ("modifiers", list(&self.modifiers)),
            ("rewrite", text(&self.rewrite)),
            ("open", Some(Wanted::Str(self.open.trim()))),
        ]
    }
}

#[derive(Clone, Copy)]
enum Wanted<'a> {
    Str(&'a str),
    Bool(bool),
    List(&'a [String]),
}

impl Wanted<'_> {
    fn matches(self, v: &Value) -> bool {
        match (self, v) {
            (Wanted::Str(w), Value::String(s)) => s.value() == w,
            (Wanted::Bool(w), Value::Boolean(b)) => *b.value() == w,
            (Wanted::List([one]), Value::String(s)) => s.value() == one,
            (Wanted::List(w), Value::Array(a)) => a.iter().map(Value::as_str).eq(w.iter().map(|s| Some(s.as_str()))),
            _ => false,
        }
    }

    /// One item is written as a plain string unless the file already used an array there.
    fn to_value(self, was_array: bool) -> Value {
        match self {
            Wanted::Str(s) => s.into(),
            Wanted::Bool(b) => b.into(),
            Wanted::List([one]) if !was_array => one.as_str().into(),
            Wanted::List(v) => Value::Array(v.iter().map(String::as_str).collect::<Array>()),
        }
    }
}

impl ConfigDoc {
    /// A missing file is an empty document: the first save creates it with only what was set.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut doc = Self::parse(path, &text)?;
        doc.stamp = stamp(path);
        Ok(doc)
    }

    fn parse(path: &Path, text: &str) -> Result<Self, String> {
        let doc = text.parse::<DocumentMut>().map_err(|e| format!("{}: {}", path.display(), e.message()))?;
        Ok(ConfigDoc { path: path.to_path_buf(), doc, stamp: None })
    }

    /// True when the file on disk is no longer the version this document was loaded from or saved as.
    pub fn changed_on_disk(&self) -> bool {
        stamp(&self.path) != self.stamp
    }

    pub fn rules(&self) -> Vec<RuleDraft> {
        self.doc
            .get("rule")
            .and_then(Item::as_array_of_tables)
            .map_or(vec![], |a| a.iter().map(RuleDraft::read).collect())
    }

    fn rule_tables(&mut self) -> &mut ArrayOfTables {
        let item = self.doc.entry("rule").or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()));
        if !item.is_array_of_tables() {
            *item = Item::ArrayOfTables(ArrayOfTables::new());
        }
        item.as_array_of_tables_mut().expect("just ensured")
    }

    /// Rewrite rule `index` to match `draft`, touching only keys whose value changed, so
    /// untouched keys keep their formatting and every comment stays put.
    pub fn replace_rule(&mut self, index: usize, draft: &RuleDraft) {
        let Some(table) = self.rule_tables().get_mut(index) else { return };
        for (key, wanted) in draft.fields() {
            let existing = table.get_mut(key).and_then(Item::as_value_mut);
            match (wanted, existing) {
                (None, _) => {
                    table.remove(key);
                }
                (Some(w), Some(v)) if w.matches(v) => {}
                (Some(w), Some(v)) => {
                    let new = w.to_value(v.is_array());
                    replace_keeping_decor(v, new);
                }
                (Some(w), None) => {
                    table.insert(key, Item::Value(w.to_value(false)));
                }
            }
        }
    }

    /// Insert after `after` (or append), returning the new rule's index. The new table inherits
    /// its neighbour's place in the file, so it lands right below it, before any later `[ui]`.
    pub fn insert_rule(&mut self, after: Option<usize>, draft: &RuleDraft) -> usize {
        let rules = self.rule_tables();
        let index = after.map_or(rules.len(), |i| (i + 1).min(rules.len()));
        let mut table = Table::new();
        for (key, wanted) in draft.fields() {
            if let Some(w) = wanted {
                table.insert(key, Item::Value(w.to_value(false)));
            }
        }
        rules.insert(index, table);
        index
    }

    pub fn remove_rule(&mut self, index: usize) {
        let rules = self.rule_tables();
        if index < rules.len() {
            rules.remove(index);
        }
    }

    /// Swap two rules. Their file positions swap too, so the text order follows (comments above
    /// a rule move with it).
    pub fn swap_rules(&mut self, a: usize, b: usize) {
        let rules = self.rule_tables();
        if a == b || a.max(b) >= rules.len() {
            return;
        }
        let (pa, pb) = (rules.get(a).and_then(Table::position), rules.get(b).and_then(Table::position));
        let ta = rules.replace(a, Table::new());
        let mut tb = rules.replace(b, ta);
        tb.set_position(pa);
        rules.replace(a, tb);
        if let Some(t) = rules.get_mut(b) {
            t.set_position(pb);
        }
    }

    pub fn set_default(&mut self, target: &str) {
        set_value(self.doc.as_table_mut(), "default", target.into());
    }

    pub fn set(&mut self, setting: Setting, on: bool) {
        let ui = self.doc.entry("ui").or_insert_with(|| Item::Table(Table::new()));
        // `ui = 3` isn't table-like: leave it, and `save`'s validation reports it with a line.
        let Some(ui) = ui.as_table_like_mut() else { return };
        match ui.get_mut(setting.key()).and_then(Item::as_value_mut) {
            Some(v) => replace_keeping_decor(v, on.into()),
            None => {
                ui.insert(setting.key(), Item::Value(on.into()));
            }
        }
    }

    pub fn text(&self) -> String {
        self.doc.to_string()
    }

    /// Validate, then write `config.toml.tmp` and rename it over the original.
    pub fn save(&mut self) -> Result<(), String> {
        let text = self.text();
        Ruleset::parse(&text).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("toml.tmp");
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, &self.path))
            .map_err(|e| format!("writing {}: {e}", self.path.display()))?;
        self.stamp = stamp(&self.path);
        Ok(())
    }
}

/// Insert or replace a top-level scalar without disturbing its comments.
fn set_value(table: &mut Table, key: &str, value: Value) {
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(v) => replace_keeping_decor(v, value),
        None => {
            // Root keys must stay above the first `[table]`; `insert` keeps them there.
            table.insert(key, Item::Value(value));
        }
    }
}

/// `default = "safari"   # comment` keeps `   # comment` when the value changes.
fn replace_keeping_decor(slot: &mut Value, mut value: Value) {
    *value.decor_mut() = slot.decor().clone();
    *slot = value;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{EXAMPLE_CONFIG, Settings};

    fn doc(text: &str) -> ConfigDoc {
        ConfigDoc::parse(Path::new("/x/config.toml"), text).unwrap()
    }

    #[test]
    fn edits_keep_every_comment_of_the_example() {
        let mut d = doc(EXAMPLE_CONFIG);
        d.set_default("chrome:Work");
        d.set(Setting::Paused, true);
        let out = d.text();
        let comments = |t: &str| t.lines().filter(|l| l.trim_start().starts_with('#')).count();
        assert_eq!(comments(&out), comments(EXAMPLE_CONFIG));
        assert!(out.contains("\ndefault = \"chrome:Work\"\n"), "{out}");
        let rs = Ruleset::parse(&out).unwrap();
        assert_eq!(rs.default_target().to_string(), "chrome:Work");
        assert_eq!(rs.settings(), Settings { menu_bar: true, paused: true });
    }

    #[test]
    fn trailing_comment_on_a_changed_value_survives() {
        let mut d = doc("default = \"safari\"  # fallback\n\n[ui]\npaused = true # temp\n");
        d.set_default("firefox");
        d.set(Setting::Paused, false);
        assert_eq!(d.text(), "default = \"firefox\"  # fallback\n\n[ui]\npaused = false # temp\n");
    }

    #[test]
    fn new_keys_land_where_toml_needs_them() {
        let mut d = doc("[[rule]]\nhost = \"a.com\"\nopen = \"chrome\"\n");
        d.set_default("firefox");
        d.set(Setting::MenuBar, false);
        let rs = Ruleset::parse(&d.text()).unwrap();
        assert_eq!(rs.default_target().to_string(), "firefox");
        assert_eq!((rs.len(), rs.settings().menu_bar), (1, false));
    }

    #[test]
    fn reads_and_edits_rules_without_touching_unchanged_keys() {
        let mut d = doc(EXAMPLE_CONFIG);
        let rules = d.rules();
        assert_eq!(rules.len(), Ruleset::parse(EXAMPLE_CONFIG).unwrap().len());
        let i = rules.iter().position(|r| r.name == "work apps").unwrap();
        let mut r = rules[i].clone();
        assert!(r.from.contains(&"Slack".to_string()) && r.enabled);
        r.open = "firefox".into();
        r.enabled = false;
        r.modifiers = vec!["shift".into()];
        d.replace_rule(i, &r);
        assert_eq!(d.rules()[i], r);
        let out = d.text();
        let comments = |t: &str| t.lines().filter(|l| l.trim_start().starts_with('#')).count();
        assert_eq!(comments(&out), comments(EXAMPLE_CONFIG));
        assert!(out.contains("modifiers = \"shift\""), "{out}");
        Ruleset::parse(&out).unwrap();
        let untouched = |t: &str| t.lines().filter(|l| l.starts_with("from = ")).map(String::from).collect::<Vec<_>>();
        assert_eq!(untouched(&out), untouched(EXAMPLE_CONFIG));
    }

    #[test]
    fn insert_swap_remove_keep_text_order_and_comments() {
        let text = "default = \"safari\"\n\n# first\n[[rule]]\nname = \"a\"\nhost = \"a.com\"\nopen = \"safari\"\n\n\
                    # second\n[[rule]]\nname = \"b\"\nhost = \"b.com\"\nopen = \"safari\"\n\n[ui]\npaused = false\n";
        let mut d = doc(text);
        let i = d.insert_rule(Some(0), &RuleDraft::new_rule());
        assert_eq!(i, 1);
        let names = |d: &ConfigDoc| d.rules().into_iter().map(|r| r.name).collect::<Vec<_>>();
        assert_eq!(names(&d), ["a", "New rule", "b"]);
        let out = d.text();
        assert!(out.find("New rule").unwrap() < out.find("# second").unwrap(), "{out}");
        assert!(out.find("name = \"b\"").unwrap() < out.find("[ui]").unwrap(), "{out}");
        Ruleset::parse(&out).unwrap();

        d.remove_rule(1);
        d.swap_rules(0, 1);
        assert_eq!(names(&d), ["b", "a"]);
        let out = d.text();
        assert!(out.find("# second").unwrap() < out.find("name = \"b\"").unwrap(), "{out}");
        assert!(out.find("name = \"b\"").unwrap() < out.find("# first").unwrap(), "{out}");
        assert!(out.find("name = \"a\"").unwrap() < out.find("[ui]").unwrap(), "{out}");
        let rs = Ruleset::parse(&out).unwrap();
        assert_eq!(rs.len(), 2);
    }

    #[test]
    fn titles_summarize_unnamed_rules() {
        let r =
            RuleDraft { from: vec!["Slack".into()], open: "chrome:Work".into(), enabled: true, ..Default::default() };
        assert_eq!(r.title(), "from Slack → chrome:Work");
        assert_eq!(RuleDraft { enabled: false, ..RuleDraft::new_rule() }.title(), "New rule (off)");
    }

    #[test]
    fn invalid_result_is_not_written() {
        let dir = std::env::temp_dir().join(format!("bopen-edit-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut d = ConfigDoc::load(&path).unwrap();
        d.set_default("dev.bopen");
        assert!(d.save().unwrap_err().contains("loop"));
        assert!(!path.exists());
        d.set_default("safari");
        d.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "default = \"safari\"\n");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
