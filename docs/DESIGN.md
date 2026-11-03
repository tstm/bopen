# bopen design

## Problem

A Velja/Finicky-style URL router for macOS: it intercepts every http(s) link, matches on the source app, URL, and modifier keys, and opens the link in a browser, a Chromium/Firefox profile, or a native app. It has no UI, and it must be small and fast.

Six constraints shape the design:

- LaunchServices only delivers URLs to an `.app` that declares `CFBundleURLTypes`. There is no Xcode, so the bundle is assembled by hand and ad-hoc signed.
- URLs arrive as a `GURL` Apple Event in a running `NSApplication`. The source app is available only through the event's `keySenderPIDAttr`, which can be 0, an exited process (the `open` CLI), or an intermediary.
- Chrome selects profiles by directory (`--profile-directory=Profile 1`). The mapping from human names to directories lives in `Local State` JSON, which newer macOS versions can block from being read (app-data privacy protection).
- Opening a link with "the default browser" would open it with bopen itself and loop forever.
- Every failure must still open the link somewhere and leave a log line.
- Minimal dependencies and a single binary.

## Usage

See [README.md](../README.md) and [example-config.toml](../example-config.toml). The call sites:

```rust
// macos.rs, per click: Apple Event -> Request -> ordered fallback chain -> NSWorkspace
let req = Request { link: Link::parse(&raw), source: source_from_event(event), modifiers: current_modifiers() };
let chain = agent.handle(&req);          // infallible: [routed, default, Safari], deduped
open_chain(chain, log);                  // tries each until one succeeds

// cli.rs, `bopen test`: the same pure functions, without opening anything
let route = rules.route(&req);
let (launch, warning) = browsers::plan(route.target, route.url.clone());
```

## Shape

| file | owns | AppKit |
|---|---|---|
| `rules.rs` | `Request`, `Link`, `SourceApp`, `Modifiers`, `Ruleset` (TOML parse + validation, first-match routing, globs, rewrite templates) | no, pure |
| `browsers.rs` | the single `KNOWN` browser table, `AppId`, `Target`, profile discovery, `plan(Target, url) -> Launch` | no |
| `agent.rs` | resident state: config cache keyed by (mtime, len), last good ruleset, the fallback chain, the log | no |
| `macos.rs` | all objc2 and `unsafe` code: GURL handler, sender PID -> app, modifiers, `open`, default-browser get/set, terminating the agent | yes |
| `install.rs` | `.app` assembly, Info.plist, `codesign -s -`, `lsregister`, seeding the example config | shells out |
| `cli.rs` | subcommands | via `macos` |

The load-bearing decisions:

- **Types encode invariants.** A `Ruleset` exists only after `parse`, which rejects unknown keys, rules with no conditions, out-of-range `{n}` captures, profiles on Safari, and bopen as a target. `AppId::new` refuses bopen's bundle id, and `Launch` holds only an `AppId`, so a loop can't be expressed. Per type-system-discipline.
- **One browser list.** `KNOWN` drives target parsing, error messages, `bopen browsers`, and profile discovery. A test checks the example config mentions the main keys.
- **Pure core.** `route` and `plan` are the only decision logic, and both the agent and `bopen test` call them. The objc2 surface is one file. Per boundary-discipline.
- **Resident agent, no watcher.** One `stat` per click, a re-parse only on change, and the last good config kept when a save is broken. There are no threads, no FSEvents, and no locks. NSWorkspace completion handlers only log and start the next fallback. Per separate-before-serializing-shared-state.
- **Idempotent install.** `bopen install` stops the old agent, builds in `bopen.app.tmp`, signs it, swaps it into place, and registers it. Running it again is the upgrade path. Per make-operations-idempotent.

What it deliberately does not do: scripting, regex, URL un-shortening, a file watcher, Safari profiles, or a "system default" target.

## Synthesis decision

Architect ran an arena of four design candidates (Opus, Fable, Sonnet, Haiku) plus a Fable cross-judge. The judge and I both picked **c1 (Opus)** as the base. It had the most complete user story and the GURL handler path the spike had verified. It also had the type-level loop guard and the strongest guarantee that no link is ever dropped.

Grafts:
- **From c2 (Fable):**
  - a hand-rolled `Link` splitter instead of the `url` crate (this drops idna and about 300 KB);
  - redacting query strings in the log;
  - choosing app mode only when the binary runs from inside a `.app` (so typing `bopen` prints help);
  - the (mtime, len) reload stamp;
  - one notification when a config save is rejected.
- **From c3 (Sonnet):** folding a `status` overview into `check`, with ok/WARN/FAIL levels.
- **Rejected:**
  - c3's closed `Rewrite { Zoom, Teams }` enum. The judge preferred it, but c1's general templates were already implemented and tested, and they cover rewrites users actually write, which the enum doesn't.
  - c4's `Environment` trait. One implementation plus pure parser tests is enough for now.
  - c2's `application:openURLs:` entry point. It was unverified on a cold launch, while the GURL handler was verified.

## Tradeoffs accepted

- We accept the frontmost-app heuristic sometimes attributing a link opened by a background process to the wrong app, in exchange for `from` rules working when the sender PID is useless. The `via` field shows this in the log.
- We accept reading `Local State` on every profile launch (about 1 ms), so there is no profile cache to go stale.
- We accept that a bare `host` matches subdomains. `=host` opts out.
- We accept generic rewrite templates, which have a slightly larger parse-time validation surface, in exchange for user-defined rewrites.

## Alternatives considered

- **Named targets plus match lists (Velja's model).** This adds a second namespace that has to be kept in sync. Inline `open = "chrome:Work"` is just as short.
- **One-shot process per URL.** It costs about 100–200 ms of cold AppKit start per click, and it exposes nothing simpler to callers.
- **A scripting layer (Finicky's JS, Rhai, Lua).** It adds a runtime, and it turns a declarative file that `check` can validate into a program.
- **The `regex` crate.** It is about 1 MB, and users get escaping wrong. Globs with captures cover the rewrite use cases.

## Implementation reconciliation

These are accepted deviations from the c1 sketch. Each was surfaced by running the real artifact.

- `Agent::handle` returns a fallback **chain** (`[routed, default with original URL, Safari]`) instead of `Launch` plus a hard-coded Safari fallback. A missing target app (Zoom) should land in the user's default browser, not in Safari. `macos::open_or_fallback` became `open_chain`.
- `browsers::profiles` returns a `Result`. On this machine, reading Chrome's `Local State` fails with EPERM, even outside the sandbox. `plan` therefore passes certain Chromium directory names (`Default`, `Profile N`) straight through when the list can't be read or is empty. It never passes an arbitrary name, because Chrome would create a new profile for it.
- `--config PATH` on `test`, `open`, and `check`. This lets you try an edit before you save it, and it made it possible to verify behavior without touching the live config.
- `check` dropped the "installed binary == this build" comparison. `codesign` rewrites the bundled binary, so the comparison always failed.
- The `bopen agent` foreground subcommand was dropped. LaunchServices delivers events to the bundle, not to a terminal process, so `bopen log -f` covers the same need.
- `loginwindow` is excluded as a frontmost source. It is frontmost while the screen is locked.

## Open questions and risks

- Does the real `~/Applications/bopen.app` get a privacy prompt when it reads Chrome's `Local State`? If it does, does that prompt block the click until the user answers?
- Is `keySenderPIDAttr` populated with the clicking app for Slack, Mail, and Messages? The spike verified the mechanism only with the `open` CLI as the sender.
- Does `setDefaultApplicationAtURL` accept an ad-hoc-signed app on macOS 27? This is untested, because it changes the user's default browser.
- Does the notification posted through `osascript` actually display from an LSUIElement agent?
