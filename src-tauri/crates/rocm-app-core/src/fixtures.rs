// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Deterministic fixture snapshots.
//!
//! `fixtures/scenarios.json` at the repository root is the single source of
//! truth, read by both this module and the TypeScript renderer. Two
//! hand-maintained copies of the same fixture set drift silently, and a
//! renderer test then passes against data the backend would never produce.
//!
//! Fixture mode touches no GPU, no network, and no real ROCm config, data, or
//! cache root. That is what lets the UI, the screenshot runs, and the e2e
//! harness execute anywhere.

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::platform::HostPlatform;

/// Raw fixture text, embedded at compile time so no file read can fail at
/// runtime and no test needs a working directory.
const SCENARIOS_JSON: &str = include_str!("../../../../fixtures/scenarios.json");

/// A named fixture scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scenario {
    Healthy,
    SetupRequired,
    Attention,
    UnsupportedWsl,
    Partial,
}

/// The overall answer to "is ROCm working on this computer?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Healthy,
    SetupRequired,
    Attention,
    Unsupported,
    Unknown,
}

/// One deterministic health snapshot.
///
/// Phase 2 replaces this with the versioned health contract; the shape is kept
/// deliberately small here so that replacement is a rewrite, not a migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureSnapshot {
    pub scenario: Scenario,
    pub platform: HostPlatform,
    pub verdict: Verdict,
    /// Typed reason this verdict was reached. Health is never inferred from a
    /// process exit code alone.
    pub reason_code: String,
    pub headline: String,
    pub detail: String,
    pub install_available: bool,
    /// Fixed timestamp — fixtures must not read a clock, or screenshot diffs
    /// and golden tests fail once a day for no reason.
    pub checked_at: String,
}

impl Scenario {
    /// Every scenario, in presentation order.
    pub const ALL: [Self; 5] = [
        Self::Healthy,
        Self::SetupRequired,
        Self::Attention,
        Self::UnsupportedWsl,
        Self::Partial,
    ];

    /// Stable wire identifier, matching the `serde` representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::SetupRequired => "setup-required",
            Self::Attention => "attention",
            Self::UnsupportedWsl => "unsupported-wsl",
            Self::Partial => "partial",
        }
    }

    /// Parse a wire identifier.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }
}

static SNAPSHOTS: LazyLock<Vec<FixtureSnapshot>> = LazyLock::new(|| {
    serde_json::from_str(SCENARIOS_JSON).expect("fixtures/scenarios.json is malformed")
});

/// Every fixture snapshot.
#[must_use]
pub fn all() -> &'static [FixtureSnapshot] {
    &SNAPSHOTS
}

/// The snapshot for `scenario`.
///
/// Panics only if the fixture file lost a scenario, which
/// `every_scenario_has_exactly_one_snapshot` turns into a test failure first.
#[must_use]
pub fn snapshot(scenario: Scenario) -> &'static FixtureSnapshot {
    all()
        .iter()
        .find(|s| s.scenario == scenario)
        .unwrap_or_else(|| panic!("fixtures/scenarios.json is missing {scenario:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_file_parses() {
        assert_eq!(all().len(), Scenario::ALL.len());
    }

    #[test]
    fn every_scenario_has_exactly_one_snapshot() {
        for scenario in Scenario::ALL {
            let matches = all().iter().filter(|s| s.scenario == scenario).count();
            assert_eq!(matches, 1, "{scenario:?} must appear exactly once");
        }
    }

    /// The load-bearing safety property of the whole fixture set: no fixture may
    /// advertise an install on a host that cannot support one. Without this, a
    /// hand-edited fixture can put a working Install button in front of a WSL
    /// user and every renderer test will happily agree.
    #[test]
    fn no_fixture_offers_install_on_an_ineligible_host() {
        for snap in all() {
            if snap.install_available {
                assert!(
                    snap.platform.install_allowed(),
                    "{:?} offers install on {:?}",
                    snap.scenario,
                    snap.platform
                );
            }
        }
    }

    #[test]
    fn wsl_fixture_is_unsupported_and_offers_nothing() {
        let snap = snapshot(Scenario::UnsupportedWsl);
        assert_eq!(snap.platform, HostPlatform::Wsl);
        assert_eq!(snap.verdict, Verdict::Unsupported);
        assert!(!snap.install_available);
        assert!(!snap.platform.install_allowed());
    }

    #[test]
    fn setup_required_is_the_only_installable_fixture() {
        let installable: Vec<_> = all()
            .iter()
            .filter(|s| s.install_available)
            .map(|s| s.scenario)
            .collect();
        assert_eq!(installable, vec![Scenario::SetupRequired]);
    }

    #[test]
    fn every_snapshot_carries_user_facing_text() {
        for snap in all() {
            assert!(!snap.headline.is_empty(), "{:?} headline", snap.scenario);
            assert!(!snap.detail.is_empty(), "{:?} detail", snap.scenario);
            assert!(!snap.reason_code.is_empty(), "{:?} reason", snap.scenario);
        }
    }

    /// Fixtures must not read a clock. A drifting timestamp breaks screenshot
    /// diffing and golden comparisons on a schedule nobody can reproduce.
    #[test]
    fn timestamps_are_fixed() {
        for snap in all() {
            assert_eq!(snap.checked_at, "2026-01-01T00:00:00Z");
        }
    }

    #[test]
    fn scenario_wire_names_round_trip() {
        for scenario in Scenario::ALL {
            assert_eq!(Scenario::from_wire(scenario.as_str()), Some(scenario));
        }
        assert_eq!(Scenario::from_wire("nope"), None);
    }
}
