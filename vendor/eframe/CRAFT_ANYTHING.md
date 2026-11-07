# eframe 0.36.2, patched for Craft Anything

This is a copy of `eframe` 0.36.2 (MIT OR Apache-2.0; the crate package ships the `license` field in
`Cargo.toml` but no license texts) with one change, used through `[patch.crates-io]` in the workspace
`Cargo.toml`.

## The change

`src/native/run.rs`, in `user_event`: a `UserEvent::RequestRepaint` whose pass number is more than
one pass behind is no longer dropped. eframe treats such a request as stale ("we've already
repainted") and returns `EventResult::Wait` for it. The patched code schedules it anyway
(`RepaintAt`), which costs at most one extra frame — the deadline map coalesces overlapping
requests.

## Why

Every "draw again" request reaches the event loop through this queue: the spinner and clock
timers, the build's event wake-ups, anything calling `request_repaint_after`. The staleness check
allows for one pass to have run since the request was made; if more than one ran — a resize burst,
a synchronous repaint, two input-driven frames before the queue was drained — the request is
silently discarded. When the discarded one was the last request in flight, nothing reschedules,
the run loop goes to sleep, and the window stops drawing permanently: no spinner, no clock, no
content, while the build behind it keeps working. Seen 2026-11-07 on macOS: the window froze for
48 minutes (the plan card's clock stuck at 26m 48s), no panic and no error anywhere.

The app also runs a frame watchdog (`crates/ui-egui/src/watchdog.rs`) that forces a frame when a
ticking UI stops getting them and writes an error to `desktop.log` if frames stay gone, and the
start script runs with `RUST_LOG=trace` so this queue's own `log::trace!` lines (including
"Got outdated UserEvent::RequestRepaint") land in the log for the next freeze.

## Removing it

Delete this directory and the `[patch.crates-io]` entry once a released eframe keeps a repaint
request rescheduling (or diagnoses a dropped one) — check upstream egui/eframe for a fix to the
stale `RequestRepaint` drop first. Re-run the app with `RUST_LOG=trace` and confirm the trace no
longer shows requests disappearing while the window is frozen before removing the patch.
