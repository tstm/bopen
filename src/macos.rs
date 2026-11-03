//! The only module that touches objc2 / AppKit / `unsafe`. Thin by design: it converts between
//! Cocoa objects and the domain types in `rules`/`browsers`, and holds no routing policy.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSRunningApplication, NSWorkspace,
    NSWorkspaceOpenConfiguration,
};
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSArray, NSBundle, NSDate, NSError, NSRunLoop, NSString, NSURL,
};

use crate::OWN_BUNDLE_ID;
use crate::agent::{Agent, Log};
use crate::browsers::{AppId, Launch};
use crate::rules::{Link, Modifiers, Request, SourceApp, SourceVia};

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}
/// kInternetEventClass / kAEGetURL.
const GURL: u32 = fourcc(b"GURL");
/// keyDirectObject.
const KEY_DIRECT_OBJECT: u32 = fourcc(b"----");
/// keySenderPIDAttr.
const KEY_SENDER_PID: u32 = fourcc(b"spid");

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BopenURLHandler"]
    #[ivars = RefCell<Agent>]
    struct Handler;

    impl Handler {
        #[unsafe(method(handleGetURL:withReplyEvent:))]
        fn handle_get_url(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            self.on_url(event);
        }
    }
);

impl Handler {
    fn new(mtm: MainThreadMarker, agent: Agent) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(agent));
        unsafe { msg_send![super(this), init] }
    }

    fn on_url(&self, event: &NSAppleEventDescriptor) {
        let log = self.ivars().borrow().log.clone();
        let raw: Option<Retained<NSAppleEventDescriptor>> =
            unsafe { msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT] };
        let Some(raw) = raw.and_then(|d| d.stringValue()).map(|s| s.to_string()) else {
            log.line("ERROR URL event without a URL");
            return;
        };
        let req = Request { link: Link::parse(&raw), source: source_from_event(event), modifiers: current_modifiers() };
        // A panic must not unwind into AppKit (it would abort and lose the click).
        let chain = catch_unwind(AssertUnwindSafe(|| self.ivars().borrow_mut().handle(&req))).unwrap_or_else(|_| {
            log.line("ERROR panic while routing; opening in Safari");
            vec![Launch::fallback(&raw)]
        });
        open_chain(chain, log);
    }
}

/// Run the agent forever. The GURL handler is registered before `run`, so the URL that caused
/// LaunchServices to start us is delivered, not dropped.
pub fn run_agent(agent: Agent) -> ! {
    let mtm = MainThreadMarker::new().expect("agent runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let handler = Handler::new(mtm, agent);
    let manager = NSAppleEventManager::sharedAppleEventManager();
    unsafe {
        let _: () = msg_send![
            &manager,
            setEventHandler: &*handler as &AnyObject,
            andSelector: sel!(handleGetURL:withReplyEvent:),
            forEventClass: GURL,
            andEventID: GURL
        ];
    }
    let log = handler.ivars().borrow().log.clone();
    log.line(format!("agent started (pid {})", std::process::id()));
    let _menu = crate::ui::MenuController::install(mtm, log);
    app.run();
    std::process::exit(0)
}

/// Sender PID -> running app; else the frontmost app (not us); else None.
fn source_from_event(event: &NSAppleEventDescriptor) -> Option<SourceApp> {
    let pid: Option<Retained<NSAppleEventDescriptor>> =
        unsafe { msg_send![event, attributeDescriptorForKeyword: KEY_SENDER_PID] };
    let pid = pid.map(|d| d.int32Value()).filter(|&p| p > 0 && p as u32 != std::process::id());
    let sender = pid
        .and_then(NSRunningApplication::runningApplicationWithProcessIdentifier)
        .filter(|a| a.bundleIdentifier().is_some_and(|b| b.to_string() != OWN_BUNDLE_ID));
    if let Some(app) = sender {
        return Some(source_app(&app, SourceVia::Sender));
    }
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        // loginwindow is frontmost while the screen is locked; it never clicks links.
        .filter(|a| {
            a.bundleIdentifier().is_some_and(|b| ![OWN_BUNDLE_ID, "com.apple.loginwindow"].contains(&&*b.to_string()))
        })
        .map(|a| source_app(&a, SourceVia::Frontmost))
}

fn source_app(app: &NSRunningApplication, via: SourceVia) -> SourceApp {
    SourceApp {
        bundle_id: app.bundleIdentifier().map(|s| s.to_string()),
        name: app.localizedName().map(|s| s.to_string()),
        via,
    }
}

/// Current hardware modifier state, read in the URL handler (i.e. at click time).
fn current_modifiers() -> Modifiers {
    let flags = NSEvent::modifierFlags_class();
    [
        (NSEventModifierFlags::Shift, Modifiers::SHIFT),
        (NSEventModifierFlags::Control, Modifiers::CONTROL),
        (NSEventModifierFlags::Option, Modifiers::OPTION),
        (NSEventModifierFlags::Command, Modifiers::COMMAND),
    ]
    .into_iter()
    .filter(|(f, _)| flags.contains(*f))
    .fold(Modifiers::default(), |acc, (_, m)| acc.union(m))
}

/// Try each launch in order until one succeeds (failures can be immediate or asynchronous).
fn open_chain(mut chain: Vec<Launch>, log: Log) {
    if chain.is_empty() {
        return;
    }
    let launch = chain.remove(0);
    let what = launch.to_string();
    open(&launch, move |result| {
        if let Err(e) = result {
            let next = chain.first().map_or("giving up".into(), |l| format!("trying {l}"));
            log.line(format!("ERROR opening with {what}: {e}; {next}"));
            open_chain(chain.clone(), log.clone());
        }
    });
}

/// Execute a launch. Returns once the request is handed to NSWorkspace; `done` runs exactly once,
/// possibly on another thread.
pub fn open(launch: &Launch, done: impl Fn(Result<(), String>) + Send + 'static) {
    let ws = NSWorkspace::sharedWorkspace();
    let Some(app_url) = app_url(launch.app()) else {
        return done(Err(format!("{} is not installed", launch.app().as_str())));
    };
    let url = match launch {
        Launch::OpenUrl { url, .. } => match NSURL::URLWithString(&NSString::from_str(url)) {
            Some(u) => Some(u),
            None => return done(Err(format!("not a valid URL: {url}"))),
        },
        Launch::NewInstance { .. } => None,
    };
    let cfg = NSWorkspaceOpenConfiguration::configuration();
    cfg.setActivates(true);
    let handler =
        RcBlock::new(move |_app: *mut NSRunningApplication, err: *mut NSError| match unsafe { err.as_ref() } {
            Some(err) => done(Err(err.localizedDescription().to_string())),
            None => done(Ok(())),
        });
    match (launch, url) {
        (Launch::OpenUrl { .. }, Some(url)) => {
            ws.openURLs_withApplicationAtURL_configuration_completionHandler(
                &NSArray::from_retained_slice(&[url]),
                &app_url,
                &cfg,
                Some(&handler),
            );
        }
        (Launch::OpenUrl { .. }, None) => unreachable!("URL validated above"),
        (Launch::NewInstance { args, .. }, _) => {
            cfg.setCreatesNewApplicationInstance(true);
            let args: Vec<_> = args.iter().map(|a| NSString::from_str(a)).collect();
            cfg.setArguments(&NSArray::from_retained_slice(&args));
            ws.openApplicationAtURL_configuration_completionHandler(&app_url, &cfg, Some(&handler));
        }
    }
}

/// Like `open`, but waits (spinning the run loop) until NSWorkspace reports back. For the CLI.
pub fn open_blocking(launch: &Launch) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    open(launch, move |r| {
        let _ = tx.send(r);
    });
    wait(&rx, Duration::from_secs(15)).unwrap_or_else(|| Err("timed out waiting for LaunchServices".into()))
}

/// LaunchServices lookup. None = not installed.
fn app_url(app: &AppId) -> Option<Retained<NSURL>> {
    NSWorkspace::sharedWorkspace().URLForApplicationWithBundleIdentifier(&NSString::from_str(app.as_str()))
}

pub fn app_path(app: &AppId) -> Option<PathBuf> {
    app_url(app).and_then(|u| u.path()).map(|p| PathBuf::from(p.to_string()))
}

/// Bundle id of the current default handler for https links.
pub fn default_browser() -> Option<String> {
    let probe = NSURL::URLWithString(&NSString::from_str("https://example.com"))?;
    let app = NSWorkspace::sharedWorkspace().URLForApplicationToOpenURL(&probe)?;
    NSBundle::bundleWithURL(&app)?.bundleIdentifier().map(|s| s.to_string())
}

/// Make `app_bundle` the handler for http and https. macOS asks the user to confirm; this waits
/// for the answer. Err carries the NSError description.
pub fn set_default_browser(app_bundle: &Path) -> Result<(), String> {
    let app = NSURL::fileURLWithPath(&NSString::from_str(&app_bundle.to_string_lossy()));
    for scheme in ["http", "https"] {
        let (tx, rx) = mpsc::channel();
        let handler = RcBlock::new(move |err: *mut NSError| {
            let _ = tx.send(unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string()));
        });
        NSWorkspace::sharedWorkspace().setDefaultApplicationAtURL_toOpenURLsWithScheme_completionHandler(
            &app,
            &NSString::from_str(scheme),
            Some(&handler),
        );
        match wait(&rx, Duration::from_secs(120)) {
            Some(None) => {}
            Some(Some(e)) => return Err(format!("{scheme}: {e}")),
            None => return Err(format!("{scheme}: no answer from macOS")),
        }
    }
    Ok(())
}

/// Tell a running agent the `[ui]` table changed. No-op if none runs.
pub fn notify_agent_reload() {
    let center = objc2_foundation::NSDistributedNotificationCenter::defaultCenter();
    unsafe {
        center.postNotificationName_object_userInfo_deliverImmediately(
            &NSString::from_str(&crate::ui::reload_notification()),
            None,
            None,
            true,
        );
    }
}

pub fn agent_running() -> bool {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(OWN_BUNDLE_ID)).count() > 0
}

/// Ask a running bopen agent to quit, so the next click starts the freshly installed binary.
pub fn terminate_running_agent() {
    let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(OWN_BUNDLE_ID));
    for app in apps.iter() {
        app.terminate();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !app.isTerminated() && Instant::now() < deadline {
            spin(Duration::from_millis(50));
        }
        if !app.isTerminated() {
            app.forceTerminate();
        }
    }
}

/// Wait for a completion handler while letting the main run loop deliver it, if it needs to.
fn wait<T>(rx: &mpsc::Receiver<T>, timeout: Duration) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(v) = rx.try_recv() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        spin(Duration::from_millis(20));
    }
}

fn spin(d: Duration) {
    NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(d.as_secs_f64()));
}
