//! Assemble, sign, and register `~/Applications/bopen.app` from the running binary. No Xcode.
//! Every step is idempotent; re-running `bopen install` after `cargo install` upgrades in place.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::OWN_BUNDLE_ID;
use crate::agent::{config_path, home};
use crate::browsers::{Target, known_by_bundle_id};
use crate::macos;
use crate::rules::{EXAMPLE_CONFIG, Ruleset};

const LSREGISTER: &str =
    "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

pub fn app_path() -> PathBuf {
    home().join("Applications/bopen.app")
}

/// In order; each step is safe to repeat, and a crash between steps leaves either the old or the
/// new bundle, never a half one.
pub fn install(set_default: bool) -> Result<(), String> {
    let app = app_path();
    let tmp = app.with_extension("app.tmp");
    let exe = std::env::current_exe().map_err(|e| format!("can't find own executable: {e}"))?;

    macos::terminate_running_agent();

    let _ = std::fs::remove_dir_all(&tmp);
    let macos_dir = tmp.join("Contents/MacOS");
    std::fs::create_dir_all(&macos_dir).map_err(|e| format!("{}: {e}", macos_dir.display()))?;
    std::fs::write(tmp.join("Contents/Info.plist"), info_plist(env!("CARGO_PKG_VERSION")))
        .map_err(|e| format!("writing Info.plist: {e}"))?;
    std::fs::copy(&exe, macos_dir.join("bopen")).map_err(|e| format!("copying {}: {e}", exe.display()))?;
    run("/usr/bin/codesign", &["--force", "--sign", "-", path_str(&tmp)?])?;

    if app.exists() {
        std::fs::remove_dir_all(&app).map_err(|e| format!("removing old {}: {e}", app.display()))?;
    }
    std::fs::rename(&tmp, &app).map_err(|e| format!("moving bundle into place: {e}"))?;
    run(LSREGISTER, &["-f", path_str(&app)?])?;
    println!("installed {}", app.display());

    let config = config_path();
    if !config.exists() {
        let previous = macos::default_browser().filter(|b| b != OWN_BUNDLE_ID);
        let default = seed_default(previous.as_deref());
        let text = EXAMPLE_CONFIG.replacen("default = \"safari\"", &format!("default = \"{default}\""), 1);
        std::fs::create_dir_all(config.parent().expect("config has a parent"))
            .and_then(|_| std::fs::write(&config, text))
            .map_err(|e| format!("writing {}: {e}", config.display()))?;
        println!("wrote example config to {} (default = \"{default}\")", config.display());
    }

    if !set_default {
        println!("skipped making bopen the default browser (run `bopen set-default` when ready)");
    } else if macos::default_browser().as_deref() == Some(OWN_BUNDLE_ID) {
        println!("bopen is already the default browser");
    } else {
        println!("asking macOS to make bopen the default browser; confirm the dialog...");
        macos::set_default_browser(&app)?;
        println!("bopen is now the default browser");
    }
    Ok(())
}

/// Hand the default back to the config's `default` app (or Safari), stop the agent, delete the
/// bundle. Leaves the config and log in place.
pub fn uninstall() -> Result<(), String> {
    let app = app_path();
    if macos::default_browser().as_deref() == Some(OWN_BUNDLE_ID) {
        let target = Ruleset::load(&config_path())
            .ok()
            .flatten()
            .map(|r| r.default_target().app())
            .unwrap_or_else(crate::browsers::AppId::safari);
        let to = macos::app_path(&target).ok_or_else(|| format!("{} is not installed", target.as_str()))?;
        println!("asking macOS to make {} the default browser again; confirm the dialog...", target.as_str());
        macos::set_default_browser(&to)?;
    }
    macos::terminate_running_agent();
    if app.exists() {
        let _ = run(LSREGISTER, &["-u", path_str(&app)?]);
        std::fs::remove_dir_all(&app).map_err(|e| format!("removing {}: {e}", app.display()))?;
    }
    println!("removed {}; config and log are left in place", app.display());
    Ok(())
}

/// The browser that was default before bopen, as the config spells it.
fn seed_default(previous: Option<&str>) -> String {
    match previous {
        Some(id) => match known_by_bundle_id(id) {
            Some(b) => b.key.to_string(),
            None if Target::parse(id).is_ok() => id.to_string(),
            None => "safari".into(),
        },
        None => "safari".into(),
    }
}

/// The `CFBundleDocumentTypes` entry for HTML is what makes System Settings list us as a browser.
fn info_plist(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>{OWN_BUNDLE_ID}</string>
  <key>CFBundleName</key><string>bopen</string>
  <key>CFBundleDisplayName</key><string>bopen</string>
  <key>CFBundleExecutable</key><string>bopen</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>CFBundleVersion</key><string>{version}</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>Web site URL</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>CFBundleURLSchemes</key><array><string>http</string><string>https</string></array>
    </dict>
  </array>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>HTML document</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>LSItemContentTypes</key><array><string>public.html</string><string>public.xhtml</string></array>
    </dict>
  </array>
</dict>
</plist>
"#
    )
}

fn path_str(p: &Path) -> Result<&str, String> {
    p.to_str().ok_or_else(|| format!("non-UTF-8 path {}", p.display()))
}

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
    match out.status.success() {
        true => Ok(()),
        false => Err(format!("{cmd} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_default_from_previous_browser() {
        assert_eq!(seed_default(Some("com.google.Chrome")), "chrome");
        assert_eq!(seed_default(Some("com.example.NewBrowser")), "com.example.NewBrowser");
        assert_eq!(seed_default(None), "safari");
    }

    #[test]
    fn example_config_seed_line_exists() {
        // install rewrites this exact line; if the example changes, this must too.
        assert!(EXAMPLE_CONFIG.contains("\ndefault = \"safari\"\n"));
    }

    #[test]
    fn info_plist_is_valid_xml_shape() {
        let p = info_plist("1.2.3");
        assert!(p.contains("<string>dev.bopen</string>") && p.contains("<string>1.2.3</string>"));
        assert_eq!(p.matches("<dict>").count(), p.matches("</dict>").count());
    }
}
