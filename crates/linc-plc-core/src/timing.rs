//! Scan timing: an anchored schedule and the three numbers a ladder is judged on.
//!
//! The schedule is anchored once: each scan's expected start is the previous
//! expected start plus the interval, never "now" plus the interval. A scan that
//! wakes late therefore shifts its own lateness into the next wake-up instead of
//! quietly resetting the clock, so a run that is consistently late keeps
//! reporting it rather than looking healthy.
//!
//! All of this uses `std::time::Instant`, which is `CLOCK_MONOTONIC` on Linux.
//! Do not switch it to `CLOCK_MONOTONIC_RAW`: that clock is not slewed by NTP, so
//! a corrected system clock would show up here as phantom lateness and false
//! overruns.
//!
//! Three durations are kept, each with its current value, minimum, maximum and an
//! exponentially weighted average over roughly [`EWMA_WINDOW`]:
//!
//! - **scan time** — how long the body (inputs, ops, outputs) took;
//! - **cycle time** — wake-up to wake-up, the period as it actually happened;
//! - **latency** — how far past its expected start a wake-up landed.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The average forgets this slowly; the weight per scan is `interval / window`.
pub const EWMA_WINDOW: Duration = Duration::from_secs(2);

/// Waking a hair late is the clock, not the machine. Anything past this is
/// reported as lateness.
pub const LATE_TOLERANCE: Duration = Duration::from_micros(100);

/// One measured duration: current, bounds, and the weighted average.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metric {
    pub now_us: u32,
    pub min_us: u32,
    pub max_us: u32,
    /// Exponentially weighted mean over [`EWMA_WINDOW`].
    pub avg_us: u32,
}

impl Metric {
    fn observe(&mut self, value: Duration, weight: f32) {
        let value_us = u32::try_from(value.as_micros()).unwrap_or(u32::MAX);
        self.now_us = value_us;
        self.min_us = if self.min_us == 0 {
            value_us
        } else {
            self.min_us.min(value_us)
        };
        self.max_us = self.max_us.max(value_us);
        let previous = self.avg_us as f32;
        let blended = previous + weight * (value_us as f32 - previous);
        self.avg_us = blended.max(0.0) as u32;
    }
}

/// The timing a snapshot carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimingSnapshot {
    pub scan: Metric,
    pub cycle: Metric,
    pub latency: Metric,
    pub overruns: u64,
}

/// The anchored schedule and the metrics it feeds.
#[derive(Debug, Clone)]
pub struct ScanTiming {
    interval: Duration,
    expected: Instant,
    last_wake: Option<Instant>,
    weight: f32,
    scan: Metric,
    cycle: Metric,
    latency: Metric,
    overruns: u64,
}

impl ScanTiming {
    /// Anchor the first expected start at `start`; the daemon passes "now".
    pub fn new(interval: Duration, start: Instant) -> Self {
        let interval = interval.max(Duration::from_micros(1));
        let weight = (interval.as_secs_f32() / EWMA_WINDOW.as_secs_f32()).clamp(0.001, 1.0);
        Self {
            interval,
            expected: start,
            last_wake: None,
            weight,
            scan: Metric::default(),
            cycle: Metric::default(),
            latency: Metric::default(),
            overruns: 0,
        }
    }

    /// Record a wake-up: its lateness against the schedule and the cycle since
    /// the previous wake. A wake past [`LATE_TOLERANCE`] counts as an overrun.
    pub fn wake(&mut self, now: Instant) {
        let lateness = now.saturating_duration_since(self.expected);
        self.latency.observe(lateness, self.weight);
        if lateness > LATE_TOLERANCE {
            self.overruns = self.overruns.saturating_add(1);
        }
        if let Some(previous) = self.last_wake {
            self.cycle
                .observe(now.saturating_duration_since(previous), self.weight);
        }
        self.last_wake = Some(now);
    }

    /// Record the end of the body, which is the scan time.
    pub fn finish(&mut self, now: Instant) {
        if let Some(wake) = self.last_wake {
            self.scan
                .observe(now.saturating_duration_since(wake), self.weight);
        }
    }

    /// Move the schedule one interval forward. This is the only place the
    /// expectation changes, so lateness never re-anchors the clock.
    pub fn schedule_next(&mut self) {
        self.expected += self.interval;
    }

    /// When the next scan is due. A negative offset means it is already late.
    pub fn expected(&self) -> Instant {
        self.expected
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn snapshot(&self) -> TimingSnapshot {
        TimingSnapshot {
            scan: self.scan,
            cycle: self.cycle,
            latency: self.latency,
            overruns: self.overruns,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_late_wake_counts_even_when_the_body_is_fast() {
        let start = base();
        let mut timing = ScanTiming::new(Duration::from_millis(1), start);
        // Woken 5 ms late, body finished 100 us later.
        timing.wake(start + Duration::from_micros(5_100));
        timing.finish(start + Duration::from_micros(5_200));
        let snapshot = timing.snapshot();
        assert_eq!(snapshot.overruns, 1, "the wake was past its expected start");
        assert_eq!(snapshot.scan.now_us, 100, "the body itself was fast");
        assert_eq!(snapshot.latency.now_us, 5_100);
    }

    #[test]
    fn a_long_body_shows_up_as_scan_time_and_makes_the_next_wake_late() {
        let start = base();
        let interval = Duration::from_millis(1);
        let mut timing = ScanTiming::new(interval, start);
        timing.wake(start);
        timing.finish(start + Duration::from_millis(3));
        let snapshot = timing.snapshot();
        assert_eq!(snapshot.scan.now_us, 3_000, "the body took 3 ms");
        assert_eq!(
            snapshot.overruns, 0,
            "a long body is not itself a late wake"
        );

        // The schedule still points at start + 1 ms, so the next wake is 2 ms late.
        timing.schedule_next();
        timing.wake(start + Duration::from_millis(3));
        assert_eq!(timing.snapshot().overruns, 1, "the schedule did not move");
    }

    #[test]
    fn lateness_never_re_anchors_the_schedule() {
        let start = base();
        let interval = Duration::from_millis(1);
        let mut timing = ScanTiming::new(interval, start);
        for step in 0..10u32 {
            // Every wake is 500 us late and the body is fast.
            let at = start + interval * step + Duration::from_micros(500);
            timing.wake(at);
            timing.finish(at + Duration::from_micros(50));
            timing.schedule_next();
        }
        assert_eq!(
            timing.expected(),
            start + interval * 10,
            "ten intervals, not ten wakes plus a bit each time"
        );
        assert_eq!(timing.snapshot().overruns, 10, "each wake was late");
    }

    #[test]
    fn a_clean_run_reports_no_lateness() {
        let start = base();
        let interval = Duration::from_millis(2);
        let mut timing = ScanTiming::new(interval, start);
        for step in 0..5u32 {
            let at = start + interval * step;
            timing.wake(at);
            timing.finish(at + Duration::from_micros(200));
            timing.schedule_next();
        }
        let snapshot = timing.snapshot();
        assert_eq!(snapshot.overruns, 0);
        assert_eq!(snapshot.latency.now_us, 0);
        assert_eq!(snapshot.scan.now_us, 200);
        assert_eq!(snapshot.cycle.now_us, 2_000, "the cycle is the interval");
        assert!(snapshot.scan.max_us >= snapshot.scan.min_us);
    }
}
