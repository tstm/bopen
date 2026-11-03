//! Writes to the config file that keep the user's comments, ordering, and formatting. Every write
//! re-validates the whole document with `Ruleset::parse` first, so the UI can't save a config the
//! agent would reject, and lands atomically (write + rename).

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, Value};

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
}

impl ConfigDoc {
    /// A missing file is an empty document: the first save creates it with only what was set.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Self::parse(path, &text)
    }

    fn parse(path: &Path, text: &str) -> Result<Self, String> {
        let doc = text.parse::<DocumentMut>().map_err(|e| format!("{}: {}", path.display(), e.message()))?;
        Ok(ConfigDoc { path: path.to_path_buf(), doc })
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
    pub fn save(&self) -> Result<(), String> {
        let text = self.text();
        Ruleset::parse(&text).map_err(|e| format!("not saved, {}: {e}", self.path.display()))?;
        let tmp = self.path.with_extension("toml.tmp");
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, &self.path))
            .map_err(|e| format!("writing {}: {e}", self.path.display()))
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
