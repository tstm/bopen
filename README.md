# bopen

bopen routes every link you click on macOS to the right browser, browser profile, or app.
Rules can match on the app you clicked the link in, the URL, and the modifier keys you held.

It works like Velja or Finicky, but has no UI. It is configured with a small TOML file and is a single binary of about 700 KB.

## Install

```sh
cargo install --path .
bopen install
```

`bopen install` does these steps:

1. Builds `~/Applications/bopen.app` from the binary. The app has no Dock icon and is ad-hoc signed. Xcode is not required.
2. Registers the app with LaunchServices.
3. Writes the example config to `~/.config/bopen/config.toml`, if there is no config yet. The `default` in that config is set to your current default browser.
4. Asks macOS to make bopen the default browser. macOS shows one confirmation dialog.

To build and register the app without changing the default browser, use `bopen install --no-default`. Run `bopen set-default` later.

After each `cargo install`, run `bopen install` again. The command stops the running copy and replaces the app.

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
bopen browsers                           # installed browsers and their profiles
bopen log -f                             # follow routing decisions (one line per click)
bopen uninstall                          # make the config's default the default browser again, and remove the app
```

`bopen test` and the background app use the same code path, so a dry run gives the same result as a real click.

## How it works

- macOS starts `bopen.app` on the first click. The app then keeps running in the background, so later clicks cost only one `stat` of the config file plus the routing work.
- The source app comes from the sender PID of the URL Apple Event. If that PID is unusable, bopen uses the frontmost app. Each log line says which method was used (`sender` or `frontmost`).
- Profiles are selected by starting a short-lived second instance of the browser with `--profile-directory=<dir>` (Chromium browsers) or `-P <name>` (Firefox). The second instance passes the URL to the browser that is already running.
- bopen always opens the URL somewhere. If the target fails, it tries the config's `default` with the original URL, then Safari. The `open` value can never be bopen itself, so a link can't loop back to bopen.
- The log is `~/Library/Logs/bopen.log`. Query strings and fragments are removed before logging, so tokens in links are not written to disk.

## Known limitations

- **Chrome profile names and macOS privacy.** To map a name like `Work` to a directory, bopen reads `~/Library/Application Support/Google/Chrome/Local State`. Newer macOS versions can block reading another app's data. If that happens, `bopen browsers` and `bopen check` report it. You can allow access in System Settings › Privacy & Security, or use directory names such as `chrome:Profile 1`. Directory names work without reading the file.
- **Links opened from scripts.** For links opened with the `open` command or from a script, the sender process has usually exited, so the source falls back to the frontmost app.
- **Profile selection support.** Safari, Arc, and Orion have no command-line profile selection.
- **Ad-hoc signature.** The app is ad-hoc signed. After each reinstall, macOS can ask again for any privacy permission you granted.
