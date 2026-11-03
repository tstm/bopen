# bopen

bopen routes every link you click on macOS to the right browser, browser profile, or app.
Rules can match on the app you clicked the link in, the URL, and the modifier keys you held.

It works like Velja or Finicky. Rules live in a small TOML file that you can edit by hand or in the settings window. A menu bar icon switches the default browser. It is a single binary of about 1 MB.

## Install

```sh
cargo install --path .
bopen install
```

`bopen install` does these steps:

1. Builds `~/Applications/bopen.app` from the binary. The app has no Dock icon and is ad-hoc signed. Xcode is not required.
2. Registers the app with LaunchServices.
3. Writes the example config to `~/.config/bopen/config.toml`, if there is no config yet. The `default` in that config is set to your current default browser.
4. Adds a login item (`~/Library/LaunchAgents/dev.bopen.plist`) and starts bopen, so the menu bar icon is there now and after every login.
5. Asks macOS to make bopen the default browser. macOS shows one confirmation dialog.

To build and register the app without changing the default browser, use `bopen install --no-default`. Run `bopen set-default` later.

After each `cargo install`, run `bopen install` again. The command stops the running copy and replaces the app.

## Menu bar

The icon (a branching arrow) shows:
- the default browser, with a submenu of profiles for Chrome, Brave, Edge, Firefox and the rest;
- **Open last link in**, which re-opens the link you just clicked (or one of the nine before it) in another browser or profile, for when a link lands in the wrong place;
- **Pause rules**, which sends every link to the default until you turn it off;
- **Settings…**, **Open config file**, **Show log** and **Quit bopen**;
- a warning at the top when the config has an error or bopen is not the default browser. Click the warning to fix it.

Choosing a browser rewrites the `default = ...` line in your config and keeps your comments. `bopen menu-bar off` hides the icon; bopen keeps routing links. If you quit bopen, the next link you click starts it again.

## Settings window

Open it from the menu bar icon (**Settings…**), by double-clicking `bopen.app` while bopen runs, or with `bopen settings`.

- **General tab.** The default browser, a **Make bopen the default browser** button, and switches for the menu bar icon, Pause rules and Start at login.
- **Rules tab.**
  - The rule list is on the left, in the order bopen checks the rules. Drag a rule to reorder it, or use **↑** and **↓**. Use **+** and **−** to add and remove rules.
  - The selected rule's conditions and target are on the right. Lists are comma-separated.
  - In **is opened from**, apps show by name. The **+** next to it adds a running app, or any app via **Choose app…**; either way it stores the app's bundle id.
  - **Test a link** at the bottom shows which rule a URL matches, optionally for a source app. It updates as you edit.

Every change is saved to the config file as you make it, and your comments stay. If an edit would make the config invalid, the window shows the error and doesn't save it. Hand edits to the file show up when you switch back to the window.

## Configure

Edit `~/.config/bopen/config.toml`. The change applies on the next click; a restart is not needed.

```toml
default = "safari"                 # used when no rule matches

[[rule]]                            # hold Option to force Safari
modifiers = ["option"]
open = "safari"

[[rule]]                            # Zoom links open in the Zoom app
host = "zoom.us"
path = "/j/*"
rewrite = "zoommtg://zoom.us/join?confno={1}&{query}"
open = "us.zoom.xos"

[[rule]]                            # links clicked in Slack open in the Chrome "Work" profile
from = ["Slack", "com.microsoft.Outlook"]
open = "chrome:Work"

[[rule]]                            # work domains open in the Work profile from any app
host = ["acme.com", "*.atlassian.net"]
open = "chrome:Work"
```

- Rules are checked from the top. The first rule that matches wins.
- All conditions in a rule must match. A list matches if any item in it matches.
- `bopen example` prints the full, commented example config.

| key | meaning |
|---|---|
| `from` | The app the link was clicked in, as an app name or a bundle id. The match is not case-sensitive. |
| `host` | `example.com` matches the domain and all subdomains. `=example.com` matches only that exact host. `*.example.com` is a glob. |
| `path` | A glob on the URL path. Each `*` is captured as `{1}`, `{2}`, and so on. |
| `url` | A glob on the whole URL. |
| `modifiers` | Keys that must be held: `option`, `shift`, `command`, `control`. |
| `open` | `safari`, `chrome`, `chrome:<profile>`, `brave:<profile>`, `edge:…`, `firefox:<profile>`, or the bundle id of any app. |
| `enabled` | `false` turns the rule off without deleting it. |
| `rewrite` | A different URL to open. Placeholders: `{url}` `{scheme}` `{host}` `{path}` `{query}` `{1}`–`{9}`. |

A Chrome profile can be named in three ways: by the name shown in Chrome (`Work`), by its directory (`Profile 1`), or by the signed-in account email. Run `bopen browsers` to list them.

Errors in the config are reported with the line number:
- `bopen check` reports them on request.
- When you save a broken config, bopen keeps using the last config that worked. It logs the error and posts a notification through `osascript`.

## Commands

```sh
bopen test https://github.com/acme/api --from Slack --mods option   # dry run: shows which rule matches
bopen test <url> --config draft.toml     # try an edit before you save it
bopen open <url> --from Slack            # route a URL for real
bopen check                              # validate the config, apps, profiles, and installation
bopen default chrome:Work                # change `default`, keeping the file's comments
bopen pause / bopen resume               # ignore the rules / use them again
bopen menu-bar on|off                    # show or hide the menu bar icon
bopen settings                           # open the settings window
bopen browsers                           # installed browsers and their profiles
bopen log -f                             # follow routing decisions (one line per click)
bopen uninstall                          # make the config's default the default browser again, remove the app and login item
```

`bopen test` and the background app use the same code path, so a dry run gives the same result as a real click.

## How it works

- macOS starts `bopen.app` on the first click. The app then keeps running in the background, so later clicks cost only one `stat` of the config file plus the routing work.
- The source app comes from the sender PID of the URL Apple Event. If that PID is unusable, bopen uses the frontmost app. Each log line says which method was used (`sender` or `frontmost`).
- Profiles are selected by starting a short-lived second instance of the browser with `--profile-directory=<dir>` (Chromium browsers) or `-P <name>` (Firefox). The second instance passes the URL to the browser that is already running.
- bopen always opens the URL somewhere. If the target fails, it tries the config's `default` with the original URL, then Safari. The `open` value can never be bopen itself, so a link can't loop back to bopen.
- The log is `~/Library/Logs/bopen.log`. Query strings and fragments are removed before logging, so tokens in links are not written to disk. The last ten full links are kept in memory only, for **Open last link in**.

## Known limitations

- **Chrome profile names and macOS privacy.** To map a name like `Work` to a directory, bopen reads `~/Library/Application Support/Google/Chrome/Local State`. Newer macOS versions can block reading another app's data. If that happens, `bopen browsers` and `bopen check` report it. You can allow access in System Settings › Privacy & Security, or use directory names such as `chrome:Profile 1`. Directory names work without reading the file.
- **Links opened from scripts.** For links opened with the `open` command or from a script, the sender process has usually exited, so the source falls back to the frontmost app.
- **Profile selection support.** Safari, Arc, and Orion have no command-line profile selection.
- **Ad-hoc signature.** The app is ad-hoc signed. After each reinstall, macOS can ask again for any privacy permission you granted.
