# bopen UI plan

Status: phases 1 and 2 implemented 2026-11-03 (config editing, menu bar, launch at login). Phases 3 to 5 are not started. Decisions: launch at login, menu bar icon on by default, no browser-picker popup.

Deviations found while implementing phases 1 and 2:
- Launch at login is a LaunchAgent (`~/Library/LaunchAgents/dev.bopen.plist`, running `open -g` on the bundle) written by `bopen install`, not `SMAppService`. `SMAppService.mainApp` registers the *calling* bundle, so the CLI can't register the app, and it would leave the ad-hoc signing question open. The LaunchAgent needs neither.
- The CLI tells a running agent to re-read `[ui]` with a distributed notification (`dev.bopen.reload`). Reopening the app (double-click, `open`) does the same through `applicationShouldHandleReopen:`. A raw `rapp` Apple Event handler doesn't work, because NSApplication replaces it during launch.
- Hand edits to `[ui] menu_bar` apply on the next reopen, `bopen menu-bar`, or agent start. Routing settings (`paused`, `default`, rules) apply on the next click, as before.
- Every menu item and status line from the sketch shipped except "Open last link in…" (phase 5).

## Goal

Add two pieces of UI, similar to Velja's:

1. **Menu bar icon:** pick the default browser or profile quickly, pause routing, and open preferences.
2. **Preferences window:** edit rules without touching TOML. Also covers general settings and shows detected browsers and profiles.

The config file stays the only source of truth. The UI is just another editor of `~/.config/bopen/config.toml`. Hand edits and UI edits must coexist, comments included.

## Load-bearing decisions

### 1. Native AppKit from Rust (objc2), in the existing agent process

| option | verdict |
|---|---|
| **AppKit via objc2-app-kit** | **Chosen.** Stays one Rust binary, so `cargo install && bopen install` keeps working. It looks native, adds an estimated 150–300 KB, and objc2-app-kit 0.3.2 already covers NSStatusItem, NSMenu, NSWindow, NSTableView, NSGridView, NSPopUpButton and NSTokenField. The cost is verbose layout code. |
| WKWebView with embedded HTML | The forms would be faster to build, but it isn't native, and it brings a JS↔Rust message bridge plus a second UI language. Keep this as the fallback if AppKit form code turns out to be unworkable. |
| SwiftUI helper built with `swiftc` | The Command Line Tools ship `swiftc`, so this would work without Xcode. But it means two languages, two binaries and a build step that `cargo install` can't do. Rejected. |

The agent already stays running with Accessory policy (`LSUIElement`), which is exactly what a menu bar app needs. The icon and the URL handler share one process and one run loop, so no IPC is needed.

### 2. The config file is the only shared state

- The menu and the preferences window never send state to the router. They write the file. The agent's existing (mtime, len) check reloads it on the next click, and that one path already validates and keeps the last good config.
- The UI reloads its own view the same way, re-reading the file when its window becomes key or a menu opens. Then a hand edit in an editor appears in the UI with no file watcher.
- Writes go through a new pure module, `config_edit.rs`, built on **`toml_edit`**. It edits the document in place, so comments, ordering and formatting the user wrote survive. Before writing, it re-parses the new text with `Ruleset::parse`. An invalid document is never written; the UI shows the `ConfigError` instead.
- Writes are atomic: write `config.toml.tmp`, then rename it over the original. If the file changed on disk since the UI loaded it (the stamp differs), the UI reloads and asks before overwriting.

### 3. Small schema additions, all optional and backward compatible

```toml
[ui]
menu_bar = true        # show the icon (default true once the UI ships)
paused = false         # true: every link goes to `default`, rules ignored

[[rule]]
name = "Work apps"     # already exists; the UI shows it as the list title
enabled = false        # new: the UI's checkbox; disabled rules are skipped
```

Anything the UI can't represent (it can represent everything today, since all conditions are string lists) stays as-is in the file.

## Menu bar

The icon is an SF Symbol template image (for example `arrow.triangle.branch`), so it follows light and dark mode. It is not Velja's icon.

```
┌─────────────────────────────────┐
│ Default browser                 │  (disabled header)
│ ✓ Safari                        │
│   Chrome                      ▸ │  → Default · Work · Personal
│   Firefox                     ▸ │  → profiles from profiles.ini
│   Arc                           │
├─────────────────────────────────┤
│   Open last link in           ▸ │  → same browser list, last 10 links
│   Pause rules            ⌥⌘P   │  (checkmark when paused)
├─────────────────────────────────┤
│ ⚠ Config error on line 12       │  (only when the last save was rejected; click → prefs)
│ ⚠ bopen is not the default…     │  (only when true; click → set-default)
├─────────────────────────────────┤
│   Preferences…            ⌘,   │
│   Open config file              │
│   Show log                      │
│   Quit bopen                    │
└─────────────────────────────────┘
```

- **Browser list.** Built when the menu opens (`menuNeedsUpdate:`) from `browsers::KNOWN` filtered to installed apps, plus `profiles()`. That way it is always current and costs nothing while closed. Selecting an item rewrites `default = "chrome:Work"` through `config_edit`.
- **Open last link in.** A ring buffer of the last 10 routed URLs, kept in the agent's memory only. They are never written to disk, which keeps the log redaction guarantee. Choosing a browser re-opens that link there, which is the "oops, wrong profile" escape hatch.
- **Pause.** Flips `[ui] paused`.
- **Quit.** The next click relaunches the agent anyway. If `menu_bar = true`, the icon disappears until then, which is why the next section exists.

### Keeping the icon visible: launch at login

Today LaunchServices starts the agent on the first click. A menu bar icon has to be there from login. The plan is to try `SMAppService.mainApp.register()` first and fall back to a `~/Library/LaunchAgents/dev.bopen.plist` written by `bopen install`. Open question: does SMAppService accept an ad-hoc-signed app in `~/Applications`? This needs a spike. The LaunchAgent plist is known to work.

## Preferences window

A single NSWindow with an NSToolbar tab style. It is created lazily on first open and hidden on close, never destroyed. It opens from the menu, from `⌘,`, by double-clicking `bopen.app` in Finder (handle the reopen Apple Event `aevt/rapp`), and from a new `bopen prefs` CLI command, which runs `open ~/Applications/bopen.app` and triggers that same reopen event.

### General
- Default browser popup (the same list as the menu).
- "bopen is the default browser" status, with a **Make default** button (`set_default_browser`).
- Show menu bar icon, Launch at login, Pause rules.

### Rules (the main tab)

```
┌───────────────────────┬──────────────────────────────────────────────┐
│ ☑ Option → Safari     │ Name    [Work apps                        ]  │
│ ☑ Zoom links          │                                              │
│ ☑ Work apps        ◀  │ When the link…                               │
│ ☐ Work domains        │   is opened from  [Slack ×][Outlook ×] [+▾]  │
│ ☑ Personal            │   host is         [acme.com ×][*.atlassian…] │
│                       │   path matches    [                       ]  │
│                       │   URL matches     [                       ]  │
│                       │   keys held       ☐⇧ ☐⌃ ☐⌥ ☐⌘              │
│                       │                                              │
│                       │ Then              open in [Chrome — Work ▾]  │
│                       │                   rewrite [               ]  │
│ [+] [−]   drag to     │                                              │
│           reorder     │ ⚠ line 31: unknown placeholder {7}           │
├───────────────────────┴──────────────────────────────────────────────┤
│ Test: [https://github.com/acme/api   ] from [Slack ▾] [⌥]            │
│ → rule #3 "Work apps" → Chrome (profile Work)                        │
└──────────────────────────────────────────────────────────────────────┘
```

- **Left: NSTableView.** It shows the enabled checkbox and the name, or an auto-summary when there's no name. Rows reorder by drag, because first match wins and order is the whole semantics. The table has +/− buttons.
- **Right: NSGridView form**, one row per condition.
  - The fields are NSTokenFields, since each condition is already a list.
  - The **+▾** menu next to "opened from" lists running apps (`NSWorkspace.runningApplications`) plus "Choose app…" (NSOpenPanel on `/Applications`). It inserts bundle ids, but tokens display the app name.
  - The open-in popup uses the same browser and profile list as the menu, plus "Other app…".
- **Validation on every edit.** Every edit builds the would-be document and runs `Ruleset::parse` on it. Errors show inline under the form, and nothing is saved while the document is invalid. Valid edits save after a short debounce. There is no Save button, matching the Velja/macOS settings convention.
- **Tester at the bottom.** It calls the same `route` + `plan` as `bopen test`, so the UI and the CLI can't disagree. It re-runs on every edit, which is how a user sees what a rule actually does.

### Browsers
- Read-only table of `KNOWN` browsers: installed or not, plus their profiles.
- When `Local State` is unreadable, it shows the privacy message from `profiles()` and an **Open Privacy Settings** button.

### Advanced
- Open config file, Reveal log, and the `bopen check` report, rendered as a list.

## Module shape

| file | owns | AppKit |
|---|---|---|
| `config_edit.rs` (new) | `ConfigDoc` (a toml_edit document plus its disk stamp), `RuleDraft` (an editable rule), and `set_default`, `set_paused`, `rules()`, `replace_rule`, `insert`, `remove`, `move`, `save()`. Validation goes through `Ruleset::parse`; saves are atomic. | no, pure, unit-tested |
| `ui/menu.rs` (new) | NSStatusItem, menu delegate, browser list, recent links | yes |
| `ui/prefs/*.rs` (new) | window, toolbar tabs, rules list and form, tester | yes |
| `ui/widgets.rs` (new) | small builders (label, token field, popup, grid row) so layout code stays readable | yes |
| `agent.rs` | gains `paused`, the recent-links ring and `enabled` handling. Routing is unchanged. | no |
| `macos.rs` | stays the URL and event bridge; registers the reopen handler | yes |

This widens the earlier "all `unsafe` lives in `macos.rs`" rule to "all `unsafe` lives in `macos.rs` and `ui/`". The pure core (`rules`, `browsers`, `config_edit`) stays AppKit-free, and that is where the tests go.

## Phases

Each phase ships on its own and is usable.

1. **`config_edit` and the schema additions.** Covers `enabled`, `[ui]`, round-trip tests proving comments survive, and the atomic write. No UI yet; `bopen default <target>` and `bopen pause` become CLI commands as a by-product.
2. **Menu bar.** Default browser selection, pause, open config, show log, quit, and the status warnings. Plus launch at login (the SMAppService spike and the LaunchAgent fallback).
3. **Preferences shell.** The window, the tabs, and the General, Browsers and Advanced tabs. Also the reopen event and `bopen prefs`.
4. **Rules editor and tester.** The largest phase, about 800–1,200 lines of AppKit.
5. **Polish.** Open last link in…, drag reorder, and the app-picker token display.

Size budget: the binary stays under about 1.2 MB. Idle cost is zero: the menu is built only when opened, and there are no timers or watchers.

## Decisions (2026-11-03)

- **Launch at login: yes.** The agent starts at login so the icon is always there. Try `SMAppService.mainApp` first, with a `~/Library/LaunchAgents/dev.bopen.plist` fallback. Ad-hoc signing compatibility is still to be spiked.
- **Menu bar icon: on by default.** `[ui] menu_bar = false` hides it.
- **Browser-picker popup (`open = "ask"`): out of scope** for now.

## Open questions

- **Rule-to-TOML mapping:** should the UI write `host = "x"` (a scalar) when there's one item, matching hand-written style? The plan says yes: keep the existing value shape and use a scalar for one item.
