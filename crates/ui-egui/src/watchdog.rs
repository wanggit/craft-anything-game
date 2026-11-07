//! A watchdog that keeps the window drawing when the UI's own repaint requests go missing.
//!
//! 2026-11-07: the window stopped drawing for 48 minutes while a build worked behind it (the
//! plan card's clock froze at 26m 48s). Nothing crashed and no lock was held: the frame loop's
//! "draw again" requests — every spinner, every clock tick, every build event — stopped being
//! honoured somewhere between egui's request and the frame, and no error was written anywhere.
//! Two things came out of that: this watchdog, and `RUST_LOG=trace` in the start script so the
//! next freeze has the frame scheduler's own traces to read.
//!
//! The watchdog is one thread: once a second it compares when a frame last ran with when a
//! ticking widget last asked for one. A spinner or clock on screen (`[`want_tick`]`) means the
//! UI expects frames to keep coming; if they stopped for [`QUIET`] anyway, the watchdog asks for
//! one itself. If frames stay gone it says so in the log — that entry is the report to read when
//! a freeze happens again.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Once, OnceLock};
use std::time::Duration;

/// No frame has been drawn yet (the window is still opening).
const NEVER: u64 = u64::MAX;

/// How long the UI may go without a frame while something is ticking before the watchdog acts.
const QUIET: Duration = Duration::from_secs(2);

/// How long frames may stay gone while something is ticking before the watchdog says so.
const ALARM: Duration = Duration::from_secs(30);

/// How often the watchdog says so, at most.
const ALARM_EVERY: Duration = Duration::from_secs(60);

/// A tick asked for up to this long after the last frame still counts as "in" that frame (the
/// ask happens while the frame is being built, and the clock is stamped at its start).
const GRACE: Duration = Duration::from_millis(500);

static LAST_FRAME_MS: AtomicU64 = AtomicU64::new(NEVER);
static LAST_WANT_MS: AtomicU64 = AtomicU64::new(NEVER);
static LAST_ALARM_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis().min(u64::MAX as u128) as u64
}

/// A frame is being built. Called once per pass from the app's `ui`.
pub fn note_frame() {
    LAST_FRAME_MS.store(now_ms(), Ordering::Relaxed);
}

/// A ticking widget (a spinner, a clock) is on screen and expects frames to keep coming.
pub fn want_tick() {
    LAST_WANT_MS.store(now_ms(), Ordering::Relaxed);
}

/// Whether the watchdog should ask for a frame: the UI went quiet for [`QUIET`] while a ticking
/// widget was on screen at the last frame. `gap` is how long ago that frame was, `wanted` how
/// long ago the last tick was asked for (`None`: nothing ticking). The ask happens inside the
/// frame, so a ticking UI has `wanted ≈ gap`; anything much older means the UI went idle, which
/// is a normal reason to draw nothing and must be left alone.
fn needs_a_frame(gap: Duration, wanted: Option<Duration>) -> bool {
    gap >= QUIET && wanted.is_some_and(|w| w <= gap + GRACE)
}

/// Start the watchdog thread (once per process). It only reads the two stamps and calls
/// [`egui::Context::request_repaint`]; it takes no locks, so it can never be the stuck one.
pub fn start(ctx: egui::Context) {
    static ONCE: Once = Once::new();
    ONCE.call_once(move || {
        let spawned = std::thread::Builder::new().name("ui-watchdog".into()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let now = now_ms();
                let last_frame = LAST_FRAME_MS.load(Ordering::Relaxed);
                if last_frame == NEVER {
                    continue; // no frame yet: the window is still opening
                }
                let gap = Duration::from_millis(now.saturating_sub(last_frame));
                let wanted = match LAST_WANT_MS.load(Ordering::Relaxed) {
                    NEVER => None,
                    at => Some(Duration::from_millis(now.saturating_sub(at))),
                };
                if !needs_a_frame(gap, wanted) {
                    continue;
                }
                ctx.request_repaint();
                let alarmed = LAST_ALARM_MS.load(Ordering::Relaxed);
                if gap >= ALARM && now.saturating_sub(alarmed) >= ALARM_EVERY.as_millis() as u64 {
                    LAST_ALARM_MS.store(now, Ordering::Relaxed);
                    log::error!(
                        "the window hasn't drawn a frame for {}s although a spinner or clock is on screen; \
                     the frame loop is stuck. The build behind it keeps working. Run with RUST_LOG=trace \
                     to see the frame scheduler's own traces in desktop.log.",
                        gap.as_secs()
                    );
                }
            }
        });
        if spawned.is_err() {
            log::error!("the UI watchdog thread couldn't start; a frozen window will stay frozen");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case the watchdog exists for: a spinner or clock was on screen, then frames stopped.
    #[test]
    fn a_frozen_animation_gets_a_frame_request() {
        assert!(needs_a_frame(QUIET, Some(QUIET)));
        assert!(needs_a_frame(ALARM, Some(ALARM)));
    }

    /// A window that draws nothing because nothing is happening is normal: leave it alone.
    #[test]
    fn an_idle_window_is_left_alone() {
        assert!(!needs_a_frame(ALARM, Some(ALARM + Duration::from_secs(10))));
        assert!(!needs_a_frame(QUIET, None));
    }

    /// A window drawing normally is left alone.
    #[test]
    fn a_drawing_window_is_left_alone() {
        assert!(!needs_a_frame(Duration::from_millis(50), Some(Duration::from_millis(40))));
        assert!(!needs_a_frame(Duration::from_millis(1999), Some(Duration::from_millis(1999))));
    }

    /// The stamps move; before the first frame nothing is watched.
    #[test]
    fn stamps_record_frames_and_ticks() {
        note_frame();
        want_tick();
        assert_ne!(LAST_FRAME_MS.load(Ordering::Relaxed), NEVER);
        assert_ne!(LAST_WANT_MS.load(Ordering::Relaxed), NEVER);
    }
}
