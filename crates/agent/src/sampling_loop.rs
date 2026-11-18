//! Timing for the sampler thread: when to prime, when to take a sample.
//!
//! Two problems found on the first Windows runs, both fixed here:
//!
//! - **Drift.** The loop used to `sleep(interval)` and then sample, so every
//!   tick took the interval *plus* however long sampling took (process scan,
//!   PDH collection, per-process handle/thread lookups): median 1.045 s on
//!   the test machine, 39 samples in a 41 s session. Ticks are now scheduled
//!   on a fixed grid of deadlines, so sampling time comes out of the sleep
//!   instead of adding to it.
//! - **First sample after an idle gap.** Every rate the sampler reports
//!   (sysinfo CPU and per-process CPU, disk and network byte deltas, PDH
//!   rates) is "change since the previous sample". Nothing is sampled while
//!   idle (that's what keeps an idle agent at ~0% CPU), so a session's first
//!   sample used to average over the whole gap since the *previous* session.
//!   Now the sampler is primed the moment recording starts, so the first
//!   sample covers exactly its own interval.
//!
//! "Active" originally meant only "a session is recording." Phase 6 added a
//! second, independent reason to be active: the overlay polling live
//! numbers with nothing recording (see `telemetry::Telemetry::wants_samples`).
//! Either one dropping to false while the other is still true must not
//! idle the loop or re-prime it — only the *combined* condition going
//! false, then true again, is a real idle gap. That's why this takes one
//! `active` closure rather than a recording flag: the caller ORs whatever
//! conditions apply, and this loop doesn't need to know what they mean.

use std::time::{Duration, Instant};

/// While idle, how often to check whether sampling should start. Short so
/// the first sample after becoming active isn't delayed by up to a whole
/// interval. It's cheap because it's just the `active` closure, typically
/// one or two atomic loads: no sampling runs while idle.
const IDLE_POLL: Duration = Duration::from_millis(50);

pub enum Step {
    /// Just became active: reset every rate baseline to now, and discard
    /// the result.
    Prime,
    /// Take a sample. The callback returns `false` to stop the loop for good
    /// (the core thread has shut down).
    Sample,
}

/// Drives `step` until it returns `false`. Idles cheaply while `active()`
/// is false. On each start, primes once and then samples every
/// `interval()`, measured from the prime.
///
/// `interval` and `active` are both read again on every tick, so a change
/// — a `reload`'d interval, or recording/overlay-polling starting or
/// stopping — takes effect from the next tick without restarting the loop.
/// The tick already being waited for keeps its old deadline.
pub fn run(
    interval: impl Fn() -> Duration,
    active: impl Fn() -> bool,
    mut step: impl FnMut(Step) -> bool,
) {
    // A zero interval (a hand-edited config) would otherwise spin.
    let interval = || interval().max(Duration::from_millis(1));
    loop {
        while !active() {
            std::thread::sleep(IDLE_POLL);
        }
        if !step(Step::Prime) {
            return;
        }
        let mut deadline = Instant::now() + interval();
        loop {
            let now = Instant::now();
            if deadline > now {
                std::thread::sleep(deadline - now);
            }
            if !active() {
                break; // nothing needs samples anymore; idle, primed again on the next start
            }
            if !step(Step::Sample) {
                return;
            }
            deadline = next_deadline(deadline, interval(), Instant::now());
        }
    }
}

/// The next tick after `prev` on the `prev + k * interval` grid that's still
/// in the future at `now`.
///
/// When more than a whole interval was missed (a long stall, or sampling
/// itself taking longer than the interval), the missed ticks are skipped
/// rather than fired back-to-back. Catching up would record a burst of
/// samples a few ms apart, whose rates are computed over near-zero elapsed
/// time and so are mostly noise.
pub fn next_deadline(prev: Instant, interval: Duration, now: Instant) -> Instant {
    let next = prev + interval;
    if next > now || interval.is_zero() {
        return next;
    }
    let missed = (now.duration_since(prev).as_nanos() / interval.as_nanos()).min(u32::MAX as u128);
    prev + interval * (missed as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn on_time_ticks_stay_on_the_grid() {
        let t0 = Instant::now();
        // Woke 5 ms late: the next tick is still t0 + 2 intervals, so the
        // lateness doesn't carry forward into the schedule.
        assert_eq!(
            next_deadline(t0 + 100 * MS, 100 * MS, t0 + 105 * MS),
            t0 + 200 * MS
        );
    }

    #[test]
    fn missed_ticks_are_skipped_not_burst() {
        let t0 = Instant::now();
        // 350 ms behind a 100 ms grid: ticks at 200 and 300 are gone; the
        // next one is 400, not 200 followed immediately by 300.
        assert_eq!(next_deadline(t0, 100 * MS, t0 + 350 * MS), t0 + 400 * MS);
        // Exactly on a grid point counts as missed (it's not in the future).
        assert_eq!(next_deadline(t0, 100 * MS, t0 + 200 * MS), t0 + 300 * MS);
    }

    /// Regression for the 4.5% drift: with a deliberately slow sample (15 ms
    /// of work per 40 ms tick), a sleep-then-sample loop managed one tick per
    /// ~55 ms (~18 in a second). The deadline loop should hold ~25.
    #[test]
    fn slow_samples_do_not_stretch_the_interval() {
        let recording = AtomicBool::new(true);
        let interval = 40 * MS;
        let start = Instant::now();
        let mut samples = 0;
        run(
            || interval,
            || recording.load(Ordering::SeqCst),
            |step| {
                if let Step::Sample = step {
                    samples += 1;
                    std::thread::sleep(15 * MS);
                }
                start.elapsed() < Duration::from_secs(1)
            },
        );
        // 1000 / 40 = 25 ideal. Allow scheduler slack, but stay well clear
        // of the ~18 the old loop produced.
        assert!(samples >= 22, "only {samples} samples in 1 s at 40 ms");
    }

    #[test]
    fn primes_once_per_start_before_sampling() {
        let recording = Arc::new(AtomicBool::new(false));
        let flag = recording.clone();
        let starter = std::thread::spawn(move || {
            std::thread::sleep(120 * MS);
            flag.store(true, Ordering::SeqCst);
        });

        let begin = Instant::now();
        let mut steps: Vec<(&str, Duration)> = Vec::new();
        run(
            || 50 * MS,
            || recording.load(Ordering::SeqCst),
            |step| {
                steps.push((
                    match step {
                        Step::Prime => "prime",
                        Step::Sample => "sample",
                    },
                    begin.elapsed(),
                ));
                steps.len() < 4
            },
        );
        starter.join().unwrap();

        let kinds: Vec<_> = steps.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, ["prime", "sample", "sample", "sample"]);
        // Nothing happened while idle, and the prime came promptly after
        // the start (within one idle poll plus slack).
        let (_, primed_at) = steps[0];
        assert!(primed_at >= 120 * MS, "primed before recording started");
        assert!(
            primed_at < 120 * MS + IDLE_POLL + 60 * MS,
            "prime late: {primed_at:?}"
        );
        // The first sample is one interval after the prime, not after an
        // arbitrary leftover sleep.
        let first_gap = steps[1].1 - primed_at;
        assert!(
            first_gap >= 45 * MS && first_gap < 100 * MS,
            "{first_gap:?}"
        );
    }

    /// What `reload` relies on: a new interval applies from the next tick,
    /// without restarting the loop or the session.
    #[test]
    fn a_changed_interval_applies_from_the_next_tick() {
        let recording = AtomicBool::new(true);
        let interval_ms = AtomicU64::new(20);
        let mut sampled_at: Vec<Instant> = Vec::new();
        run(
            || Duration::from_millis(interval_ms.load(Ordering::Relaxed)),
            || recording.load(Ordering::SeqCst),
            |step| {
                if let Step::Sample = step {
                    sampled_at.push(Instant::now());
                    if sampled_at.len() == 3 {
                        interval_ms.store(150, Ordering::Relaxed);
                    }
                }
                sampled_at.len() < 5
            },
        );
        let gaps: Vec<Duration> = sampled_at.windows(2).map(|w| w[1] - w[0]).collect();
        // Before the change: ~20 ms apart. After it: ~150 ms.
        assert!(gaps[0] < 100 * MS && gaps[1] < 100 * MS, "{gaps:?}");
        assert!(gaps[2] >= 140 * MS && gaps[3] >= 140 * MS, "{gaps:?}");
    }

    /// `main.rs` ORs the recording flag and the overlay's telemetry lease
    /// into one `active` closure (Phase 6). Handing off between the two
    /// reasons — one drops while the other is still true — must not idle
    /// the loop or re-prime: only both being false at once is a real gap.
    #[test]
    fn handing_off_between_two_active_reasons_does_not_idle_or_reprime() {
        let recording = AtomicBool::new(true);
        let overlay = AtomicBool::new(false);
        let mut steps: Vec<&str> = Vec::new();
        run(
            || 5 * MS,
            || recording.load(Ordering::SeqCst) || overlay.load(Ordering::SeqCst),
            |step| {
                steps.push(match step {
                    Step::Prime => "prime",
                    Step::Sample => "sample",
                });
                match steps.len() {
                    // Recording is on alone, then the overlay picks up
                    // "active" just as recording lets go — never both false
                    // at once.
                    2 => {
                        overlay.store(true, Ordering::SeqCst);
                        recording.store(false, Ordering::SeqCst);
                    }
                    6 => return false,
                    _ => {}
                }
                true
            },
        );
        // Exactly one prime, at the very start; everything after the
        // handoff is still plain samples, not a second prime.
        assert_eq!(
            steps,
            ["prime", "sample", "sample", "sample", "sample", "sample"]
        );
    }
}
