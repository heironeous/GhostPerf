use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::CoreMsg;
use crate::now_millis;

/// While idle, nothing is being measured (see the doc comment below), so
/// there's no need to wake up at `check_interval`'s precise cadence (15ms
/// by default — ~66 times/sec) just to check whether recording has
/// started. Polling this much more coarsely while idle was worth doing:
/// found during Phase 5's resource self-check, it was most of this
/// thread's idle CPU cost (see TODO.md).
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Runs on a dedicated thread that does nothing but sleep for
/// `check_interval` and measure how late it actually woke up. A busy or
/// starved scheduler makes `thread::sleep` return later than requested;
/// anything beyond `threshold` is recorded as a stall event. Only emits
/// while a session is recording — a stall with nothing to attribute it to
/// isn't useful to store.
///
/// `timing` returns `(check_interval, threshold)` and is read again on every
/// wakeup, so a `reload` that changes either applies without restarting
/// this thread.
pub fn run(
    core_tx: Sender<CoreMsg>,
    recording_flag: Arc<AtomicBool>,
    timing: impl Fn() -> (Duration, Duration),
) {
    let mut last_wake = Instant::now();
    loop {
        let (check_interval, threshold) = timing();
        // Read once before sleeping (to pick the sleep duration) and once
        // after (to see whether recording changed while we were asleep).
        // Only a sleep that was `recording` on both sides is a meaningful
        // `check_interval`-paced tick; if either side says otherwise —
        // idle throughout, or recording flipped either direction mid-sleep
        // — `last_wake` isn't a valid baseline for it (e.g. "just started
        // after an idle gap" would otherwise measure the whole gap as one
        // giant stall, the same bug `Sampler::prime` exists to avoid for
        // sample rates). Resync and wait for the next full interval.
        let recording_before = recording_flag.load(Ordering::SeqCst);
        std::thread::sleep(if recording_before {
            check_interval
        } else {
            IDLE_POLL_INTERVAL
        });
        let now = Instant::now();
        let recording_after = recording_flag.load(Ordering::SeqCst);

        if !recording_before || !recording_after {
            last_wake = now;
            continue;
        }

        let actual = now.duration_since(last_wake);
        last_wake = now;

        let overshoot = actual.saturating_sub(check_interval);
        if overshoot >= threshold {
            let event = CoreMsg::Event {
                ts: now_millis(),
                kind: "stall".to_string(),
                value: overshoot.as_secs_f64() * 1000.0,
                detail: None,
            };
            if core_tx.send(event).is_err() {
                break; // core thread shut down
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn flags_a_sleep_that_overshoots_the_threshold() {
        let (tx, rx) = channel();
        let flag = Arc::new(AtomicBool::new(true));
        let flag_clone = flag.clone();

        // A zero threshold means any measured wakeup (even dead on time)
        // qualifies, so this doesn't depend on actually observing scheduler
        // jitter within the test's time budget.
        let handle = std::thread::spawn(move || {
            run(tx, flag_clone, || {
                (Duration::from_millis(5), Duration::ZERO)
            });
        });

        let event = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("expected a stall event");
        match event {
            CoreMsg::Event { kind, value, .. } => {
                assert_eq!(kind, "stall");
                assert!(value >= 0.0);
            }
            _ => panic!("unexpected message"),
        }

        flag.store(false, Ordering::SeqCst);
        drop(handle); // detach; test process exit cleans up the sleeping thread
    }

    #[test]
    fn stays_silent_while_not_recording() {
        let (tx, rx) = channel();
        let flag = Arc::new(AtomicBool::new(false));

        std::thread::spawn(move || {
            run(tx, flag, || {
                (Duration::from_millis(1), Duration::from_millis(50))
            });
        });

        // Longer than IDLE_POLL_INTERVAL, so this actually exercises an
        // idle wakeup-and-skip, not just "the thread hadn't done anything
        // yet" — the two look the same from here (no event either way),
        // but only one tests what this is meant to test.
        assert!(rx
            .recv_timeout(IDLE_POLL_INTERVAL + Duration::from_millis(150))
            .is_err());
    }

    #[test]
    fn starting_to_record_after_an_idle_gap_is_not_itself_a_stall() {
        let (tx, rx) = channel();
        let flag = Arc::new(AtomicBool::new(false));
        let flag_clone = flag.clone();

        // A threshold well under IDLE_POLL_INTERVAL: if the idle-to-
        // recording transition didn't reset the baseline, the first
        // measurement after flipping the flag would overshoot by close to
        // a full IDLE_POLL_INTERVAL and wrongly fire as a stall.
        std::thread::spawn(move || {
            run(tx, flag_clone, || {
                (Duration::from_millis(5), Duration::from_millis(20))
            });
        });

        // Let it settle into idle polling, then start "recording".
        std::thread::sleep(IDLE_POLL_INTERVAL * 2);
        flag.store(true, Ordering::SeqCst);

        // No stall for a while after the flip, even though the idle gap
        // just before it was much longer than the stall threshold.
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }
}
