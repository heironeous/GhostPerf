use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::CoreMsg;
use crate::now_millis;

/// Runs on a dedicated thread that does nothing but sleep for
/// `check_interval` and measure how late it actually woke up. A busy or
/// starved scheduler makes `thread::sleep` return later than requested;
/// anything beyond `threshold` is recorded as a stall event. Only emits
/// while a session is recording — a stall with nothing to attribute it to
/// isn't useful to store.
pub fn run(
    core_tx: Sender<CoreMsg>,
    recording_flag: Arc<AtomicBool>,
    check_interval: Duration,
    threshold: Duration,
) {
    let mut last_wake = Instant::now();
    loop {
        std::thread::sleep(check_interval);
        let now = Instant::now();
        let actual = now.duration_since(last_wake);
        last_wake = now;

        if !recording_flag.load(Ordering::SeqCst) {
            continue;
        }

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
            run(tx, flag_clone, Duration::from_millis(5), Duration::ZERO);
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
            run(
                tx,
                flag,
                Duration::from_millis(1),
                Duration::from_millis(50),
            );
        });

        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }
}
