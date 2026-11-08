//! Driving Camoufox (a fingerprint-resistant Firefox build) through its Playwright integration.
//!
//! Camoufox speaks Playwright, not CDP, so it gets its own helper: a small Python program
//! ([`HELPER_PY`], written to the work folder at start) that holds the browser and answers one
//! JSON request per stdin line with one response per stdout line. Everything that can grow —
//! page data, screenshots — comes back as a file in the work folder, never in a line (a line
//! that must arrive whole to parse must stay small, or it arrives in pieces).
//!
//! The helper's Python is the one beside the `camoufox` command (the environment it was
//! installed into); the browser itself Camoufox finds in its own cache.

use crate::cdp::{CdpEvent, Deadline};
use craft_anything_evidence::{ErrorCode, ToolError};
use craft_anything_platform::process::{Process, RunningProcess, StdinMode};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::ChildStdin;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

/// The engine id in the engine table and in settings (`engines.tools.camoufox`).
pub const ID: &str = "camoufox";

/// The helper, written into the work folder and run with `python -I`.
const HELPER_PY: &str = include_str!("camoufox_helper.py");

/// A JSON reply payload over this size arrives as a file in the work folder (the helper's own
/// copy of the threshold; kept equal by a test).
pub const PAYLOAD_FILE_MIN: usize = 1 << 20;

/// Biggest screenshot or page-data file read back from the work folder.
pub const MAX_RESULT_BYTES: usize = 64 << 20;

/// How long a helper process may live (each request has its own deadline).
const HELPER_LIFETIME_SECS: u64 = 7 * 24 * 3600;

/// How long a dropped helper gets to die before the caller moves on.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// The command that runs the helper: the Python of the environment that has `camoufox`.
pub struct HelperCmd {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// The helper command for a located `camoufox` command and the work folder [`Helper::start`]
/// will use: the Python beside the `camoufox` command (the environment it was installed into),
/// or the first `python3` on this computer. A `camoufox` that *is* a Python interpreter is used
/// directly.
pub fn helper_command(camoufox_exe: &Path, work_dir: &Path) -> Result<HelperCmd, ToolError> {
    let no_python = || {
        ToolError::new(ErrorCode::EngineMissing, format!("no Python was found for {}", camoufox_exe.display()))
            .with_remediation("Point Settings → Engines at the `camoufox` command of a Python that has the camoufox package (`pip install camoufox`), so the Python beside it can run the helper.")
    };
    let script = work_dir.join("camoufox_helper.py");
    let args = || vec!["-I".into(), script.display().to_string(), work_dir.display().to_string()];
    let stem = camoufox_exe.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if stem.starts_with("python") {
        return Ok(HelperCmd { program: camoufox_exe.to_path_buf(), args: args(), env: Vec::new() });
    }
    let dir = camoufox_exe.parent().unwrap_or_else(|| Path::new(""));
    for name in ["python3", "python", "python3.exe", "python.exe"] {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(HelperCmd { program: candidate, args: args(), env: Vec::new() });
        }
    }
    for name in ["python3", "python"] {
        if let Some(found) = craft_anything_platform::which::which(name) {
            return Ok(HelperCmd { program: found, args: args(), env: Vec::new() });
        }
    }
    Err(no_python())
}

/// A running helper and its work folder. Dropping it kills the helper (confirmed) and removes
/// the folder, so no profile or page data outlives the session.
pub struct Helper {
    process: RunningProcess,
    stdin: ChildStdin,
    lines: Receiver<String>,
    stderr: Arc<craft_anything_platform::sync::Mutex<VecDeque<String>>>,
    next_id: u64,
    work_dir: PathBuf,
}

impl Helper {
    /// Write the helper into `work_dir` and start the command [`helper_command`] built for it.
    /// The first request (`open`) launches the browser; failures there carry the helper's own
    /// words.
    pub fn start(cmd: &HelperCmd, work_dir: &Path, dl: &Deadline) -> Result<Helper, ToolError> {
        std::fs::create_dir_all(work_dir)
            .map_err(|e| ToolError::new(ErrorCode::FileError, format!("couldn't make the helper's work folder {}: {e}", work_dir.display())))?;
        let helper_py = work_dir.join("camoufox_helper.py");
        std::fs::write(&helper_py, HELPER_PY)
            .map_err(|e| ToolError::new(ErrorCode::FileError, format!("couldn't write the helper script into {}: {e}", work_dir.display())))?;
        let (tx, lines) = channel();
        let stderr: Arc<craft_anything_platform::sync::Mutex<VecDeque<String>>> = Arc::default();
        let sink = stderr.clone();
        let cancel = dl.cancel_token();
        let mut p = Process::new(&cmd.program)
            .args(&cmd.args)
            .stdin(StdinMode::Piped)
            .timeout(Duration::from_secs(HELPER_LIFETIME_SECS))
            .stdout_cap(16 << 20)
            .on_stdout_line(move |l| {
                let _ = tx.send(l.to_string());
            })
            .on_stderr_line(move |l| {
                if let Ok(mut tail) = sink.lock() {
                    if tail.len() >= 40 {
                        tail.pop_front();
                    }
                    tail.push_back(l.to_string());
                }
            })
            .cancel_when(move || cancel.is_cancelled())
            .label("camoufox")
            .remediation("Check the Camoufox install in Settings → Engines (`pip install camoufox`, then `camoufox fetch`).");
        for (k, v) in &cmd.env {
            p = p.env(k, v);
        }
        let mut process = p.spawn().map_err(|e| {
            ToolError::new(ErrorCode::EngineMissing, format!("couldn't start the Camoufox helper with {}: {e}", cmd.program.display()))
                .with_remediation("Point Settings → Engines at the `camoufox` command of a Python that has the camoufox package, and check that Python starts.")
        })?;
        let stdin = process.take_stdin().ok_or_else(|| ToolError::internal("the Camoufox helper has no input pipe"))?;
        Ok(Helper { process, stdin, lines, stderr, next_id: 0, work_dir: work_dir.to_path_buf() })
    }

    /// Is the helper still running?
    pub fn alive(&mut self) -> bool {
        !self.process.is_finished()
    }

    fn exited(&self) -> ToolError {
        let said = self.stderr.lock().map(|t| t.iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default()).unwrap_or_default();
        let said = if said.is_empty() { String::new() } else { format!(" It said: {}", said.trim()) };
        ToolError::new(ErrorCode::ProgramFailed, format!("The Camoufox helper stopped.{said}"))
            .with_remediation("Check the Camoufox install in Settings → Engines (`pip install camoufox`, then `camoufox fetch`); the log says what it printed.")
    }

    fn write_request(&mut self, id: u64, op: &str, args: Value) -> Result<(), ToolError> {
        let mut msg = json!({"id": id, "op": op});
        if let (Some(dst), Some(src)) = (msg.as_object_mut(), args.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        let mut line = msg.to_string();
        line.push('\n');
        use std::io::Write;
        self.stdin.write_all(line.as_bytes()).and_then(|()| self.stdin.flush()).map_err(|_| self.exited())
    }

    /// One request/response round trip. A reply payload over the line threshold arrives as a
    /// file and is read (and deleted) here, so callers always see the payload itself.
    pub fn request(&mut self, op: &str, args: Value, dl: &Deadline) -> Result<Value, ToolError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.write_request(id, op, args)?;
        loop {
            if dl.expired() {
                return Err(ToolError::new(ErrorCode::TimedOut, format!("the Camoufox helper did not answer {op} in time."))
                    .with_remediation("Raise the timeout, or check the browser is responsive."));
            }
            let left = Duration::from_millis(200);
            let line = match self.lines.recv_timeout(left) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) if self.process.is_finished() => return Err(self.exited()),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(self.exited()),
            };
            let msg: Value = match serde_json::from_str(line.trim()) {
                Ok(m) => m,
                // Servers may log to stdout; only our answer matters.
                Err(_) => continue,
            };
            if msg.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                let text = err.get("message").and_then(Value::as_str).unwrap_or("unknown error");
                return Err(ToolError::new(ErrorCode::ProgramFailed, text.to_string())
                    .with_remediation("Check the page and the Camoufox install in Settings → Engines; the helper's log says what it printed."));
            }
            let ok = msg.get("ok").cloned().unwrap_or(Value::Null);
            if let Some(name) = ok.get("_file").and_then(Value::as_str) {
                let bytes = self.read_work_file(name)?;
                return serde_json::from_slice(&bytes).map_err(|e| ToolError::new(ErrorCode::GarbledOutput, format!("the helper's page data isn't JSON: {e}")));
            }
            return Ok(ok);
        }
    }

    /// Read (and delete) a file the helper wrote into the work folder. Names must be plain file
    /// names: anything with a path separator is refused.
    fn read_work_file(&self, name: &str) -> Result<Vec<u8>, ToolError> {
        if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err(ToolError::new(ErrorCode::GarbledOutput, format!("the helper named a bad file: {name:?}")));
        }
        let path = self.work_dir.join(name);
        let meta = std::fs::metadata(&path).map_err(|e| ToolError::new(ErrorCode::GarbledOutput, format!("the helper's file {name:?} is missing: {e}")))?;
        if meta.len() > MAX_RESULT_BYTES as u64 {
            let _ = std::fs::remove_file(&path);
            return Err(ToolError::new(ErrorCode::GarbledOutput, format!("the helper's file {name:?} is over the {} MiB limit.", MAX_RESULT_BYTES >> 20))
                .with_remediation("Capture fewer widths or a smaller page at a time."));
        }
        let bytes = std::fs::read(&path).map_err(|e| ToolError::new(ErrorCode::GarbledOutput, format!("couldn't read the helper's file {name:?}: {e}")))?;
        let _ = std::fs::remove_file(&path);
        Ok(bytes)
    }

    // ---- typed ops -------------------------------------------------------------------------

    /// Launch Camoufox and open its first page; returns product, version and user agent.
    pub fn open(&mut self, headless: bool, width: u32, height: u32, os: &str, dl: &Deadline) -> Result<Value, ToolError> {
        self.request("open", json!({"headless": headless, "width": width, "height": height, "os": os}), dl)
    }

    /// Load a page and collect what went wrong, as the browser's events.
    pub fn goto(&mut self, url: &str, settle_ms: u64, dl: &Deadline) -> Result<(bool, Vec<CdpEvent>), ToolError> {
        let r = self.request("goto", json!({"url": url, "settle_ms": settle_ms}), dl)?;
        let loaded = r.get("loaded").and_then(Value::as_bool).unwrap_or(false);
        let mut events = Vec::new();
        for e in r.get("events").and_then(Value::as_array).into_iter().flatten() {
            events.push(CdpEvent {
                method: e.get("method").and_then(Value::as_str).unwrap_or_default().to_string(),
                params: e.get("params").cloned().unwrap_or(Value::Null),
                session_id: None,
            });
        }
        Ok((loaded, events))
    }

    /// Run a read-only script in the page and return its JSON value.
    pub fn eval(&mut self, script: &str, dl: &Deadline) -> Result<Value, ToolError> {
        let r = self.request("eval", json!({"script": script}), dl)?;
        Ok(r.get("value").cloned().unwrap_or(Value::Null))
    }

    /// A PNG of the whole page (`full_page`) or of the current viewport.
    pub fn screenshot(&mut self, full_page: bool, dl: &Deadline) -> Result<Vec<u8>, ToolError> {
        let mode = if full_page { "full_page" } else { "viewport" };
        let r = self.request("screenshot", json!({"mode": mode}), dl)?;
        let name = r.get("file").and_then(Value::as_str).unwrap_or_default();
        self.read_work_file(name)
    }

    /// Size the page and pin light mode, so captures don't depend on this computer's theme.
    pub fn viewport(&mut self, width: u32, height: u32, dl: &Deadline) -> Result<(), ToolError> {
        self.request("viewport", json!({"width": width, "height": height}), dl).map(|_| ())
    }

    pub fn key(&mut self, key: &str, dl: &Deadline) -> Result<(), ToolError> {
        self.request("key", json!({"key": key}), dl).map(|_| ())
    }

    /// Let late content settle (and the helper's event handlers run).
    pub fn sleep(&mut self, ms: u64, dl: &Deadline) -> Result<(), ToolError> {
        self.request("sleep", json!({"ms": ms}), dl).map(|_| ())
    }

    /// Close the browser. Best effort: the process is killed when this drops anyway.
    pub fn close(&mut self) {
        let dl = Deadline::new(Duration::from_secs(5), craft_anything_platform::cancel::CancelToken::default());
        let _ = self.request("close", json!({}), &dl);
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        // Confirmed, not just asked: the session may end (and the process exit) right after.
        self.process.kill_and_wait(KILL_WAIT);
        if let Err(e) = std::fs::remove_dir_all(&self.work_dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("couldn't remove the Camoufox work folder {}: {e}", self.work_dir.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_anything_platform::cancel::CancelToken;

    fn dl(secs: u64) -> Deadline {
        Deadline::new(Duration::from_secs(secs), CancelToken::default())
    }

    /// The stand-in's command: this test binary re-entered as the ignored stand-in test below.
    /// The behavior and the work folder travel as environment variables (extra argv would be
    /// read as test-name filters by the harness).
    fn stand_in(behavior: &str, work: &Path) -> HelperCmd {
        let exe = std::env::current_exe().unwrap();
        HelperCmd {
            program: exe,
            args: vec!["--ignored".into(), "--exact".into(), "camoufox::tests::stand_in_helper".into()],
            env: vec![("CRAFT_STANDIN_BEHAVIOR".into(), behavior.into()), ("CRAFT_STANDIN_WORK".into(), work.display().to_string())],
        }
    }

    fn work_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("work");
        (dir, path)
    }

    #[test]
    fn round_trip_payloads_and_events() {
        let (_keep, work) = work_dir();
        let cmd = stand_in("normal", &work);
        let d = dl(30);
        let mut h = Helper::start(&cmd, &work, &d).unwrap();
        let info = h.open(true, 800, 600, "macos", &d).unwrap();
        assert_eq!(info["product"], "Camoufox");
        let (loaded, events) = h.goto("http://127.0.0.1:1/", 0, &d).unwrap();
        assert!(loaded);
        assert!(events.iter().any(|e| e.method == "Network.requestWillBeSent"), "{events:?}");
        // A small value arrives in the line; a big one through a file.
        assert_eq!(h.eval("small", &d).unwrap(), json!({"small": true}));
        let big = h.eval("big", &d).unwrap();
        assert_eq!(big["pad"].as_str().map(|s| s.len()), Some(2 << 20), "the file payload is the whole value");
        // A screenshot comes back as bytes from the work folder.
        let png = h.screenshot(true, &d).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(!work.join("shot-1.png").exists(), "read files are deleted");
        h.viewport(390, 800, &d).unwrap();
        h.key("Escape", &d).unwrap();
        h.sleep(1, &d).unwrap();
        h.close();
    }

    #[test]
    fn a_helper_error_is_a_tool_error() {
        let (_keep, work) = work_dir();
        let cmd = stand_in("normal", &work);
        let d = dl(30);
        let mut h = Helper::start(&cmd, &work, &d).unwrap();
        let err = h.eval("boom", &d).unwrap_err();
        assert_eq!(err.code, ErrorCode::ProgramFailed);
        assert!(err.message.contains("stand-in says no"), "{}", err.message);
    }

    #[test]
    fn a_helper_that_exits_is_reported_not_hung() {
        let (_keep, work) = work_dir();
        let cmd = stand_in("exits", &work);
        let d = dl(30);
        let mut h = Helper::start(&cmd, &work, &d).unwrap();
        let err = h.eval("small", &d).unwrap_err();
        assert_eq!(err.code, ErrorCode::ProgramFailed, "{}", err.message);
        assert!(err.message.contains("stopped"), "{}", err.message);
    }

    #[test]
    fn a_helper_that_never_answers_times_out() {
        let (_keep, work) = work_dir();
        let cmd = stand_in("hangs", &work);
        let d = dl(30);
        let mut h = Helper::start(&cmd, &work, &d).unwrap();
        let short = Deadline::new(Duration::from_millis(500), CancelToken::default());
        let err = h.eval("small", &short).unwrap_err();
        assert_eq!(err.code, ErrorCode::TimedOut, "{}", err.message);
    }

    #[test]
    fn a_file_name_with_a_path_is_refused() {
        let (_keep, work) = work_dir();
        let cmd = stand_in("normal", &work);
        let d = dl(30);
        let h = Helper::start(&cmd, &work, &d).unwrap();
        for name in ["../escape.json", "a/b.json", "a\\b.json", ""] {
            assert!(h.read_work_file(name).is_err(), "{name:?} must be refused");
        }
    }

    #[test]
    fn payload_thresholds_agree_with_the_helper() {
        let line = HELPER_PY.lines().find(|l| l.starts_with("PAYLOAD_FILE_MIN")).unwrap_or_default();
        assert!(line.contains("1 << 20"), "helper says {line:?}");
        assert_eq!(PAYLOAD_FILE_MIN, 1 << 20);
    }

    /// The stand-in helper: a second entry point of this test binary that speaks the protocol
    /// on stdio (started by the tests above with `--ignored --exact`). CRAFT_STANDIN_BEHAVIOR
    /// `exits` quits at once; `hangs` reads but never answers. CRAFT_STANDIN_WORK is where
    /// file payloads go (the real helper gets the work folder as its argument).
    #[test]
    #[ignore = "stand-in started by the tests above"]
    fn stand_in_helper() {
        use std::io::{BufRead, Write};
        let behavior = std::env::var("CRAFT_STANDIN_BEHAVIOR").unwrap_or_default();
        if behavior == "exits" {
            return;
        }
        let work = std::env::var("CRAFT_STANDIN_WORK").map(PathBuf::from).unwrap_or_default();
        let stdin = std::io::stdin();
        let mut out = std::io::stdout();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { return };
            let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            if behavior == "hangs" {
                continue;
            }
            let op = req.get("op").and_then(Value::as_str).unwrap_or_default();
            let reply = match op {
                "open" => json!({"id": id, "ok": {"product": "Camoufox", "version": "156.0.1", "user_agent": "UA"}}),
                "goto" => json!({"id": id, "ok": {"loaded": true, "events": [
                    {"method": "Network.requestWillBeSent", "params": {"requestId": "1", "request": {"url": "http://127.0.0.1:1/"}}},
                    {"method": "Network.loadingFailed", "params": {"requestId": "1", "errorText": "net::ERR_CONNECTION_REFUSED", "type": "Document"}}
                ]}}),
                "eval" if req.get("script") == Some(&json!("boom")) => {
                    json!({"id": id, "error": {"message": "stand-in says no"}})
                }
                "eval" if req.get("script") == Some(&json!("big")) => {
                    let payload = json!({"value": {"pad": "x".repeat(2 << 20)}}).to_string();
                    std::fs::write(work.join("resp-big.json"), payload).unwrap();
                    json!({"id": id, "ok": {"_file": "resp-big.json"}})
                }
                "eval" => json!({"id": id, "ok": {"value": {"small": true}}}),
                "screenshot" => {
                    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
                    png.extend_from_slice(&[0u8; 24]);
                    std::fs::write(work.join("shot-1.png"), &png).unwrap();
                    json!({"id": id, "ok": {"file": "shot-1.png", "width": 800, "height": 600}})
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
