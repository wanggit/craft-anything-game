//! Shared helpers for the browser (CDP) tools: resolving a browser (attach to a loopback endpoint
//! or launch a located Chromium), strict data minimisation (never keep cookies, auth headers or
//! storage values; declared secrets redacted in every encoding), JSON body *outlines*, a DOM
//! summary, PNG pixel diffing and base64.

use crate::tool::Ctx;
use craft_anything_engines::cdp::{self, CdpClient, Deadline, Endpoint, WsConn};
use craft_anything_evidence::{Environment, ErrorCode, ToolError, host_triple, sha256_hex};
use craft_anything_platform::process::{Process, RunningProcess, StdinMode};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// Biggest screenshot we will decode or return inline.
pub const MAX_SCREENSHOT_BYTES: usize = 64 << 20;
/// Working-set admission for a screenshot decode (width·height·4 must be under this).
pub const MAX_PIXELS_WORKING_SET: u64 = 256 << 20;

/// The evidence environment for a CDP-backed observation, in the run's mode: Clean room's
/// visitor view of a website is a clean-room result (#157).
pub fn cdp_env(clean_room: bool, engine_version: Option<String>) -> Environment {
    Environment { clean_room, engine: "cdp".into(), engine_version, host: host_triple(), language_id: None }
}

// ---------------------------------------------------------------------------------------------
// Browser resolution: attach or launch

/// How a browser-running tool may obtain a browser.
pub struct LaunchOptions {
    /// Launch a located Chromium when no endpoint is given.
    pub allow_launch: bool,
    pub headless: bool,
    pub no_sandbox: bool,
    /// Seconds to wait for a launched browser to start listening.
    pub startup_secs: u64,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        LaunchOptions { allow_launch: true, headless: true, no_sandbox: false, startup_secs: 30 }
    }
}

/// A connected browser: its loopback endpoint and product info. If we launched it, the owned
/// process is held here and killed when this is dropped.
pub struct Browser {
    pub endpoint: Endpoint,
    pub product: String,
    pub version: String,
    /// The `User-Agent` from `/json/version` (empty when unknown); names the runtime.
    pub user_agent: String,
    pub launched: bool,
    // The process is dropped before the profile so its supervisor is asked to stop it first.
    #[allow(dead_code)]
    _proc: Option<RunningProcess>,
    #[allow(dead_code)]
    _profile: Option<BrowserProfile>,
}

struct BrowserProfile(std::path::PathBuf);

impl Drop for BrowserProfile {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("couldn't remove the private browser profile {}: {e}", self.0.display());
        }
    }
}

/// What is answering on a CDP endpoint: `electron`, `edge`, `chrome`, or `unknown`, from the
/// `/json/version` product and user agent (Electron and Edge both name themselves there).
pub fn runtime_of(product: &str, user_agent: &str) -> &'static str {
    let has = |s: &str| user_agent.contains(s) || product.contains(s);
    if has("Electron") {
        "electron"
    } else if has("Edg/") {
        "edge"
    } else if has("Chrome/") {
        "chrome"
    } else {
        "unknown"
    }
}

/// The error returned when no browser is available and none was asked to launch.
pub fn engine_missing(detail: impl std::fmt::Display) -> ToolError {
    ToolError::new(ErrorCode::EngineMissing, format!("no browser is available: {detail}")).with_remediation(
        "Give a loopback CDP endpoint (start a browser with --remote-debugging-port), or install a browser from Settings → Engines so one can be launched.",
    )
}

/// Where a launched browser's private profile goes. A Flatpak or Snap browser has a /tmp of its
/// own, so it couldn't use a profile there and we couldn't read its port from it: its profile goes
/// in the home folder its sandbox always shares (#747). Anything else uses the temp folder.
fn profile_base(exe: &std::path::Path, home: Option<std::path::PathBuf>) -> std::path::PathBuf {
    let parent = exe.parent().unwrap_or(std::path::Path::new(""));
    let name = exe.file_name().filter(|n| !n.is_empty());
    match (name, home) {
        (Some(app), Some(home)) if parent.ends_with("flatpak/exports/bin") => home.join(".var/app").join(app).join("cache"),
        (Some(app), Some(home)) if parent == std::path::Path::new("/snap/bin") => home.join("snap").join(app).join("common"),
        _ => std::env::temp_dir(),
    }
}

impl Browser {
    /// A handle to a browser someone else owns (it is not killed when this drops).
    pub fn attached(endpoint: &Endpoint, product: &str, version: &str) -> Browser {
        Browser {
            endpoint: endpoint.clone(),
            product: product.into(),
            version: version.into(),
            user_agent: String::new(),
            launched: false,
            _proc: None,
            _profile: None,
        }
    }

    /// Resolve a browser from an optional endpoint, falling back to launching a located Chromium.
    pub fn resolve(ctx: &Ctx<'_>, cdp_endpoint: Option<&str>, opts: &LaunchOptions, dl: &Deadline) -> Result<Browser, ToolError> {
        if let Some(ep_str) = cdp_endpoint.map(str::trim).filter(|s| !s.is_empty()) {
            let endpoint = cdp::parse_endpoint(ep_str)?;
            let version = cdp::json_version(&endpoint, dl)?;
            return Ok(Browser {
                product: version.get("Browser").and_then(Value::as_str).unwrap_or("unknown").to_string(),
                version: version.get("Protocol-Version").and_then(Value::as_str).unwrap_or_default().to_string(),
                user_agent: version.get("User-Agent").and_then(Value::as_str).unwrap_or_default().to_string(),
                endpoint,
                launched: false,
                _proc: None,
                _profile: None,
            });
        }
        // A website opened with web_site_open is the page every browser tool looks at by default
        // (only when it is a CDP page; a Camoufox site is the website tools' own).
        if let Some(site) = ctx.session.ext.get::<super::website_tools::SiteBrowser>()
            && let Some(ep) = site.backend.cdp_endpoint()
            && site.backend.alive(dl)
        {
            let info = site.backend.info();
            return Ok(Browser::attached(&ep, &info.product, &info.version));
        }
        if !opts.allow_launch {
            return Err(engine_missing("no endpoint was given"));
        }
        Browser::launch(ctx, opts, dl)
    }

    pub(crate) fn launch(ctx: &Ctx<'_>, opts: &LaunchOptions, dl: &Deadline) -> Result<Browser, ToolError> {
        let exe = craft_anything_engines::locate("chromium", &ctx.session.settings.engines.tools, &ctx.session.settings.engine_store_dir())
            .ok_or_else(|| engine_missing("no Chromium-based browser was found"))?;
        let profile = BrowserProfile(super::artifact_tools::fresh_dir_in(&profile_base(&exe, dirs::home_dir()), "craft-browser")?);
        let mut args: Vec<String> = vec![
            format!("--user-data-dir={}", profile.0.display()),
            "--remote-debugging-port=0".into(),
            "--remote-allow-origins=*".into(),
            "--no-first-run".into(),
            "--no-default-browser-check".into(),
            "--disable-background-networking".into(),
            "--disable-sync".into(),
            "--disable-extensions".into(),
            "--disable-features=Translate".into(),
        ];
        if opts.headless {
            args.push("--headless=new".into());
            args.push("--disable-gpu".into());
        }
        if opts.no_sandbox {
            args.push("--no-sandbox".into());
        }
        args.push("about:blank".into());
        let cancel = ctx.cancel.clone();
        let tail = StderrTail::default();
        let sink = tail.clone();
        let mut proc = Process::new(&exe)
            .args(&args)
            .stdin(StdinMode::Closed)
            .timeout(Duration::from_secs(opts.startup_secs.saturating_add(3600)))
            .cancel_when(move || cancel.is_cancelled())
            .on_stderr_line(move |l| sink.push(l))
            .label("browser")
            // On Windows the first process can hand the browser to another and exit.
            .hands_off()
            .spawn()?;
        let name = exe.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "the browser".into());
        let endpoint = Self::await_devtools(&profile.0, dl, &mut proc, &tail, &name)?;
        let version = cdp::json_version(&endpoint, dl)?;
        Ok(Browser {
            product: version.get("Browser").and_then(Value::as_str).unwrap_or("unknown").to_string(),
            version: version.get("Protocol-Version").and_then(Value::as_str).unwrap_or_default().to_string(),
            user_agent: version.get("User-Agent").and_then(Value::as_str).unwrap_or_default().to_string(),
            endpoint,
            launched: true,
            _proc: Some(proc),
            _profile: Some(profile),
        })
    }

    /// Launch an arbitrary debuggable executable (e.g. an Electron app's own binary) with a fresh
    /// profile and a zero-port remote-debugging listener, then attach to it. Consent-gated callers
    /// use this to drive an app they were given the path to.
    pub fn launch_app(ctx: &Ctx<'_>, exe: &std::path::Path, app_args: &[String], startup_secs: u64, dl: &Deadline) -> Result<Browser, ToolError> {
        if !exe.is_file() {
            return Err(ToolError::new(ErrorCode::NotFound, format!("{} is not a file", exe.display()))
                .with_remediation("Give the absolute path of the app's executable."));
        }
        let profile = BrowserProfile(super::artifact_tools::fresh_temp_dir("craft-electron")?);
        let mut args: Vec<String> = vec![
            format!("--user-data-dir={}", profile.0.display()),
            "--remote-debugging-port=0".into(),
            "--remote-allow-origins=*".into(),
            "--no-first-run".into(),
        ];
        args.extend(app_args.iter().cloned());
        let cancel = ctx.cancel.clone();
        let tail = StderrTail::default();
        let sink = tail.clone();
        let mut proc = Process::new(exe)
            .args(&args)
            .stdin(StdinMode::Closed)
            .timeout(Duration::from_secs(startup_secs.saturating_add(3600)))
            .cancel_when(move || cancel.is_cancelled())
            .on_stderr_line(move |l| sink.push(l))
            .label("electron-app")
            .hands_off()
            .spawn()?;
        let name = exe.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "the app".into());
        let endpoint = Self::await_devtools(&profile.0, dl, &mut proc, &tail, &name)?;
        let version = cdp::json_version(&endpoint, dl)?;
        Ok(Browser {
            product: version.get("Browser").and_then(Value::as_str).unwrap_or("Electron app").to_string(),
            version: version.get("Protocol-Version").and_then(Value::as_str).unwrap_or_default().to_string(),
            user_agent: version.get("User-Agent").and_then(Value::as_str).unwrap_or_default().to_string(),
            endpoint,
            launched: true,
            _proc: Some(proc),
            _profile: Some(profile),
        })
    }

    /// Poll the launched browser's `DevToolsActivePort` file for the chosen loopback port. A browser
    /// that exits first is reported at once; either failure says how long it took and what the
    /// browser last printed, so the cause is in the message.
    fn await_devtools(profile: &std::path::Path, dl: &Deadline, proc: &mut RunningProcess, tail: &StderrTail, name: &str) -> Result<Endpoint, ToolError> {
        let file = profile.join("DevToolsActivePort");
        let started = std::time::Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(&file) {
                let port: Option<u16> = text.lines().next().and_then(|l| l.trim().parse().ok()).filter(|p| *p != 0);
                if let Some(port) = port {
                    return cdp::parse_endpoint(&format!("http://127.0.0.1:{port}"));
                }
            }
            let waited = started.elapsed().as_secs_f32();
            // What the browser last said, when it said anything.
            let said = tail.lines().into_iter().rev().find(|l| !l.trim().is_empty()).map(|l| format!(" It said: {}", l.trim())).unwrap_or_default();
            if proc.is_finished() {
                return Err(ToolError::new(ErrorCode::ProgramFailed, format!("Couldn't start the private browser: {name} closed after {waited:.1} s, before it was ready.{said}"))
                    .with_remediation("Close any windows of it that a crash left behind and try again; to use another Chromium-based browser (Chrome, Edge, Brave), set its path in Settings → Engines.")
                    .with_details(json!({"browser": name, "waited_secs": waited, "stderr": tail.lines()})));
            }
            if dl.expired() {
                return Err(ToolError::new(
                    ErrorCode::TimedOut,
                    format!("Couldn't start the private browser: {name} didn't get ready within {waited:.0} s.{said}"),
                )
                .with_remediation("Try again; if it keeps happening, set another Chromium-based browser (Chrome, Edge, Brave) in Settings → Engines.")
                .with_details(json!({"browser": name, "waited_secs": waited, "stderr": tail.lines()})));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Target selection and page connection

/// Choose a target by id, else the first of `want_kind` (e.g. "page"), from `/json/list`.
pub fn pick_target<'a>(targets: &'a [Value], target_id: Option<&str>, want_kind: Option<&str>) -> Result<&'a Value, ToolError> {
    if let Some(id) = target_id {
        return targets.iter().find(|t| t.get("id").and_then(Value::as_str) == Some(id)).ok_or_else(|| {
            ToolError::new(ErrorCode::NotFound, format!("no CDP target with id {id:?}"))
                .with_remediation("Call web_pages_list to see the available target ids.")
        });
    }
    let chosen = match want_kind {
        Some(k) => targets.iter().find(|t| t.get("type").and_then(Value::as_str) == Some(k)),
        None => targets.first(),
    };
    chosen.ok_or_else(|| {
        ToolError::new(ErrorCode::NotFound, "the browser has no debuggable target of the required kind")
            .with_remediation("Open a page in the browser, or pass an explicit target_id.")
    })
}

/// Connect a page/renderer target's WebSocket and build a JSON-RPC client.
pub fn connect_target(browser: &Browser, target: &Value, max_message: usize, dl: &Deadline) -> Result<CdpClient, ToolError> {
    connect_endpoint(&browser.endpoint, target, max_message, dl)
}

/// [`connect_target`] for an endpoint a backend holds.
pub fn connect_endpoint(endpoint: &Endpoint, target: &Value, max_message: usize, dl: &Deadline) -> Result<CdpClient, ToolError> {
    let ws_url = target.get("webSocketDebuggerUrl").and_then(Value::as_str).ok_or_else(|| {
        ToolError::new(ErrorCode::EngineMissing, "the target has no webSocketDebuggerUrl (another client may be attached)")
            .with_remediation("Close other DevTools clients, or pick a different target.")
    })?;
    let wt = cdp::ws_target(ws_url, endpoint)?;
    let conn = WsConn::connect(&wt, max_message, dl)?;
    Ok(CdpClient::new(conn))
}

// ---------------------------------------------------------------------------------------------
// Browser backends: one page interface for CDP and for Camoufox (R3.14)

/// A region of the page in CSS pixels, in page coordinates (from the page's top left).
#[derive(Debug, Clone, Copy)]
pub struct Clip {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// One page in a running browser — everything the website tools do to it. A CDP page is a
/// Chromium's; a Camoufox page answers the same moves through its Playwright helper.
pub trait PageConn {
    /// Open `url` and wait for it to settle; returns what went wrong while it loaded
    /// ([`super::website_tools::page_health`]).
    fn navigate(&mut self, url: &str, width: u32, height: u32, settle_ms: u64, dl: &Deadline) -> Result<Value, ToolError>;
    /// Evaluate one of our own read-only scripts in the page and return its JSON value.
    fn eval(&mut self, script: &str, dl: &Deadline) -> Result<Value, ToolError>;
    /// A PNG of the page: the whole page cut to `clip` (page coordinates), or of the current
    /// viewport when `clip` is None.
    fn screenshot(&mut self, clip: Option<Clip>, dl: &Deadline) -> Result<Vec<u8>, ToolError>;
    /// Size the page and pin light mode, so captures don't depend on this computer's theme.
    fn set_viewport(&mut self, width: u32, height: u32, dl: &Deadline) -> Result<(), ToolError>;
    fn press_escape(&mut self, dl: &Deadline);
    /// Let late content settle.
    fn settle(&mut self, ms: u64, dl: &Deadline) -> Result<(), ToolError>;
    /// Leave the page tidy (disable what we enabled); bounded even past the deadline.
    fn finish(&mut self, dl: &Deadline);
}

/// What a browser backend is: its identity (for evidence) and a way to reach its page.
pub trait WebBackend: Send + Sync {
    fn info(&self) -> &BackendInfo;
    /// Is the browser still answering?
    fn alive(&self, dl: &Deadline) -> bool;
    /// Connect to the page this backend drives.
    fn page(&self, dl: &Deadline) -> Result<Box<dyn PageConn + '_>, ToolError>;
    /// The loopback CDP endpoint, when this backend speaks CDP (the other browser tools attach
    /// to the session's site through it).
    fn cdp_endpoint(&self) -> Option<Endpoint> {
        None
    }
}

/// The browser's identity, for evidence: the engine that drove it (`cdp` or `camoufox`) and
/// what the browser says it is.
pub struct BackendInfo {
    pub engine: &'static str,
    pub product: String,
    pub version: String,
    pub user_agent: String,
}

/// The evidence environment for a website observation through `info`'s backend.
pub fn backend_env(clean_room: bool, info: &BackendInfo) -> Environment {
    Environment { clean_room, engine: info.engine.into(), engine_version: Some(info.version.clone()), host: host_triple(), language_id: None }
}

/// A page over CDP, in a Chromium.
struct CdpConn(CdpClient);

impl PageConn for CdpConn {
    fn navigate(&mut self, url: &str, width: u32, height: u32, settle_ms: u64, dl: &Deadline) -> Result<Value, ToolError> {
        self.0.call("Page.enable", json!({}), None, dl)?;
        // Errors and failed requests are read from the browser's own events; a browser that
        // refuses one of these still loads the page.
        for domain in ["Runtime.enable", "Log.enable", "Network.enable"] {
            let _ = self.0.call(domain, json!({}), None, dl);
        }
        self.set_viewport(width, height, dl)?;
        let r = self.0.call("Page.navigate", json!({"url": url}), None, dl)?;
        if let Some(err) = r.get("errorText").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            return Err(ToolError::new(ErrorCode::ProgramFailed, format!("the page didn't load: {err}"))
                .with_remediation("Check the address and your internet connection, then try again."));
        }
        // Wait for the load event (bounded by the overall deadline), then let late content settle.
        let loaded = Deadline::new(Duration::from_secs(45), crate::tool::CancelToken::default());
        let mut done = false;
        let mut seen: Vec<craft_anything_engines::cdp::CdpEvent> = Vec::new();
        let mut keep = |events: Vec<craft_anything_engines::cdp::CdpEvent>| {
            let wanted = events.into_iter().filter(|e| e.method.starts_with("Runtime.") || e.method.starts_with("Log.") || e.method.starts_with("Network."));
            seen.extend(wanted.take(MAX_HEALTH_EVENTS.saturating_sub(seen.len())));
        };
        while !done && !loaded.expired() && !dl.expired() {
            let slice = Deadline::new(Duration::from_millis(250), crate::tool::CancelToken::default());
            self.0.pump_until(&slice)?;
            let events = self.0.take_events();
            done = events.iter().any(|e| e.method == "Page.loadEventFired");
            keep(events);
        }
        let settle = Deadline::new(Duration::from_millis(settle_ms.min(30_000)), crate::tool::CancelToken::default());
        self.0.pump_until(&settle)?;
        keep(self.0.take_events());
        for domain in ["Network.disable", "Log.disable"] {
            let _ = self.0.call(domain, json!({}), None, dl);
        }
        Ok(super::website_tools::page_health(&seen))
    }

    fn eval(&mut self, script: &str, dl: &Deadline) -> Result<Value, ToolError> {
        let r = self.0.call("Runtime.evaluate", json!({"expression": script, "returnByValue": true, "awaitPromise": true}), None, dl)?;
        if let Some(ex) = r.get("exceptionDetails") {
            let m = ex.pointer("/exception/description").or_else(|| ex.get("text")).and_then(Value::as_str).unwrap_or("script error");
            return Err(ToolError::new(ErrorCode::ProgramFailed, format!("reading the page failed: {}", m.lines().next().unwrap_or(m))));
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    fn screenshot(&mut self, clip: Option<Clip>, dl: &Deadline) -> Result<Vec<u8>, ToolError> {
        let params = match clip {
            None => json!({"format": "png"}),
            Some(c) => json!({"format": "png", "captureBeyondViewport": true, "clip": {"x": c.x, "y": c.y, "width": c.width, "height": c.height, "scale": 1}}),
        };
        let shot = self.0.call("Page.captureScreenshot", params, None, dl)?;
        let data = shot.get("data").and_then(Value::as_str).ok_or_else(|| ToolError::new(ErrorCode::GarbledOutput, "the browser returned no screenshot"))?;
        b64_decode(data)
    }

    fn set_viewport(&mut self, width: u32, height: u32, dl: &Deadline) -> Result<(), ToolError> {
        self.0.call(
            "Emulation.setDeviceMetricsOverride",
            json!({"width": width, "height": height, "deviceScaleFactor": 1, "mobile": width < 600}),
            None,
            dl,
        )?;
        let _ = self.0.call("Emulation.setEmulatedMedia", json!({"features": [{"name": "prefers-color-scheme", "value": "light"}]}), None, dl);
        Ok(())
    }

    fn press_escape(&mut self, dl: &Deadline) {
        for kind in ["keyDown", "keyUp"] {
            let _ = self.0.call("Input.dispatchKeyEvent", json!({"type": kind, "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27}), None, dl);
        }
    }

    fn settle(&mut self, ms: u64, _dl: &Deadline) -> Result<(), ToolError> {
        let d = Deadline::new(Duration::from_millis(ms), crate::tool::CancelToken::default());
        self.0.pump_until(&d)?;
        let _ = self.0.take_events();
        Ok(())
    }

    fn finish(&mut self, dl: &Deadline) {
        super::browser_tools::cleanup(&mut self.0, dl);
    }
}

/// A Chromium over CDP (launched by us or attached).
pub struct CdpBackend {
    info: BackendInfo,
    endpoint: Endpoint,
    // Owned for its lifetime: dropped (killed, profile removed) when the last Arc goes.
    #[allow(dead_code)]
    browser: craft_anything_platform::sync::Mutex<Browser>,
}

impl WebBackend for CdpBackend {
    fn info(&self) -> &BackendInfo {
        &self.info
    }

    fn alive(&self, dl: &Deadline) -> bool {
        cdp::json_version(&self.endpoint, dl).is_ok()
    }

    fn page(&self, dl: &Deadline) -> Result<Box<dyn PageConn + '_>, ToolError> {
        let targets = cdp::json_list(&self.endpoint, dl)?;
        let target = pick_target(&targets, None, Some("page"))?;
        Ok(Box::new(CdpConn(connect_endpoint(&self.endpoint, target, cdp::PAGE_MAX_MESSAGE, dl)?)))
    }

    fn cdp_endpoint(&self) -> Option<Endpoint> {
        Some(self.endpoint.clone())
    }
}

/// Most browser events read for a page's health (a busy page sends thousands of network events).
const MAX_HEALTH_EVENTS: usize = 20_000;

/// Camoufox through its Playwright helper (R3.14): one helper process, one page.
pub struct CamoufoxBackend {
    info: BackendInfo,
    helper: Arc<craft_anything_platform::sync::Mutex<craft_anything_engines::camoufox::Helper>>,
}

impl Drop for CamoufoxBackend {
    fn drop(&mut self) {
        // Close the browser through its own API first: killing the helper alone leaves the
        // browser dying slowly on its own (seen in a live run). A call still in flight keeps
        // the guard; the helper's own drop kills everything as the backstop.
        if let Ok(mut h) = self.helper.try_lock() {
            h.close();
        }
    }
}

/// A page in Camoufox. Every move is one helper request; each lock is labelled and short-lived.
struct CamoufoxConn(Arc<craft_anything_platform::sync::Mutex<craft_anything_engines::camoufox::Helper>>);

impl CamoufoxConn {
    /// Run one helper request with the guard labelled, so a long page load is waited for, not
    /// treated as a stuck lock.
    fn with_helper<T>(&self, job: &'static str, f: impl FnOnce(&mut craft_anything_engines::camoufox::Helper) -> Result<T, ToolError>) -> Result<T, ToolError> {
        let mut guard = self.0.lock().map_err(|_| ToolError::internal("the Camoufox helper lock is poisoned"))?;
        guard.label(job);
        f(&mut guard)
    }
}

impl PageConn for CamoufoxConn {
    fn navigate(&mut self, url: &str, width: u32, height: u32, settle_ms: u64, dl: &Deadline) -> Result<Value, ToolError> {
        self.with_helper("camoufox-navigate", |h| {
            h.viewport(width, height, dl)?;
            let (_loaded, events) = h.goto(url, settle_ms, dl)?;
            Ok(super::website_tools::page_health(&events))
        })
    }

    fn eval(&mut self, script: &str, dl: &Deadline) -> Result<Value, ToolError> {
        self.with_helper("camoufox-eval", |h| {
            h.eval(script, dl).map_err(|e| {
                if e.code == ErrorCode::ProgramFailed { ToolError::new(ErrorCode::ProgramFailed, format!("reading the page failed: {}", e.message)) } else { e }
            })
        })
    }

    fn screenshot(&mut self, clip: Option<Clip>, dl: &Deadline) -> Result<Vec<u8>, ToolError> {
        self.with_helper("camoufox-screenshot", |h| {
            let shot = h.screenshot(clip.is_some(), dl)?;
            let Some(c) = clip else { return Ok(shot) };
            crop_png(&shot, c)
        })
    }

    fn set_viewport(&mut self, width: u32, height: u32, dl: &Deadline) -> Result<(), ToolError> {
        self.with_helper("camoufox-viewport", |h| h.viewport(width, height, dl))
    }

    fn press_escape(&mut self, dl: &Deadline) {
        let _ = self.with_helper("camoufox-key", |h| h.key("Escape", dl));
    }

    fn settle(&mut self, ms: u64, dl: &Deadline) -> Result<(), ToolError> {
        self.with_helper("camoufox-settle", |h| h.sleep(ms, dl))
    }

    fn finish(&mut self, _dl: &Deadline) {
        // Nothing is left enabled between calls: the helper keeps its own page tidy.
    }
}

impl WebBackend for CamoufoxBackend {
    fn info(&self) -> &BackendInfo {
        &self.info
    }

    fn alive(&self, _dl: &Deadline) -> bool {
        self.helper.lock().map(|mut h| h.alive()).unwrap_or(false)
    }

    fn page(&self, _dl: &Deadline) -> Result<Box<dyn PageConn + '_>, ToolError> {
        Ok(Box::new(CamoufoxConn(self.helper.clone())))
    }
}

/// Cut a full-page PNG to `clip` (page coordinates); rows past the image are white, so a page
/// shorter than the clip still gives the requested size.
pub fn crop_png(bytes: &[u8], clip: Clip) -> Result<Vec<u8>, ToolError> {
    let img = decode_png(bytes)?;
    let (x, y) = ((clip.x.max(0.0)) as u32, (clip.y.max(0.0)) as u32);
    let (w, h) = ((clip.width.max(1.0)) as u32, (clip.height.max(1.0)) as u32);
    let mut rgba = vec![255u8; (w as usize).saturating_mul(h as usize).saturating_mul(4)];
    for row in 0..h {
        let src_y = y.saturating_add(row);
        if src_y >= img.height {
            break;
        }
        for col in 0..w {
            let src_x = x.saturating_add(col);
            if src_x >= img.width {
                break;
            }
            let src = ((src_y as usize).saturating_mul(img.width as usize).saturating_add(src_x as usize)).saturating_mul(4);
            let dst = ((row as usize).saturating_mul(w as usize).saturating_add(col as usize)).saturating_mul(4);
            if let (Some(s), Some(d)) = (img.rgba.get(src..src + 4), rgba.get_mut(dst..dst + 4)) {
                d.copy_from_slice(s);
            }
        }
    }
    encode_png(&Image { width: w, height: h, rgba })
}

/// Start the website tools' browser: Camoufox when the settings ask for it (or say `auto` and
/// it is installed), else a private Chromium. `width`/`height` size the first page.
pub fn open_site_backend(ctx: &Ctx<'_>, opts: &LaunchOptions, width: u32, height: u32, dl: &Deadline) -> Result<Arc<dyn WebBackend>, ToolError> {
    let camoufox = ctx.session.settings.engine_path(craft_anything_engines::camoufox::ID);
    match (ctx.session.settings.engines.web_browser, camoufox) {
        (crate::settings::WebBrowser::Chromium, _) => launch_cdp_backend(ctx, opts, dl),
        (crate::settings::WebBrowser::Auto, None) => launch_cdp_backend(ctx, opts, dl),
        (_, Some(exe)) => launch_camoufox_backend(ctx, &exe, opts.headless, width, height, dl),
        (crate::settings::WebBrowser::Camoufox, None) => Err(ToolError::new(
            ErrorCode::EngineMissing,
            "Camoufox is set as the website browser, but it isn't installed.",
        )
        .with_remediation("Install it (`pip install camoufox`, then `camoufox fetch`) and set its path in Settings → Engines, or set engines.web_browser = \"chromium\" in config.toml.")),
    }
}

fn launch_cdp_backend(ctx: &Ctx<'_>, opts: &LaunchOptions, dl: &Deadline) -> Result<Arc<dyn WebBackend>, ToolError> {
    let b = Browser::launch(ctx, opts, dl)?;
    let info = BackendInfo { engine: "cdp", product: b.product.clone(), version: b.version.clone(), user_agent: b.user_agent.clone() };
    Ok(Arc::new(CdpBackend { info, endpoint: b.endpoint.clone(), browser: craft_anything_platform::sync::Mutex::new(b) }))
}

fn launch_camoufox_backend(
    _ctx: &Ctx<'_>,
    exe: &std::path::Path,
    headless: bool,
    width: u32,
    height: u32,
    dl: &Deadline,
) -> Result<Arc<dyn WebBackend>, ToolError> {
    let work = super::artifact_tools::fresh_dir_in(&profile_base(exe, dirs::home_dir()), "craft-camoufox")?;
    let cmd = craft_anything_engines::camoufox::helper_command(exe, &work)?;
    let mut helper = craft_anything_engines::camoufox::Helper::start(&cmd, &work, dl)?;
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    };
    let opened = helper.open(headless, width, height, os, dl)?;
    let info = BackendInfo {
        engine: "camoufox",
        product: opened.get("product").and_then(Value::as_str).unwrap_or("Camoufox").to_string(),
        version: opened.get("version").and_then(Value::as_str).unwrap_or_default().to_string(),
        user_agent: opened.get("user_agent").and_then(Value::as_str).unwrap_or_default().to_string(),
    };
    Ok(Arc::new(CamoufoxBackend { info, helper: Arc::new(craft_anything_platform::sync::Mutex::new(helper)) }))
}

// ---------------------------------------------------------------------------------------------
// Data minimisation: secrets, URLs, headers

/// Secrets the caller names, to redact wherever they appear, in every encoding.
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    raw: Vec<String>,
    spellings: Vec<String>,
    pub redactions: u64,
}

const REDACTED: &str = "«redacted»";

impl Secrets {
    pub fn new(values: &[String]) -> Self {
        let mut spellings: Vec<String> = Vec::new();
        for v in values.iter().filter(|v| !v.is_empty()) {
            for s in [v.clone(), percent_encode(v), json_escape(v)] {
                if !s.is_empty() && !spellings.contains(&s) {
                    spellings.push(s);
                }
            }
        }
        // Longest first so a value that contains another is replaced whole.
        spellings.sort_by_key(|s| std::cmp::Reverse(s.len()));
        Secrets { raw: values.iter().filter(|v| !v.is_empty()).cloned().collect(), spellings, redactions: 0 }
    }

    pub fn declared(&self) -> usize {
        self.raw.len()
    }

    /// Redact every declared spelling in a string.
    pub fn text(&mut self, s: &str) -> String {
        if self.spellings.is_empty() {
            return s.to_string();
        }
        let mut out = s.to_string();
        for spelling in &self.spellings {
            if out.contains(spelling.as_str()) {
                let n = out.matches(spelling.as_str()).count() as u64;
                out = out.replace(spelling.as_str(), REDACTED);
                self.redactions += n;
            }
        }
        out
    }

    /// Recursively redact declared values throughout a JSON value (keys and strings).
    pub fn scrub(&mut self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.text(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.scrub(x)).collect()),
            Value::Object(o) => {
                let mut m = serde_json::Map::new();
                for (k, val) in o {
                    m.insert(self.text(k), self.scrub(val));
                }
                Value::Object(m)
            }
            other => other.clone(),
        }
    }

    /// True when a string (or its bytes) still contains any raw declared value — used by tests to
    /// prove nothing leaked.
    pub fn leaks_in(&self, haystack: &str) -> bool {
        self.raw.iter().any(|v| haystack.contains(v.as_str()))
    }
}

/// Minimal `encodeURIComponent`-style encoding for redaction spelling coverage.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')');
        if unreserved {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// JSON string-escaping of a value's inner text (without the surrounding quotes).
pub fn json_escape(s: &str) -> String {
    let quoted = serde_json::Value::String(s.to_string()).to_string();
    quoted.get(1..quoted.len().saturating_sub(1)).unwrap_or("").to_string()
}

/// Remove userinfo (`user:pass@`) from a URL and drop query values that obviously hold credentials.
pub fn sanitize_url(url: &str) -> String {
    let url = url.trim();
    // Strip scheme://userinfo@ → scheme://host…
    if let Some(scheme_end) = url.find("://") {
        let (scheme, rest) = url.split_at(scheme_end + 3);
        let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        if let Some(at) = rest.get(..auth_end).and_then(|a| a.rfind('@')) {
            let after = rest.get(at + 1..).unwrap_or("");
            return format!("{scheme}{after}");
        }
    }
    url.to_string()
}

/// Header names we keep from a response, by lower-case name. Cookies and credentials are never
/// kept; most values are dropped and only the length/encoding of others is retained.
fn header_kept(name: &str) -> bool {
    matches!(
        name,
        "content-type"
            | "content-length"
            | "content-encoding"
            | "content-security-policy"
            | "content-security-policy-report-only"
            | "link"
            | "referrer-policy"
            | "cross-origin-opener-policy"
            | "cross-origin-embedder-policy"
            | "cross-origin-resource-policy"
            | "x-content-type-options"
            | "strict-transport-security"
            | "cache-control"
    )
}

const HEADER_BLOCKED: &[&str] = &["cookie", "set-cookie", "authorization", "proxy-authorization"];

/// Project a CDP headers object (name → value) down to the allowlist. Blocked credential headers
/// are reported only by presence; other values are replaced by their length.
pub fn project_headers(headers: &Value) -> Value {
    let mut kept = serde_json::Map::new();
    let mut blocked: Vec<String> = Vec::new();
    if let Some(o) = headers.as_object() {
        for (k, v) in o {
            let lower = k.to_ascii_lowercase();
            if HEADER_BLOCKED.contains(&lower.as_str()) {
                blocked.push(lower);
                continue;
            }
            if header_kept(&lower) {
                kept.insert(lower, v.clone());
            } else {
                let len = v.as_str().map(str::len).unwrap_or(0);
                kept.insert(lower, json!({"value_length": len}));
            }
        }
    }
    json!({"kept": kept, "credentials_present": blocked})
}

/// Every header with its value; credential headers are always redacted, and declared secrets
/// are redacted wherever they appear.
pub fn header_values(headers: &Value, secrets: &mut Secrets) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(o) = headers.as_object() {
        for (k, v) in o {
            let lower = k.to_ascii_lowercase();
            let value = if HEADER_BLOCKED.contains(&lower.as_str()) { REDACTED.to_string() } else { secrets.text(v.as_str().unwrap_or_default()) };
            out.insert(lower, json!(value));
        }
    }
    Value::Object(out)
}

/// `scheme://host[:port]` of an http(s) URL.
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next().filter(|h| !h.is_empty())?;
    Some(format!("{scheme}://{host}"))
}

/// Largest body text kept per request or response.
pub const MAX_KEPT_BODY: usize = 64 * 1024;

/// A body as kept in evidence: UTF-8 text with secrets redacted (capped), or for binary data only
/// its size. The digest is of the redacted text, so it can't be used to guess a secret.
pub fn kept_body(raw: &[u8], secrets: &mut Secrets) -> Value {
    kept_text(raw, secrets, MAX_KEPT_BODY)
}

/// [`kept_body`] with its own cap on the kept text.
pub fn kept_text(raw: &[u8], secrets: &mut Secrets, cap: usize) -> Value {
    match std::str::from_utf8(raw) {
        Ok(text) => {
            let redacted = secrets.text(text);
            let mut end = redacted.len().min(cap);
            while !redacted.is_char_boundary(end) {
                end -= 1;
            }
            json!({
                "state": "captured",
                "bytes": raw.len(),
                "sha256": craft_anything_evidence::sha256_hex(redacted.as_bytes()),
                "text": redacted.get(..end).unwrap_or_default(),
                "truncated": end < redacted.len(),
            })
        }
        Err(_) => json!({"state": "captured", "bytes": raw.len(), "binary": true}),
    }
}

// ---------------------------------------------------------------------------------------------
// JSON body outlines (no values)

/// Reduce a JSON value to a typed outline with counts. No scalar values survive.
pub fn json_outline(v: &Value) -> Value {
    outline_inner(v, 0)
}

fn outline_inner(v: &Value, depth: usize) -> Value {
    if depth > 24 {
        return json!({"kind": "max-depth"});
    }
    match v {
        Value::Null => json!({"kind": "null"}),
        Value::Bool(_) => json!({"kind": "boolean"}),
        Value::Number(_) => json!({"kind": "number"}),
        Value::String(_) => json!({"kind": "string"}),
        Value::Array(a) => {
            // Summarise element outlines without keeping values.
            let mut sample: Vec<Value> = Vec::new();
            for e in a.iter().take(8) {
                let s = outline_inner(e, depth + 1);
                if !sample.contains(&s) {
                    sample.push(s);
                }
            }
            json!({"kind": "array", "count": a.len(), "element_outlines": sample})
        }
        Value::Object(o) => {
            let mut props: Vec<Value> = Vec::new();
            for (k, val) in o.iter().take(256) {
                props.push(json!({"name": k, "value": outline_inner(val, depth + 1)}));
            }
            json!({"kind": "object", "property_count": o.len(), "properties": props})
        }
    }
}

// ---------------------------------------------------------------------------------------------
// DOM summary (node types/names, attribute names and value lengths only)

/// Summarise a `DOM.getDocument` tree: counts and a bounded structure of node names/types with
/// attribute names and *value lengths* only. No text content or attribute values are kept.
pub fn dom_summary(root: &Value, max_nodes: usize) -> Value {
    let mut element_counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut attr_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut total = 0u64;
    let mut text_nodes = 0u64;
    let mut budget = max_nodes;
    walk_dom(root, &mut element_counts, &mut attr_names, &mut total, &mut text_nodes, &mut budget, 0);
    json!({
        "total_nodes": total,
        "text_nodes": text_nodes,
        "elements_by_name": element_counts,
        "attribute_names": attr_names.into_iter().collect::<Vec<_>>(),
        "truncated": budget == 0,
    })
}

#[allow(clippy::too_many_arguments)]
fn walk_dom(
    node: &Value,
    elems: &mut std::collections::BTreeMap<String, u64>,
    attrs: &mut std::collections::BTreeSet<String>,
    total: &mut u64,
    text_nodes: &mut u64,
    budget: &mut usize,
    depth: usize,
) {
    if *budget == 0 || depth > 4096 {
        return;
    }
    *budget -= 1;
    *total += 1;
    let node_type = node.get("nodeType").and_then(Value::as_u64).unwrap_or(0);
    if node_type == 3 {
        *text_nodes += 1;
    } else if node_type == 1 {
        let name = node.get("nodeName").and_then(Value::as_str).unwrap_or("?").to_ascii_lowercase();
        *elems.entry(name).or_insert(0) += 1;
    }
    // attributes is a flat [name, value, name, value, …] array.
    if let Some(a) = node.get("attributes").and_then(Value::as_array) {
        let mut i = 0;
        while i < a.len() {
            if let Some(n) = a.get(i).and_then(Value::as_str) {
                attrs.insert(n.to_ascii_lowercase());
            }
            i += 2;
        }
    }
    for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
        walk_dom(child, elems, attrs, total, text_nodes, budget, depth + 1);
    }
    if let Some(cd) = node.get("contentDocument") {
        walk_dom(cd, elems, attrs, total, text_nodes, budget, depth + 1);
    }
}

// ---------------------------------------------------------------------------------------------
// base64 and PNG

pub fn b64_decode(s: &str) -> Result<Vec<u8>, ToolError> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s.trim()).map_err(|e| ToolError::new(ErrorCode::GarbledOutput, format!("invalid base64: {e}")))
}

pub fn b64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// A decoded image as 8-bit RGBA.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Read a PNG's declared dimensions without decoding the pixels.
pub fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), ToolError> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let reader = decoder.read_info().map_err(|e| ToolError::new(ErrorCode::GarbledOutput, format!("not a readable PNG: {e}")))?;
    let info = reader.info();
    Ok((info.width, info.height))
}

/// Decode a picture (PNG, JPEG, WebP, or a GIF's first frame) to 8-bit RGBA, within the same
/// memory budget as [`decode_png`].
pub fn decode_picture(bytes: &[u8]) -> Result<Image, ToolError> {
    if bytes.starts_with(b"\x89PNG") {
        return decode_png(bytes);
    }
    let unreadable = |e: image::ImageError| ToolError::new(ErrorCode::GarbledOutput, format!("not a readable PNG, JPEG, WebP or GIF picture: {e}"));
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| unreadable(e.into()))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_PIXELS_WORKING_SET);
    reader.limits(limits);
    let rgba = reader.decode().map_err(unreadable)?.into_rgba8();
    Ok(Image { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
}

/// Decode a PNG to 8-bit RGBA, refusing anything whose working set would exceed the budget.
pub fn decode_png(bytes: &[u8]) -> Result<Image, ToolError> {
    let (w, h) = png_dimensions(bytes)?;
    if (w as u64).saturating_mul(h as u64).saturating_mul(4) > MAX_PIXELS_WORKING_SET {
        return Err(ToolError::new(ErrorCode::OverLimit, format!("the image is too large to decode ({w}×{h})")));
    }
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| ToolError::new(ErrorCode::GarbledOutput, format!("not a readable PNG: {e}")))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).map_err(|e| ToolError::new(ErrorCode::GarbledOutput, format!("could not decode the PNG: {e}")))?;
    let channels = frame.color_type.samples();
    let (fw, fh) = (frame.width, frame.height);
    let pixels = (fw as usize).saturating_mul(fh as usize);
    let mut rgba = vec![0u8; pixels.saturating_mul(4)];
    let data = buf.get(..frame.buffer_size()).unwrap_or(&buf);
    for i in 0..pixels {
        let src = i.saturating_mul(channels);
        let dst = i.saturating_mul(4);
        let (r, g, b, a) = match channels {
            1 => {
                let v = data.get(src).copied().unwrap_or(0);
                (v, v, v, 255)
            }
            2 => {
                let v = data.get(src).copied().unwrap_or(0);
                (v, v, v, data.get(src + 1).copied().unwrap_or(255))
            }
            3 => (data.get(src).copied().unwrap_or(0), data.get(src + 1).copied().unwrap_or(0), data.get(src + 2).copied().unwrap_or(0), 255),
            _ => (
                data.get(src).copied().unwrap_or(0),
                data.get(src + 1).copied().unwrap_or(0),
                data.get(src + 2).copied().unwrap_or(0),
                data.get(src + 3).copied().unwrap_or(255),
            ),
        };
        if let Some(px) = rgba.get_mut(dst..dst + 4) {
            px.copy_from_slice(&[r, g, b, a]);
        }
    }
    Ok(Image { width: fw, height: fh, rgba })
}

/// Compare two decoded images. A pixel differs when any channel's absolute difference exceeds the
/// threshold. Returns a value-only summary (no pixel data).
pub fn pixel_diff(a: &Image, b: &Image, tolerance: u8) -> Value {
    if a.width != b.width || a.height != b.height {
        return json!({
            "comparable": false,
            "reason": "different dimensions",
            "before": {"width": a.width, "height": a.height},
            "after": {"width": b.width, "height": b.height},
        });
    }
    let total = (a.width as u64).saturating_mul(a.height as u64);
    let mut differing = 0u64;
    let mut max_channel_delta = 0u8;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for i in 0..(total as usize) {
        let o = i.saturating_mul(4);
        let (Some(pa), Some(pb)) = (a.rgba.get(o..o + 4), b.rgba.get(o..o + 4)) else { break };
        let delta = pa.iter().zip(pb.iter()).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        if delta > tolerance {
            differing += 1;
            max_channel_delta = max_channel_delta.max(delta);
            let x = (i as u32) % a.width;
            let y = (i as u32) / a.width;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    let fraction = if total == 0 { 0.0 } else { differing as f64 / total as f64 };
    let bbox = if differing == 0 { Value::Null } else { json!({"x": min_x, "y": min_y, "width": max_x - min_x + 1, "height": max_y - min_y + 1}) };
    json!({
        "comparable": true,
        "identical": differing == 0,
        "width": a.width,
        "height": a.height,
        "total_pixels": total,
        "differing_pixels": differing,
        "difference_fraction": fraction,
        "max_channel_delta": max_channel_delta,
        "tolerance": tolerance,
        "changed_bounding_box": bbox,
    })
}

/// The top-left `w`×`h` part of an image (clamped to its size).
pub fn crop(img: &Image, w: u32, h: u32) -> Image {
    let (w, h) = (w.min(img.width), h.min(img.height));
    let mut rgba = Vec::with_capacity((w as usize).saturating_mul(h as usize).saturating_mul(4));
    for y in 0..h as usize {
        let start = y.saturating_mul(img.width as usize).saturating_mul(4);
        if let Some(row) = img.rgba.get(start..start.saturating_add((w as usize).saturating_mul(4))) {
            rgba.extend_from_slice(row);
        }
    }
    Image { width: w, height: h, rgba }
}

/// The parts of a page that change by themselves (a carousel, a video, an animation), as one bit
/// per `tile`×`tile` square, found by photographing the page twice ([`NoiseMask::between`]).
/// Comparisons leave them out: a build can't be expected to match a frame that moves.
#[derive(Debug, Clone, PartialEq)]
pub struct NoiseMask {
    pub tile: u32,
    pub cols: u32,
    pub rows: u32,
    pub bits: Vec<bool>,
}

/// The tile size noise is found and stored at.
pub const NOISE_TILE: u32 = 16;

impl NoiseMask {
    /// The tiles whose mean colour differs by more than `threshold` per channel between two
    /// pictures of the same page, grown by one tile so a moving edge is covered too.
    pub fn between(a: &Image, b: &Image, threshold: u8) -> NoiseMask {
        let tile = NOISE_TILE;
        let (w, h) = (a.width.min(b.width), a.height.min(b.height));
        let (cols, rows) = (w.div_ceil(tile), h.div_ceil(tile));
        let changed: Vec<bool> = (0..rows)
            .flat_map(|ty| (0..cols).map(move |tx| (tx, ty)))
            .map(|(tx, ty)| tile_mean(a, tile, tx, ty).iter().zip(tile_mean(b, tile, tx, ty)).any(|(x, y)| (x - y).abs() > f64::from(threshold)))
            .collect();
        let at = |x: i64, y: i64| {
            x >= 0 && y >= 0 && x < i64::from(cols) && y < i64::from(rows) && changed.get((y * i64::from(cols) + x) as usize).copied().unwrap_or(false)
        };
        let bits = (0..rows)
            .flat_map(|ty| (0..cols).map(move |tx| (i64::from(tx), i64::from(ty))))
            .map(|(x, y)| (-1..=1).any(|dy| (-1..=1).any(|dx| at(x + dx, y + dy))))
            .collect();
        NoiseMask { tile, cols, rows, bits }
    }

    /// The tiles either mask covers (two masks of the same page).
    pub fn union(mut self, other: &NoiseMask) -> NoiseMask {
        if (self.cols, self.rows) == (other.cols, other.rows) {
            for (a, b) in self.bits.iter_mut().zip(&other.bits) {
                *a |= *b;
            }
        }
        self
    }

    /// The pixel (`x`, `y`) of the page is in a part that changes by itself.
    pub fn covers(&self, x: u32, y: u32) -> bool {
        let (tx, ty) = (x / self.tile.max(1), y / self.tile.max(1));
        tx < self.cols && ty < self.rows && self.bits.get((ty * self.cols + tx) as usize).copied().unwrap_or(false)
    }

    /// How many tiles change by themselves.
    pub fn count(&self) -> usize {
        self.bits.iter().filter(|b| **b).count()
    }

    /// As a PNG with one pixel per tile, white where the page changes by itself.
    pub fn to_png(&self) -> Result<Vec<u8>, ToolError> {
        let rgba = self.bits.iter().flat_map(|b| if *b { [255, 255, 255, 255] } else { [0, 0, 0, 255] }).collect();
        encode_png(&Image { width: self.cols.max(1), height: self.rows.max(1), rgba })
    }

    /// Read back a mask saved with [`NoiseMask::to_png`].
    pub fn from_png(bytes: &[u8]) -> Result<NoiseMask, ToolError> {
        let img = decode_png(bytes)?;
        let bits = img.rgba.as_chunks::<4>().0.iter().map(|[r, ..]| *r > 127).collect();
        Ok(NoiseMask { tile: NOISE_TILE, cols: img.width, rows: img.height, bits })
    }
}

/// The mean colour of tile (`tx`, `ty`) of `img`.
fn tile_mean(img: &Image, block: u32, bx: u32, by: u32) -> [f64; 3] {
    let mut sum = [0u64; 3];
    let mut n = 0u64;
    for y in by * block..((by + 1) * block).min(img.height) {
        for x in bx * block..((bx + 1) * block).min(img.width) {
            let o = ((y as usize) * (img.width as usize) + x as usize) * 4;
            if let Some(px) = img.rgba.get(o..o + 3) {
                for (s, v) in sum.iter_mut().zip(px) {
                    *s += u64::from(*v);
                }
                n += 1;
            }
        }
    }
    let n = n.max(1) as f64;
    [sum[0] as f64 / n, sum[1] as f64 / n, sum[2] as f64 / n]
}

/// How alike two same-size images look at a coarse scale: the share of `block`×`block` tiles
/// whose mean colours are within `threshold` per channel. Tolerant of anti-aliasing and
/// sub-pixel shifts, unlike an exact pixel diff. 1.0 for identical images.
pub fn block_similarity(a: &Image, b: &Image, block: u32, threshold: u8) -> f64 {
    block_similarity_masked(a, b, block, threshold, None)
}

/// [`block_similarity`] over the tiles `mask` doesn't cover (a tile counts when its top-left
/// pixel isn't in a moving part). Everything masked compares as 1.0: nothing was left to differ.
pub fn block_similarity_masked(a: &Image, b: &Image, block: u32, threshold: u8, mask: Option<&NoiseMask>) -> f64 {
    let block = block.max(1);
    if a.width != b.width || a.height != b.height || a.width == 0 || a.height == 0 {
        return 0.0;
    }
    let mean = |img: &Image, bx: u32, by: u32| tile_mean(img, block, bx, by);
    let (cols, rows) = (a.width.div_ceil(block), a.height.div_ceil(block));
    let (mut same, mut counted) = (0u64, 0u64);
    for by in 0..rows {
        for bx in 0..cols {
            if mask.is_some_and(|m| m.covers(bx * block, by * block)) {
                continue;
            }
            counted += 1;
            let (ma, mb) = (mean(a, bx, by), mean(b, bx, by));
            if ma.iter().zip(mb.iter()).all(|(x, y)| (x - y).abs() <= f64::from(threshold)) {
                same += 1;
            }
        }
    }
    if counted == 0 { 1.0 } else { same as f64 / counted as f64 }
}

/// `img` as an 8-bit RGBA PNG.
pub fn encode_png(img: &Image) -> Result<Vec<u8>, ToolError> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| ToolError::internal(format!("PNG header: {e}")))?;
        w.write_image_data(&img.rgba).map_err(|e| ToolError::internal(format!("PNG data: {e}")))?;
    }
    Ok(out)
}

/// `img` as an 8-bit RGBA PNG of at most `max` bytes, or `None` when it would be bigger. It's
/// compressed a chunk at a time, so a picture that won't fit stops as soon as it passes `max`
/// instead of being held whole first.
pub fn encode_png_within(img: &Image, max: usize) -> Result<Option<Vec<u8>>, ToolError> {
    /// Takes bytes until `max`, then refuses them and says so.
    struct Capped {
        out: Vec<u8>,
        max: usize,
        over: bool,
    }
    impl std::io::Write for Capped {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.out.len().saturating_add(buf.len()) > self.max {
                self.over = true;
                return Err(std::io::Error::other("the PNG passed its size limit"));
            }
            self.out.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut capped = Capped { out: Vec::new(), max, over: false };
    let written = (|| {
        let mut enc = png::Encoder::new(&mut capped, img.width, img.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| format!("PNG header: {e}"))?;
        let mut stream = w.stream_writer().map_err(|e| format!("PNG data: {e}"))?;
        std::io::Write::write_all(&mut stream, &img.rgba).map_err(|e| format!("PNG data: {e}"))?;
        stream.finish().map_err(|e| format!("PNG data: {e}"))?;
        w.finish().map_err(|e| format!("PNG end: {e}"))
    })();
    match written {
        _ if capped.over => Ok(None),
        Ok(()) => Ok(Some(capped.out)),
        Err(e) => Err(ToolError::internal(e)),
    }
}

/// `img` scaled to `w`×`h` (box sampling: each output pixel is the mean of the input pixels it
/// covers). Aspect ratio isn't kept: comparisons bring both pictures to one size.
pub fn resize(img: &Image, w: u32, h: u32) -> Image {
    let (w, h) = (w.max(1), h.max(1));
    let mut rgba = vec![0u8; (w as usize).saturating_mul(h as usize).saturating_mul(4)];
    if img.width == 0 || img.height == 0 {
        return Image { width: w, height: h, rgba };
    }
    for oy in 0..h {
        let (y0, y1) = (u64::from(oy) * u64::from(img.height) / u64::from(h), (u64::from(oy + 1) * u64::from(img.height)).div_ceil(u64::from(h)));
        for ox in 0..w {
            let (x0, x1) = (u64::from(ox) * u64::from(img.width) / u64::from(w), (u64::from(ox + 1) * u64::from(img.width)).div_ceil(u64::from(w)));
            let mut sum = [0u64; 4];
            let mut n = 0u64;
            for y in y0..y1.max(y0 + 1).min(u64::from(img.height)) {
                for x in x0..x1.max(x0 + 1).min(u64::from(img.width)) {
                    let o = ((y * u64::from(img.width) + x) * 4) as usize;
                    if let Some(px) = img.rgba.get(o..o + 4) {
                        for (s, v) in sum.iter_mut().zip(px) {
                            *s += u64::from(*v);
                        }
                        n += 1;
                    }
                }
            }
            let o = ((u64::from(oy) * u64::from(w) + u64::from(ox)) * 4) as usize;
            if let Some(dst) = rgba.get_mut(o..o + 4) {
                for (d, s) in dst.iter_mut().zip(sum) {
                    *d = (s / n.max(1)) as u8;
                }
            }
        }
    }
    Image { width: w, height: h, rgba }
}

/// How alike two same-size images look to a person: the mean structural similarity (SSIM) of
/// their brightness, over 8×8 windows, after shrinking both so the wider side is at most 512 px.
/// It sees edges, text and texture that tile colours miss, and forgives small colour shifts.
/// 1.0 for identical images; 0.0 for different sizes or nothing to compare.
pub fn perceptual_similarity(a: &Image, b: &Image) -> f64 {
    perceptual_similarity_masked(a, b, None)
}

/// [`perceptual_similarity`] over the windows whose top-left pixel `mask` doesn't cover; 1.0
/// when everything is masked.
pub fn perceptual_similarity_masked(a: &Image, b: &Image, mask: Option<&NoiseMask>) -> f64 {
    if a.width != b.width || a.height != b.height || a.width == 0 || a.height == 0 {
        return 0.0;
    }
    let f = a.width.max(a.height).div_ceil(512).max(1);
    let (w, h) = (a.width.div_ceil(f) as usize, a.height.div_ceil(f) as usize);
    let luma = |img: &Image| -> Vec<f64> {
        let mut out = vec![0f64; w.saturating_mul(h)];
        let mut n = vec![0u32; w.saturating_mul(h)];
        for y in 0..img.height as usize {
            for x in 0..img.width as usize {
                let o = (y * img.width as usize + x) * 4;
                if let Some(px) = img.rgba.get(o..o + 3) {
                    let l = 0.299 * f64::from(px[0]) + 0.587 * f64::from(px[1]) + 0.114 * f64::from(px[2]);
                    let i = (y / f as usize) * w + x / f as usize;
                    if let (Some(s), Some(c)) = (out.get_mut(i), n.get_mut(i)) {
                        *s += l;
                        *c += 1;
                    }
                }
            }
        }
        for (s, c) in out.iter_mut().zip(&n) {
            *s /= f64::from((*c).max(1));
        }
        out
    };
    let (la, lb) = (luma(a), luma(b));
    let (c1, c2) = ((0.01f64 * 255.0).powi(2), (0.03f64 * 255.0).powi(2));
    const WIN: usize = 8;
    let (mut total, mut windows, mut masked) = (0f64, 0u64, 0u64);
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            // Window coordinates are in the shrunk image; the mask is in page pixels.
            if mask.is_some_and(|m| m.covers((x as u32).saturating_mul(f), (y as u32).saturating_mul(f))) {
                masked += 1;
                x += WIN;
                continue;
            }
            let (ye, xe) = ((y + WIN).min(h), (x + WIN).min(w));
            let n = ((ye - y) * (xe - x)) as f64;
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0f64, 0f64, 0f64, 0f64, 0f64);
            for yy in y..ye {
                for xx in x..xe {
                    let i = yy * w + xx;
                    let (p, q) = (la.get(i).copied().unwrap_or(0.0), lb.get(i).copied().unwrap_or(0.0));
                    sa += p;
                    sb += q;
                    saa += p * p;
                    sbb += q * q;
                    sab += p * q;
                }
            }
            let (ma, mb) = (sa / n, sb / n);
            let (va, vb, cov) = ((saa / n - ma * ma).max(0.0), (sbb / n - mb * mb).max(0.0), sab / n - ma * mb);
            total += ((2.0 * ma * mb + c1) * (2.0 * cov + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            windows += 1;
            x += WIN;
        }
        y += WIN;
    }
    match (windows, masked) {
        (0, 0) => 0.0,
        (0, _) => 1.0,
        _ => (total / windows as f64).clamp(0.0, 1.0),
    }
}

/// Build the inline PNG screenshot artifact (with its sha256).
pub fn png_artifact(bytes: &[u8]) -> Result<Value, ToolError> {
    if bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err(ToolError::new(ErrorCode::OverLimit, "the screenshot is larger than the inline limit"));
    }
    let (w, h) = png_dimensions(bytes).unwrap_or((0, 0));
    Ok(json!({
        "media_type": "image/png",
        "bytes": bytes.len(),
        "width": w,
        "height": h,
        "sha256": sha256_hex(bytes),
        "base64": b64_encode(bytes),
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_launched_browser_deletes_its_profile_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("private-profile");
        std::fs::create_dir(&profile).unwrap();
        std::fs::write(profile.join("Preferences"), b"private browser state").unwrap();
        let browser = super::Browser {
            endpoint: super::cdp::parse_endpoint("http://127.0.0.1:9222").unwrap(),
            product: "Test Browser".into(),
            version: String::new(),
            user_agent: String::new(),
            launched: true,
            _proc: None,
            _profile: Some(super::BrowserProfile(profile.clone())),
        };
        drop(browser);
        assert!(!profile.exists(), "the private browser profile survived its owner");
    }

    /// #747: a Flatpak or Snap browser keeps its profile where its sandbox can reach it and we can
    /// read its port; any other browser uses the temp folder.
    #[test]
    fn a_sandboxed_browser_gets_a_profile_it_can_reach() {
        use std::path::{Path, PathBuf};
        let home = || Some(PathBuf::from("/home/me"));
        let base = |exe: &str| super::profile_base(Path::new(exe), home());
        assert_eq!(base("/var/lib/flatpak/exports/bin/com.google.Chrome"), Path::new("/home/me/.var/app/com.google.Chrome/cache"));
        assert_eq!(base("/home/me/.local/share/flatpak/exports/bin/com.brave.Browser"), Path::new("/home/me/.var/app/com.brave.Browser/cache"));
        assert_eq!(base("/snap/bin/chromium"), Path::new("/home/me/snap/chromium/common"));
        assert_eq!(base("/usr/bin/google-chrome"), std::env::temp_dir());
        assert_eq!(super::profile_base(Path::new("/var/lib/flatpak/exports/bin/com.google.Chrome"), None), std::env::temp_dir(), "no home folder");
    }

    /// The capped encoder gives a PNG of the same pixels when it fits, and stops without one when
    /// it doesn't: noise hardly compresses, so 64×64 of it can't fit in 4 KB.
    #[test]
    fn a_png_over_its_limit_is_refused_while_encoding() {
        let small = Image { width: 3, height: 2, rgba: (0..24).collect() };
        let png = encode_png_within(&small, 1 << 20).unwrap().unwrap();
        let back = decode_png(&png).unwrap();
        assert_eq!((back.width, back.height, back.rgba), (3, 2, small.rgba.clone()));
        assert_eq!(encode_png_within(&small, png.len()).unwrap().as_deref(), Some(png.as_slice()), "exactly the limit fits");
        assert_eq!(encode_png_within(&small, png.len() - 1).unwrap(), None);
        let mut seed = 0x2545_f491_u32;
        let noise: Vec<u8> = (0..64 * 64 * 4)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        let noisy = Image { width: 64, height: 64, rgba: noise };
        assert_eq!(encode_png_within(&noisy, 4096).unwrap(), None);
        let fits = encode_png_within(&noisy, 1 << 20).unwrap().unwrap();
        assert_eq!(png_dimensions(&fits).unwrap(), (64, 64));
    }

    use super::*;

    #[test]
    fn origin_of_keeps_scheme_host_and_port() {
        assert_eq!(origin_of("https://app.test:8443/a/b?c#d").as_deref(), Some("https://app.test:8443"));
        assert_eq!(origin_of("http://x.test").as_deref(), Some("http://x.test"));
        assert_eq!(origin_of("file:///tmp/a.html"), None);
        assert_eq!(origin_of("https://"), None);
    }

    #[test]
    fn runtime_is_named_from_the_version_info() {
        let ua = |s: &str| format!("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) {s}");
        assert_eq!(runtime_of("Chrome/120.0.6099.291", &ua("Notes/1.0.0 Chrome/120.0.6099.291 Electron/28.2.10 Safari/537.36")), "electron");
        assert_eq!(runtime_of("Edg/120.0.2210.91", &ua("Chrome/120.0.0.0 Safari/537.36 Edg/120.0.2210.91")), "edge");
        assert_eq!(runtime_of("Chrome/120.0.6099.71", &ua("Chrome/120.0.6099.71 Safari/537.36")), "chrome");
        assert_eq!(runtime_of("HeadlessChrome/120", ""), "chrome");
        assert_eq!(runtime_of("Electron app", ""), "electron");
        assert_eq!(runtime_of("unknown", ""), "unknown");
    }

    #[test]
    fn secrets_redacted_in_every_encoding() {
        let mut s = Secrets::new(&["tok-123".into(), "p@ss word".into()]);
        // Raw, percent-encoded, and JSON-escaped spellings are all caught.
        let v = json!({
            "url": "https://x/?t=tok-123",
            "encoded": "value=tok-123&p=p%40ss%20word",
            "tok-123": "key is a secret too",
            "nested": ["tok-123", {"p@ss word": 1}],
        });
        let out = s.scrub(&v);
        let text = out.to_string();
        assert!(!s.leaks_in(&text), "{text}");
        assert!(!text.contains("tok-123"));
        assert!(!text.contains("p%40ss%20word"));
        assert!(s.redactions >= 4);
    }

    #[test]
    fn headers_drop_credentials() {
        let h = json!({"Set-Cookie": "a=b", "Authorization": "Bearer x", "Content-Type": "text/html", "X-Custom": "secretvalue"});
        let p = project_headers(&h);
        let text = p.to_string();
        assert!(!text.contains("Bearer x") && !text.contains("a=b") && !text.contains("secretvalue"), "{text}");
        assert_eq!(p["kept"]["content-type"], "text/html");
        assert_eq!(p["kept"]["x-custom"]["value_length"], 11);
        let creds = p["credentials_present"].as_array().unwrap();
        assert!(creds.iter().any(|c| c == "set-cookie") && creds.iter().any(|c| c == "authorization"));
    }

    #[test]
    fn json_outline_keeps_no_values() {
        let v = json!({"id": 12, "title": "my secret diary", "tags": ["a", "b"], "nested": {"flag": true}});
        let outline = json_outline(&v);
        let text = outline.to_string();
        assert!(!text.contains("my secret diary") && !text.contains("12"), "{text}");
        assert_eq!(outline["kind"], "object");
        assert_eq!(outline["property_count"], 4);
    }

    #[test]
    fn sanitize_url_strips_userinfo() {
        assert_eq!(sanitize_url("https://user:pw@host/path?q=1"), "https://host/path?q=1");
        assert_eq!(sanitize_url("https://user:pw@host/@profile"), "https://host/@profile");
        assert_eq!(sanitize_url("https://host/x"), "https://host/x");
        assert_eq!(sanitize_url("https://example.test/@profile"), "https://example.test/@profile");
        assert_eq!(sanitize_url("https://cdn.example.test/icon@2x.png"), "https://cdn.example.test/icon@2x.png");
        assert_eq!(sanitize_url("https://example.test/search?to=reader@example.test"), "https://example.test/search?to=reader@example.test");
    }

    #[test]
    fn dom_summary_counts_without_text() {
        let dom = json!({"nodeType": 9, "nodeName": "#document", "children": [
            {"nodeType": 1, "nodeName": "HTML", "children": [
                {"nodeType": 1, "nodeName": "BODY", "attributes": ["class", "main secret"], "children": [
                    {"nodeType": 3, "nodeName": "#text"},
                    {"nodeType": 1, "nodeName": "INPUT", "attributes": ["type", "password", "value", "hunter2"]}
                ]}
            ]}
        ]});
        let s = dom_summary(&dom, 1000);
        let text = s.to_string();
        assert!(!text.contains("hunter2") && !text.contains("main secret"), "{text}");
        assert_eq!(s["elements_by_name"]["input"], 1);
        assert_eq!(s["text_nodes"], 1);
        assert!(s["attribute_names"].as_array().unwrap().iter().any(|a| a == "value"));
    }

    #[test]
    fn pictures_decode_from_png_jpeg_webp_and_gif() {
        let pic = image::RgbaImage::from_fn(16, 8, |x, _| if x < 8 { image::Rgba([200, 40, 40, 255]) } else { image::Rgba([20, 20, 120, 255]) });
        let as_bytes = |f: image::ImageFormat| {
            let mut out = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(pic.clone()).into_rgb8().write_to(&mut out, f).unwrap();
            out.into_inner()
        };
        let png = encode_png(&Image { width: 16, height: 8, rgba: pic.clone().into_raw() }).unwrap();
        for (name, bytes) in [
            ("png", png),
            ("jpeg", as_bytes(image::ImageFormat::Jpeg)),
            ("webp", as_bytes(image::ImageFormat::WebP)),
            ("gif", as_bytes(image::ImageFormat::Gif)),
        ] {
            let img = decode_picture(&bytes).unwrap_or_else(|e| panic!("{name}: {}", e.message));
            assert_eq!((img.width, img.height, img.rgba.len()), (16, 8, 16 * 8 * 4), "{name}");
            let left = &img.rgba[..4];
            assert!(left[0] > 150 && left[2] < 90, "{name}: the left half stays red ({left:?})");
        }
        assert!(decode_picture(b"not a picture").is_err_and(|e| e.code == ErrorCode::GarbledOutput));
    }

    #[test]
    fn png_roundtrip_and_pixel_diff() {
        let a = encode_solid(4, 4, [10, 20, 30, 255]);
        let b = encode_solid(4, 4, [10, 20, 30, 255]);
        let mut c_px = [10u8, 20, 30, 255];
        let img_a = decode_png(&a).unwrap();
        assert_eq!((img_a.width, img_a.height), (4, 4));
        let d = pixel_diff(&decode_png(&a).unwrap(), &decode_png(&b).unwrap(), 0);
        assert_eq!(d["identical"], true);
        // Change one pixel by 5 on one channel.
        c_px[1] = 25;
        let c = encode_one_changed(4, 4, [10, 20, 30, 255], 1, c_px);
        let d2 = pixel_diff(&decode_png(&a).unwrap(), &decode_png(&c).unwrap(), 0);
        assert_eq!(d2["differing_pixels"], 1);
        assert_eq!(d2["max_channel_delta"], 5);
        // Threshold of 5 hides a delta of exactly 5.
        let d3 = pixel_diff(&decode_png(&a).unwrap(), &decode_png(&c).unwrap(), 5);
        assert_eq!(d3["differing_pixels"], 0);
        // Different dimensions are not comparable.
        let big = encode_solid(5, 4, [0, 0, 0, 255]);
        assert_eq!(pixel_diff(&decode_png(&a).unwrap(), &decode_png(&big).unwrap(), 0)["comparable"], false);
        let (ia, ib) = (decode_png(&a).unwrap(), decode_png(&big).unwrap());
        let (w, h) = (ia.width.min(ib.width), ia.height.min(ib.height));
        assert_eq!(pixel_diff(&crop(&ia, w, h), &crop(&ib, w, h), 0)["comparable"], true);
        assert!((block_similarity(&ia, &ia, 16, 0) - 1.0).abs() < f64::EPSILON);
        let black = Image { width: 32, height: 32, rgba: vec![0; 32 * 32 * 4] };
        let mut half = Image { width: 32, height: 32, rgba: vec![0; 32 * 32 * 4] };
        half.rgba[..32 * 16 * 4].fill(255);
        assert!((block_similarity(&black, &half, 16, 24) - 0.5).abs() < 1e-9, "top two of four tiles differ");
    }

    fn encode_solid(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        let data: Vec<u8> = (0..(w * h)).flat_map(|_| px).collect();
        encode_rgba(w, h, &data)
    }

    fn encode_one_changed(w: u32, h: u32, base: [u8; 4], idx: usize, changed: [u8; 4]) -> Vec<u8> {
        let mut data: Vec<u8> = (0..(w * h)).flat_map(|_| base).collect();
        if let Some(slot) = data.get_mut(idx * 4..idx * 4 + 4) {
            slot.copy_from_slice(&changed);
        }
        encode_rgba(w, h, &data)
    }

    fn encode_rgba(w: u32, h: u32, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(data).unwrap();
        }
        out
    }

    #[test]
    fn parts_that_change_by_themselves_are_left_out_of_comparisons() {
        // A 128×64 page: a grey band, and a "carousel" square at (64, 16) that shows another frame
        // in the second photo of the same page.
        let page = |carousel: u8| {
            let mut rgba = Vec::new();
            for y in 0..64u32 {
                for x in 0..128u32 {
                    let v = if (64..96).contains(&x) && (16..48).contains(&y) {
                        carousel
                    } else if y < 8 {
                        90
                    } else {
                        230
                    };
                    rgba.extend([v, v, v, 255]);
                }
            }
            Image { width: 128, height: 64, rgba }
        };
        let (first, second) = (page(10), page(200));
        let mask = NoiseMask::between(&first, &second, 24);
        assert_eq!((mask.cols, mask.rows), (8, 4));
        assert!(mask.covers(70, 20) && mask.covers(50, 20), "the carousel and one square around it");
        assert!(!mask.covers(5, 5) && !mask.covers(127, 63), "the still parts aren't masked");
        assert_eq!(NoiseMask::from_png(&mask.to_png().unwrap()).unwrap(), mask, "it reads back from its PNG");
        let still = NoiseMask::between(&first, &first, 24);
        assert_eq!(still.count(), 0);
        assert_eq!(still.clone().union(&mask), mask, "a photo pair that missed the change doesn't hide it");
        // A rebuild that matches everything but shows yet another carousel frame.
        let build = page(120);
        assert!(block_similarity(&first, &build, 16, 24) < 1.0 && perceptual_similarity(&first, &build) < 1.0);
        assert_eq!(block_similarity_masked(&first, &build, 16, 24, Some(&mask)), 1.0);
        assert!((perceptual_similarity_masked(&first, &build, Some(&mask)) - 1.0).abs() < 1e-9);
        // A real difference outside the moving part still counts.
        let mut wrong = page(120);
        for px in wrong.rgba.as_chunks_mut::<4>().0.iter_mut().take(128 * 8) {
            *px = [255, 0, 0, 255];
        }
        assert!(block_similarity_masked(&first, &wrong, 16, 24, Some(&mask)) < 1.0);
        let everything = NoiseMask { tile: NOISE_TILE, cols: 8, rows: 4, bits: vec![true; 32] };
        assert_eq!(block_similarity_masked(&first, &wrong, 16, 24, Some(&everything)), 1.0, "nothing left to differ");
    }

    #[test]
    fn perceptual_similarity_sees_structure() {
        let img = |w: u32, h: u32, f: &dyn Fn(u32, u32) -> u8| {
            let mut rgba = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    let v = f(x, y);
                    rgba.extend_from_slice(&[v, v, v, 255]);
                }
            }
            Image { width: w, height: h, rgba }
        };
        let stripes = img(200, 120, &|x, _| if (x / 4) % 2 == 0 { 30 } else { 220 });
        let same = img(200, 120, &|x, _| if (x / 4) % 2 == 0 { 30 } else { 220 });
        let flat = img(200, 120, &|_, _| 125);
        let tinted = img(200, 120, &|x, _| if (x / 4) % 2 == 0 { 40 } else { 230 });
        assert!((perceptual_similarity(&stripes, &same) - 1.0).abs() < 1e-9);
        assert!(perceptual_similarity(&stripes, &flat) < 0.2, "stripes and a flat grey look different");
        assert!(perceptual_similarity(&stripes, &tinted) > 0.9, "a small colour shift keeps the structure");
        assert_eq!(perceptual_similarity(&stripes, &img(100, 120, &|_, _| 0)), 0.0, "different sizes");
        let big = img(2000, 40, &|x, _| (x % 256) as u8);
        assert!(perceptual_similarity(&big, &big) > 0.999, "shrunk before comparing");
    }

    #[test]
    fn crop_png_cuts_the_page_and_pads_white() {
        // A 6×4 image: red column 0..2, green 2..4, blue 4..6.
        let mut rgba = Vec::new();
        for _y in 0..4 {
            for x in 0..6 {
                let c = match x {
                    0..=1 => [255, 0, 0, 255],
                    2..=3 => [0, 255, 0, 255],
                    _ => [0, 0, 255, 255],
                };
                rgba.extend_from_slice(&c);
            }
        }
        let png = encode_png(&Image { width: 6, height: 4, rgba }).unwrap();
        // Cut to (2, 1, 4×4): two green, two blue columns, then two white rows (past the image).
        let cut = crop_png(&png, Clip { x: 2.0, y: 1.0, width: 4.0, height: 4.0 }).unwrap();
        let img = decode_png(&cut).unwrap();
        assert_eq!((img.width, img.height), (4, 4));
        let px = |x: usize, y: usize| img.rgba[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4].to_vec();
        assert_eq!(px(0, 0), vec![0, 255, 0, 255], "left of the cut is page column 2");
        assert_eq!(px(3, 0), vec![0, 0, 255, 255], "right of the cut is page column 5");
        assert_eq!(px(0, 3), vec![255, 255, 255, 255], "rows past the image are white");
    }

    /// A flow through the website tools on the Camoufox backend, with a stand-in helper (no
    /// browser, no network): open → capture → close, and the two honest refusals. The fake
    /// install is a `camoufox` marker beside a `python3` wrapper that runs this test binary as
    /// the helper (the work folder travels in the wrapper's environment).
    #[cfg(unix)]
    #[test]
    fn the_site_tools_run_on_the_camoufox_backend() {
        use crate::tool::{Caller, ToolOutput};
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = std::env::current_exe().unwrap();
        std::fs::write(bin.join("camoufox"), b"never run").unwrap();
        let wrapper = bin.join("python3");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nCRAFT_STANDIN_WORK=\"$3\" exec \"{}\" --ignored --exact tools::web_common::tests::camoufox_standin --nocapture\n",
                exe.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut s = crate::testing::session();
        s.settings.engines.tools.camoufox = Some(bin.join("camoufox"));
        s.settings.engines.web_browser = crate::settings::WebBrowser::Camoufox;
        s.settings.deep_mode = true; // so the page's health is reported
        let e = crate::Engine::new(s);
        let cli = || crate::CallOptions::new(Caller::Cli).with_consent(crate::Consent::Granted);

        let opened = e.call("web_site_open", json!({"url": "http://127.0.0.1:1/", "timeout_secs": 30}), cli()).unwrap();
        let ToolOutput::Evidence(opened) = opened else { panic!("web_site_open returns evidence") };
        assert_eq!(opened.environment.engine, "camoufox");
        assert_eq!(opened.environment.engine_version.as_deref(), Some("156.0.1"));
        assert_eq!(opened.result["browser"], "Camoufox");
        assert_eq!(opened.result["title"], "Stand-in");
        let failed = opened.result["health"]["failed_requests"].as_array().cloned().unwrap_or_default();
        assert!(failed.iter().any(|f| f["error"] == "net::ERR_CONNECTION_REFUSED"), "{failed:?}");

        let out = dir.path().join("capture");
        let captured = e.call("web_site_capture", json!({"output_directory": out.display().to_string(), "widths": [1440], "timeout_secs": 30}), cli()).unwrap();
        let ToolOutput::Evidence(captured) = captured else { panic!("web_site_capture returns evidence") };
        assert_eq!(captured.environment.engine, "camoufox");
        assert_eq!(&std::fs::read(out.join("screenshot-1440.png")).unwrap()[..8], b"\x89PNG\r\n\x1a\n");
        let capture: Value = serde_json::from_str(&std::fs::read_to_string(out.join("capture.json")).unwrap()).unwrap();
        assert_eq!(capture["title"], "Stand-in");
        assert!(out.join("README.md").exists());

        let closed = e.call("web_site_close", json!({}), cli()).unwrap();
        assert_eq!(closed.to_json()["result"]["closed"], true, "{}", closed.to_json());
    }

    /// Camoufox named in the settings but not installed says so, in words that say what to do
    /// (with a Chromium present, so the precheck lets the run pick the backend).
    #[test]
    fn camoufox_set_but_missing_says_how_to_fix_it() {
        use crate::tool::Caller;
        let dir = tempfile::tempdir().unwrap();
        let chromium = dir.path().join("chrome");
        std::fs::write(&chromium, b"never run").unwrap();
        let mut s = crate::testing::session();
        s.settings.engines.tools.chromium = Some(chromium);
        s.settings.engines.web_browser = crate::settings::WebBrowser::Camoufox;
        let e = crate::Engine::new(s);
        let err = e
            .call("web_site_open", json!({"url": "http://127.0.0.1:1/"}), crate::CallOptions::new(Caller::Cli).with_consent(crate::Consent::Granted))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::EngineMissing, "{}", err.message);
        assert!(err.message.contains("Camoufox"), "{}", err.message);
        assert!(err.remediation.is_some_and(|r| r.contains("web_browser")), "the way out is named");
    }

    /// With `auto` and neither browser installed, the tools say what they need before running.
    #[test]
    fn auto_without_any_browser_says_what_it_needs() {
        use crate::tool::Caller;
        let e = crate::Engine::new(crate::testing::session());
        let err = e
            .call("web_site_open", json!({"url": "http://127.0.0.1:1/"}), crate::CallOptions::new(Caller::Cli).with_consent(crate::Consent::Granted))
            .unwrap_err();
        assert!(err.message.contains("Camoufox") && err.message.contains("installed"), "{}", err.message);
    }

    /// The stand-in Camoufox helper: this test binary re-entered with `--ignored --exact` (by
    /// the `python3` wrapper the flow test writes). It answers the helper protocol with canned
    /// page data — no browser, no network. The work folder comes from the wrapper's
    /// CRAFT_STANDIN_WORK.
    #[cfg(unix)]
    #[test]
    #[ignore = "stand-in started by the flow test's wrapper"]
    fn camoufox_standin() {
        use std::io::{BufRead, Write};
        let work = std::env::var("CRAFT_STANDIN_WORK").map(std::path::PathBuf::from).unwrap_or_default();
        let stdin = std::io::stdin();
        let mut out = std::io::stdout();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { return };
            let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            let op = req.get("op").and_then(Value::as_str).unwrap_or_default();
            let reply = match op {
                "open" => json!({"id": id, "ok": {"product": "Camoufox", "version": "156.0.1", "user_agent": "UA"}}),
                "goto" => json!({"id": id, "ok": {"loaded": true, "events": [
                    {"method": "Network.requestWillBeSent", "params": {"requestId": "1", "request": {"url": "http://127.0.0.1:1/"}}},
                    {"method": "Network.loadingFailed", "params": {"requestId": "1", "errorText": "net::ERR_CONNECTION_REFUSED", "type": "Document"}}
                ]}}),
                "eval" => {
                    let script = req.get("script").and_then(Value::as_str).unwrap_or_default();
                    let value = if script.contains("readyState") {
                        json!({"url": "http://127.0.0.1:1/", "title": "Stand-in", "ready": "complete", "lang": "", "width": 800, "height": 1200, "elements": 10, "images": 1, "links": 2, "forms": 0})
                    } else if script.contains("fontFaces") {
                        json!({"url": "http://127.0.0.1:1/", "title": "Stand-in", "outline": [{"kind": "heading", "text": "Stand-in"}], "headings": [{"text": "Stand-in"}], "links": [], "forms": [], "assets": {"images": [{"src": "i.png"}]}, "design": {}})
                    } else if script.trim().starts_with("location.href") {
                        json!("http://127.0.0.1:1/")
                    } else if script.contains("outerHTML") {
                        json!("<html><body>stand-in</body></html>")
                    } else if script.contains("scrollHeight") {
                        json!(1200)
                    } else {
                        Value::Null
                    };
                    json!({"id": id, "ok": {"value": value}})
                }
                "screenshot" => {
                    let mut rgba = Vec::new();
                    for _ in 0..12 {
                        for _ in 0..16 {
                            rgba.extend_from_slice(&[30, 102, 245, 255]);
                        }
                    }
                    let png = encode_png(&Image { width: 16, height: 12, rgba }).unwrap();
                    std::fs::write(work.join("shot-1.png"), &png).unwrap();
                    json!({"id": id, "ok": {"file": "shot-1.png", "width": 16, "height": 12}})
                }
                "viewport" | "key" | "sleep" | "close" => json!({"id": id, "ok": {}}),
                other => json!({"id": id, "error": {"message": format!("unknown op {other}")}}),
            };
            let mut line = reply.to_string();
            line.push('\n');
            let _ = out.write_all(line.as_bytes());
            let _ = out.flush();
        }
    }
}

/// The last lines a launched browser wrote to stderr, for a failure's message.
#[derive(Clone, Default)]
struct StderrTail(std::sync::Arc<craft_anything_platform::sync::Mutex<std::collections::VecDeque<String>>>);

impl StderrTail {
    fn push(&self, line: &str) {
        let mut q = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if q.len() >= 20 {
            q.pop_front();
        }
        q.push_back(line.chars().take(400).collect());
    }

    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().cloned().collect()
    }
}
