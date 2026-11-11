//! The deadlock report must not deadlock. The tracker's registry is a plain lock with no
//! re-entry, and a log sink takes locks of its own (the app's file sink takes a tracked one),
//! so the tracker logging its report while holding the registry re-entered itself and froze the
//! whole app the moment it broke its first real deadlock (2026-11-11: the UI froze at the
//! second of the report and every thread that logged piled up behind it). This is its own test
//! program: a logger is process-wide state.

use craft_anything_platform::sync::Mutex;
use std::sync::mpsc::channel;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

/// A log sink shaped like the app's file sink: `log` takes a tracked lock, so anything that logs
/// while holding the tracker's registry re-enters the tracker — exactly what froze the app.
struct LockingSink(Arc<Mutex<Vec<String>>>);

impl log::Log for LockingSink {
    fn enabled(&self, _meta: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        if let Ok(mut lines) = self.0.lock() {
            lines.push(record.args().to_string());
        }
    }
    fn flush(&self) {}
}

#[test]
fn a_broken_deadlock_is_reported_and_logged_without_freezing() {
    let logged = Arc::new(Mutex::new(Vec::new()));
    let logger: &'static LockingSink = Box::leak(Box::new(LockingSink(Arc::clone(&logged))));
    log::set_logger(logger).expect("this test program owns the logger");
    log::set_max_level(log::LevelFilter::Trace);

    let m = Arc::new(Mutex::new(1));
    let m2 = Arc::clone(&m);
    let (done, finished) = channel();
    std::thread::spawn(move || {
        // Exactly the shape that found the bug: both guards live until the statement ends, so
        // the second lock waits on a lock this thread holds. The tracker breaks that deadlock —
        // and its report must not freeze anything.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| *m2.lock().unwrap() + *m2.lock().unwrap()));
        let _ = done.send(r.map_err(|p| p.downcast_ref::<String>().cloned().unwrap_or_default()));
    });
    let why = finished
        .recv_timeout(Duration::from_secs(20))
        .expect("the deadlock report froze the test (the tracker logged under its own lock)")
        .expect_err("the double lock must be broken, not passed");
    assert!(why.contains("deadlock"), "{why}");
    // The report reached the log sink, which means it was written after the registry was free.
    let lines = logged.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(lines.iter().any(|l| l.contains("deadlock")), "{lines:?}");
    // The lock is free again (poisoned by the unwind) and still usable.
    assert_eq!(*m.lock().unwrap_or_else(PoisonError::into_inner), 1);
}
