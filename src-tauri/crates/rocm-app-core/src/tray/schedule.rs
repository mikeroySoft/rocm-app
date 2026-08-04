// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! How often the monitor is allowed to look, and when it must not.
//!
//! # Three probes, three costs
//!
//! | Probe | Cost | Cadence |
//! |---|---|---|
//! | cached metrics | one `amd-smi` read | seconds |
//! | full health | one `rocm app-snapshot` — subprocess, sysfs, registry | a minute |
//! | update check | the *same* snapshot, but it is the only probe allowed to announce an update | hours |
//!
//! The update check shares its host action with full health on purpose. The
//! producer answers update availability from a bounded cache inside the
//! snapshot it already builds, so a separate update probe would be a second
//! subprocess for data the first one already returned. [`Due`] encodes that:
//! `update` is only ever set alongside `health`, which makes coalescing a
//! property of the type rather than a rule a caller has to remember.
//!
//! # Deferring during a change
//!
//! A full probe during an install contends with the install: same CLI, same
//! files, same registry. So while a mutation is running, full health and the
//! update check are withheld and one resume is armed; cached metrics keep
//! flowing, because a read of the GPU's temperature conflicts with nothing and
//! is what keeps the tray alive while a user watches a long install.
//!
//! # No overlap, by construction
//!
//! [`Scheduler::due`] marks what it hands out as in flight and will not hand it
//! out again until [`Scheduler::finished`]. A slow snapshot therefore cannot
//! stack a second one behind it, however long it takes.

/// How long between probes of each kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intervals {
    pub metrics_ms: u64,
    pub health_ms: u64,
    pub update_ms: u64,
}

impl Default for Intervals {
    /// Two seconds, one minute, six hours.
    ///
    /// Metrics are cheap and are the only thing that visibly moves. Full health
    /// is a subprocess, so a minute keeps the tray current without making the
    /// app a background load. Six hours is under the producer's twelve-hour
    /// index cache, so a machine left running still learns about an update the
    /// same day without ever forcing a network fetch of its own.
    fn default() -> Self {
        Self {
            metrics_ms: 2 * 1_000,
            health_ms: 60 * 1_000,
            update_ms: 6 * 60 * 60 * 1_000,
        }
    }
}

/// What is due right now.
///
/// `update` implies `health`: the snapshot that answers one answers both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Due {
    pub metrics: bool,
    pub health: bool,
    pub update: bool,
}

impl Due {
    /// Whether there is anything at all to do.
    #[must_use]
    pub const fn any(self) -> bool {
        self.metrics || self.health
    }
}

/// The monitor's clock.
#[derive(Debug)]
pub struct Scheduler {
    intervals: Intervals,
    last_metrics: Option<u64>,
    last_health: Option<u64>,
    last_update: Option<u64>,
    metrics_in_flight: bool,
    health_in_flight: bool,
    resume_after_mutation: bool,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new(Intervals::default())
    }
}

impl Scheduler {
    #[must_use]
    pub const fn new(intervals: Intervals) -> Self {
        Self {
            intervals,
            last_metrics: None,
            last_health: None,
            last_update: None,
            metrics_in_flight: false,
            health_in_flight: false,
            resume_after_mutation: false,
        }
    }

    /// What to run now, marking it in flight.
    ///
    /// `mutating` comes from [`crate::controller::RocmController::is_mutating`],
    /// read once per tick rather than cached, so a mutation that starts between
    /// ticks is respected on the next one.
    pub const fn due(&mut self, now_ms: u64, mutating: bool) -> Due {
        let metrics = !self.metrics_in_flight
            && elapsed_at_least(self.last_metrics, now_ms, self.intervals.metrics_ms);

        let health = if self.health_in_flight {
            false
        } else if mutating {
            // Withheld, not skipped: the machine is being changed, so the
            // reading this probe would take is about to be wrong anyway.
            self.resume_after_mutation = true;
            false
        } else {
            self.resume_after_mutation
                || elapsed_at_least(self.last_health, now_ms, self.intervals.health_ms)
        };

        let update = health && elapsed_at_least(self.last_update, now_ms, self.intervals.update_ms);

        if metrics {
            self.metrics_in_flight = true;
        }
        if health {
            self.health_in_flight = true;
            self.resume_after_mutation = false;
        }
        Due {
            metrics,
            health,
            update,
        }
    }

    /// Report that the work handed out by [`Scheduler::due`] has finished.
    ///
    /// Timestamped on completion rather than on dispatch: a probe that took
    /// forty seconds should not be considered twenty seconds overdue the
    /// moment it returns.
    pub const fn finished(&mut self, done: Due, now_ms: u64) {
        if done.metrics {
            self.metrics_in_flight = false;
            self.last_metrics = Some(now_ms);
        }
        if done.health {
            self.health_in_flight = false;
            self.last_health = Some(now_ms);
        }
        if done.update {
            self.last_update = Some(now_ms);
        }
    }

    /// Arm exactly one full probe on the next tick, ignoring the interval.
    ///
    /// A mutation's terminal event — completed, failed, or cancelled — arms it
    /// because the machine has just been changed and a failed operation is the
    /// case where nothing else refreshes the tray. It never starts a *second*
    /// concurrent probe: the in-flight guard still applies.
    pub const fn request_full_probe(&mut self) {
        self.resume_after_mutation = true;
    }

    /// Whether a full probe is waiting for a mutation to finish.
    #[must_use]
    pub const fn deferring(&self) -> bool {
        self.resume_after_mutation
    }
}

/// Whether `interval` has passed since `last`.
///
/// A `None` last run is always due. A clock that moved *backwards* — a
/// suspend/resume or an NTP correction — is also treated as due rather than
/// left waiting for wall time to catch up, which could otherwise stall the
/// monitor for as long as the jump.
const fn elapsed_at_least(last: Option<u64>, now_ms: u64, interval_ms: u64) -> bool {
    match last {
        None => true,
        Some(last) => now_ms < last || now_ms - last >= interval_ms,
    }
}
