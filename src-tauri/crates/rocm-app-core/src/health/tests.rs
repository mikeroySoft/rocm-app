// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Overview tests, plus the generator for `fixtures/dashboard.json`.
//!
//! The renderer never derives a verdict, a component row, or a metric: it
//! renders what this module produces from producer-generated snapshots. The
//! generator lives here for the same reason the onboarding one does — a
//! hand-written screen fixture drifts from the backend silently.

use serde::Serialize;

use super::{
    ComponentStatus, FRESHNESS_TTL_MS, GpuSample, HealthOverview, HistoryPoint, MetricValue,
    NoticeCode, REQUIRED_KINDS, TelemetryFailure, TelemetryInput, overview, reason_copy,
    verdict_label,
};
use crate::contract::{
    self, AppSnapshot, ComponentKind, ComponentReport, ComponentState, EligibleAction,
    HealthReason, HealthVerdict, ReasonCode, SourceTrust, SupportLink, UpdateState,
};

/// Same instant the contract fixtures were observed at, so a fixture is fresh
/// unless a test deliberately ages it.
const NOW: u64 = 1_767_225_600_000;

/// The desktop app's own version. The CLI cannot observe it, so the consumer
/// supplies it; the fixtures pin a literal rather than this crate's version so
/// a release bump does not rewrite every committed screen.
const APP_VERSION: &str = "0.1.0";

fn snapshot_named(name: &str) -> AppSnapshot {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../fixtures/contract/");
    let raw = std::fs::read_to_string(format!("{path}{name}.json"))
        .unwrap_or_else(|e| panic!("missing fixture {name}: {e}"));
    contract::decode(&raw).unwrap_or_else(|e| panic!("fixture {name} failed to decode: {e}"))
}

fn live_sample() -> GpuSample {
    GpuSample {
        device: "card0".to_owned(),
        utilization_pct: Some(37.0),
        vram_used_mb: Some(12_288),
        vram_total_mb: Some(32_768),
        temperature_c: Some(51.0),
        power_w: Some(118.0),
    }
}

fn live_telemetry() -> TelemetryInput {
    TelemetryInput {
        sample: Some(live_sample()),
        failure: None,
        history: vec![
            HistoryPoint {
                at_unix_ms: NOW - 60_000,
                utilization_pct: Some(21.0),
                vram_used_mb: Some(9_216),
            },
            HistoryPoint {
                at_unix_ms: NOW - 30_000,
                utilization_pct: Some(44.0),
                vram_used_mb: Some(11_264),
            },
            HistoryPoint {
                at_unix_ms: NOW,
                utilization_pct: Some(37.0),
                vram_used_mb: Some(12_288),
            },
        ],
    }
}

fn view(fixture: &str) -> HealthOverview {
    overview(
        &snapshot_named(fixture),
        &live_telemetry(),
        NOW,
        Some(APP_VERSION),
    )
}

/// Every string this page puts in front of a user.
fn visible_text(o: &HealthOverview) -> Vec<String> {
    let mut out = vec![
        o.verdict_label.clone(),
        o.summary.clone(),
        o.next_step.label.clone(),
    ];
    out.extend(
        o.headline_facts
            .iter()
            .flat_map(|f| [f.label.clone(), f.value.clone()]),
    );
    out.push(o.freshness.label.clone());
    for row in &o.components {
        out.push(row.label.clone());
        out.push(row.value.clone());
        out.push(row.status_label.clone());
        out.extend(row.note.clone());
    }
    out.push(o.driver.summary.clone());
    out.push(o.driver.note.clone());
    for metric in &o.telemetry.metrics {
        out.push(metric.label.clone());
        out.push(match &metric.value {
            MetricValue::Reading { text, .. } => text.clone(),
            MetricValue::Unavailable { reason } => reason.clone(),
        });
    }
    out.extend(o.notices.iter().map(|n| n.message.clone()));
    out
}

// ---------------------------------------------------------------------------
// Typed derivation
// ---------------------------------------------------------------------------

/// Criterion: the verdict comes from typed fields, never from prose or an
/// exit code.
///
/// The strongest available form of this test: rewrite every prose field to say
/// the opposite of the truth and require the screen to be unmoved.
#[test]
fn health_verdict_ignores_producer_prose() {
    let mut lying = snapshot_named("healthy");
    lying.health.next_action = Some("EVERYTHING IS BROKEN, uninstall ROCm".to_owned());
    lying.health.reasons = vec![HealthReason {
        code: ReasonCode::UpdateAvailable,
        detail: "catastrophic failure, no runtime, unsupported platform".to_owned(),
    }];

    let o = overview(&lying, &live_telemetry(), NOW, Some(APP_VERSION));
    assert_eq!(o.verdict, HealthVerdict::Healthy);
    assert_eq!(o.verdict_label, "Ready");
    // The summary is the reviewed copy for the *code*, not the detail string.
    assert_eq!(o.summary, reason_copy(ReasonCode::UpdateAvailable));
    assert!(!o.summary.contains("catastrophic"));
    assert!(!o.next_step.label.to_lowercase().contains("uninstall"));
}

#[test]
fn health_every_reason_code_has_distinct_reviewed_copy() {
    let codes = [
        ReasonCode::PlatformWsl,
        ReasonCode::PlatformUnsupportedOs,
        ReasonCode::GpuAbsent,
        ReasonCode::GpuUnrecognisedFamily,
        ReasonCode::RuntimeAbsent,
        ReasonCode::RuntimeValidationFailed,
        ReasonCode::RuntimeActiveMissing,
        ReasonCode::RuntimeAmbiguousSelection,
        ReasonCode::DriverNotDetected,
        ReasonCode::UpdateAvailable,
        ReasonCode::UpdateMetadataUntrusted,
        ReasonCode::UpdateOffline,
        ReasonCode::ProbeIncomplete,
        ReasonCode::Unrecognised,
    ];
    let mut seen: Vec<&str> = Vec::new();
    for code in codes {
        let copy = reason_copy(code);
        assert!(!copy.trim().is_empty(), "{code:?} has no copy");
        assert!(!seen.contains(&copy), "{code:?} reuses another code's copy");
        seen.push(copy);
    }
}

#[test]
fn health_every_verdict_has_a_text_label() {
    for verdict in [
        HealthVerdict::Healthy,
        HealthVerdict::Unknown,
        HealthVerdict::SetupRequired,
        HealthVerdict::Attention,
        HealthVerdict::Unsupported,
    ] {
        assert!(!verdict_label(verdict).trim().is_empty(), "{verdict:?}");
    }
}

/// A next step that names a mutation must be one the backend actually offers.
#[test]
fn health_next_step_never_offers_an_ineligible_action() {
    for fixture in [
        "healthy",
        "setup-required",
        "attention",
        "partial",
        "offline-stale",
        "unsupported-wsl",
    ] {
        let snapshot = snapshot_named(fixture);
        let o = overview(&snapshot, &live_telemetry(), NOW, Some(APP_VERSION));
        assert!(!o.next_step.label.trim().is_empty(), "{fixture}");
        if let Some(action) = o.next_step.action {
            assert!(
                snapshot.offerable_actions().contains(&action),
                "{fixture} offers {action:?} which the backend does not"
            );
        }
    }
}

#[test]
fn health_unsupported_host_is_offered_no_action_at_all() {
    let o = view("unsupported-wsl");
    assert_eq!(o.verdict, HealthVerdict::Unsupported);
    assert!(o.next_step.action.is_none());
    assert!(
        o.notices.iter().any(|n| n.code == NoticeCode::Unsupported),
        "an unsupported host must say so"
    );
}

#[test]
fn health_setup_required_points_at_setup() {
    let o = view("setup-required");
    assert_eq!(o.next_step.action, Some(EligibleAction::InstallRuntime));
    assert_eq!(o.next_step.label, "Set up ROCm");
}

// ---------------------------------------------------------------------------
// First viewport
// ---------------------------------------------------------------------------

/// Criterion: verdict, primary reason/next action, GPU, active ROCm version,
/// and freshness are all present.
#[test]
fn health_first_viewport_answers_the_five_questions() {
    let o = view("healthy");
    assert_eq!(o.verdict_label, "Ready");
    assert!(!o.summary.trim().is_empty());
    assert!(!o.next_step.label.trim().is_empty());

    let keys: Vec<&str> = o.headline_facts.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["gpu", "system", "rocm"]);
    for fact in &o.headline_facts {
        assert!(!fact.value.trim().is_empty(), "{} is blank", fact.key);
    }
    assert_eq!(
        o.headline_facts
            .iter()
            .find(|f| f.key == "rocm")
            .map(|f| f.value.as_str()),
        Some("7.14.0")
    );
    assert!(!o.freshness.label.trim().is_empty());
}

#[test]
fn health_reports_a_failed_active_runtime_in_the_headline() {
    let o = view("attention");
    let rocm = o
        .headline_facts
        .iter()
        .find(|f| f.key == "rocm")
        .expect("rocm fact");
    assert!(
        rocm.value.contains("failed its check"),
        "a failing runtime must not read as a plain version: {}",
        rocm.value
    );
}

#[test]
fn health_reports_no_active_runtime_without_pretending_there_is_one() {
    let o = view("setup-required");
    let rocm = o
        .headline_facts
        .iter()
        .find(|f| f.key == "rocm")
        .expect("rocm fact");
    assert_eq!(rocm.value, "None yet");
}

// ---------------------------------------------------------------------------
// Component inventory
// ---------------------------------------------------------------------------

/// Criterion: every required component renders or shows an explicit
/// non-empty unknown / not-installed state.
#[test]
fn health_inventory_covers_every_required_component() {
    for fixture in ["healthy", "setup-required", "attention", "partial"] {
        let o = view(fixture);
        for kind in REQUIRED_KINDS {
            let row = o
                .components
                .iter()
                .find(|r| r.kind == kind)
                .unwrap_or_else(|| panic!("{fixture} has no row for {kind:?}"));
            assert!(!row.label.trim().is_empty(), "{fixture} {kind:?} label");
            assert!(!row.value.trim().is_empty(), "{fixture} {kind:?} value");
            assert!(
                !row.status_label.trim().is_empty(),
                "{fixture} {kind:?} status label"
            );
        }
    }
}

/// A component the producer never mentioned reads as unknown, not as absent.
#[test]
fn health_unreported_component_is_explicitly_unknown() {
    let o = view("healthy");
    let python = o
        .components
        .iter()
        .find(|r| r.kind == ComponentKind::Python)
        .expect("python row");
    assert_eq!(python.status, ComponentStatus::Unknown);
    assert_eq!(python.value, "Not reported");
    assert!(python.note.is_some());
}

#[test]
fn health_distinguishes_the_four_absent_looking_states() {
    let mut snapshot = snapshot_named("healthy");
    snapshot.components = vec![
        ComponentReport {
            kind: ComponentKind::Python,
            name: "python".to_owned(),
            state: ComponentState::NotInstalled,
        },
        ComponentReport {
            kind: ComponentKind::PyTorch,
            name: "torch".to_owned(),
            state: ComponentState::Unknown {
                reason: "the check timed out".to_owned(),
            },
        },
        ComponentReport {
            kind: ComponentKind::SystemHipRocm,
            name: "hip".to_owned(),
            state: ComponentState::Unsupported {
                version: "5.7.0".to_owned(),
                reason: "too old for this graphics card".to_owned(),
            },
        },
        ComponentReport {
            kind: ComponentKind::Engine,
            name: "lemonade".to_owned(),
            state: ComponentState::Stale {
                version: Some("1.2.3".to_owned()),
                checked_at_unix_ms: NOW - 86_400_000,
            },
        },
    ];
    let o = overview(&snapshot, &live_telemetry(), NOW, Some(APP_VERSION));
    let status = |kind: ComponentKind| {
        o.components
            .iter()
            .find(|r| r.kind == kind)
            .map(|r| r.status)
            .expect("row")
    };
    assert_eq!(status(ComponentKind::Python), ComponentStatus::NotInstalled);
    assert_eq!(status(ComponentKind::PyTorch), ComponentStatus::Unknown);
    assert_eq!(
        status(ComponentKind::SystemHipRocm),
        ComponentStatus::Unsupported
    );
    assert_eq!(status(ComponentKind::Engine), ComponentStatus::Stale);
    // All four are visually distinct in text alone.
    let labels: Vec<&str> = [
        ComponentStatus::NotInstalled,
        ComponentStatus::Unknown,
        ComponentStatus::Unsupported,
        ComponentStatus::Stale,
    ]
    .iter()
    .map(|s| s.label())
    .collect();
    let mut unique = labels.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), labels.len());
}

/// The stale note reads like the freshness line, not like a debugger: raw
/// epoch milliseconds on the Overview read as a bug, not a time.
#[test]
fn health_stale_component_note_is_humanized() {
    let mut snapshot = snapshot_named("healthy");
    snapshot.components = vec![ComponentReport {
        kind: ComponentKind::Engine,
        name: "lemonade".to_owned(),
        state: ComponentState::Stale {
            version: Some("1.2.3".to_owned()),
            checked_at_unix_ms: NOW - 3 * 60 * 60_000,
        },
    }];
    let o = overview(&snapshot, &live_telemetry(), NOW, Some(APP_VERSION));
    let row = o
        .components
        .iter()
        .find(|r| r.kind == ComponentKind::Engine)
        .expect("engine row");
    assert_eq!(
        row.note.as_deref(),
        Some("Last read 3 hours ago and not re-checked since.")
    );
}

/// A second engine is appended, not silently swallowed by the first.
#[test]
fn health_extra_components_are_shown_not_dropped() {
    let mut snapshot = snapshot_named("healthy");
    snapshot.components.push(ComponentReport {
        kind: ComponentKind::Engine,
        name: "lemonade".to_owned(),
        state: ComponentState::Installed {
            version: "1.0.0".to_owned(),
        },
    });
    snapshot.components.push(ComponentReport {
        kind: ComponentKind::Engine,
        name: "vllm".to_owned(),
        state: ComponentState::Installed {
            version: "0.9.0".to_owned(),
        },
    });
    let o = overview(&snapshot, &live_telemetry(), NOW, Some(APP_VERSION));
    let engines: Vec<&str> = o
        .components
        .iter()
        .filter(|r| r.kind == ComponentKind::Engine)
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(engines.len(), 2, "{engines:?}");
    assert!(engines.iter().any(|l| l.contains("vllm")));
}

// ---------------------------------------------------------------------------
// Driver: report only
// ---------------------------------------------------------------------------

/// Criterion: driver rows carry no install/update/remove control.
#[test]
fn health_driver_row_carries_no_control() {
    let mut snapshot = snapshot_named("healthy");
    snapshot.driver.support_links = vec![SupportLink {
        label: "AMD driver release notes".to_owned(),
        url: "https://www.amd.com/notes".to_owned(),
    }];
    let o = overview(&snapshot, &live_telemetry(), NOW, Some(APP_VERSION));

    assert_eq!(o.driver.links.len(), 1);
    let json = serde_json::to_string(&o).expect("overview serializes");
    for forbidden in ["install-driver", "update-driver", "remove-driver", "dkms"] {
        assert!(!json.contains(forbidden), "overview offers {forbidden:?}");
    }
    // And the driver inventory row is a report: a status and a version, with
    // no action field to render as a button.
    let row = o
        .components
        .iter()
        .find(|r| r.kind == ComponentKind::Driver)
        .expect("driver row");
    assert!(!row.value.trim().is_empty());
}

// ---------------------------------------------------------------------------
// Telemetry degrades one metric at a time
// ---------------------------------------------------------------------------

/// Criterion: a collector failure leaves health intact and marks only the
/// affected metrics unavailable.
#[test]
fn health_survives_a_total_telemetry_failure() {
    let snapshot = snapshot_named("healthy");
    let healthy = overview(&snapshot, &live_telemetry(), NOW, Some(APP_VERSION));
    let degraded = overview(
        &snapshot,
        &TelemetryInput {
            sample: None,
            failure: Some(TelemetryFailure::Permission),
            history: Vec::new(),
        },
        NOW,
        Some(APP_VERSION),
    );

    // Health, inventory, and driver are byte-identical.
    assert_eq!(degraded.verdict, healthy.verdict);
    assert_eq!(degraded.summary, healthy.summary);
    assert_eq!(degraded.components, healthy.components);
    assert_eq!(degraded.driver, healthy.driver);

    // Only the metrics changed, and each says why.
    assert_eq!(
        degraded.telemetry.metrics.len(),
        healthy.telemetry.metrics.len()
    );
    for metric in &degraded.telemetry.metrics {
        let MetricValue::Unavailable { reason } = &metric.value else {
            panic!("{} should be unavailable", metric.key);
        };
        assert!(reason.contains("not allowed"), "{reason}");
    }
    assert!(
        degraded
            .notices
            .iter()
            .any(|n| n.code == NoticeCode::TelemetryPermission)
    );
}

/// A partial sample yields partial readings, not an empty panel.
#[test]
fn health_reports_the_metrics_it_has_and_names_the_ones_it_lacks() {
    let o = overview(
        &snapshot_named("healthy"),
        &TelemetryInput {
            sample: Some(GpuSample {
                temperature_c: None,
                power_w: None,
                ..live_sample()
            }),
            failure: None,
            history: Vec::new(),
        },
        NOW,
        Some(APP_VERSION),
    );
    let value = |key: &str| {
        o.telemetry
            .metrics
            .iter()
            .find(|m| m.key == key)
            .map(|m| m.value.clone())
            .expect("metric")
    };
    assert!(matches!(value("utilization"), MetricValue::Reading { .. }));
    assert!(matches!(value("vram"), MetricValue::Reading { .. }));
    assert!(matches!(
        value("temperature"),
        MetricValue::Unavailable { .. }
    ));
    assert!(matches!(value("power"), MetricValue::Unavailable { .. }));
}

#[test]
fn health_zero_utilisation_is_a_reading_not_a_gap() {
    let o = overview(
        &snapshot_named("healthy"),
        &TelemetryInput {
            sample: Some(GpuSample {
                utilization_pct: Some(0.0),
                ..live_sample()
            }),
            failure: None,
            history: Vec::new(),
        },
        NOW,
        Some(APP_VERSION),
    );
    let MetricValue::Reading { text, .. } = o
        .telemetry
        .metrics
        .iter()
        .find(|m| m.key == "utilization")
        .map(|m| m.value.clone())
        .expect("metric")
    else {
        panic!("an idle GPU reads 0%, which is a reading");
    };
    assert_eq!(text, "0%");
}

/// `GpuMetrics` defaults every field to zero, so zero has to be interpreted
/// per field rather than trusted or discarded wholesale.
#[test]
fn health_gpu_sample_reads_default_metrics_as_missing() {
    let sample = GpuSample::from_metrics(&rocm_dash_core::GpuMetrics::default());
    assert_eq!(sample.utilization_pct, Some(0.0), "0% is a real reading");
    assert_eq!(sample.vram_total_mb, None, "0 MB of VRAM is not a reading");
    assert_eq!(sample.temperature_c, None);
    assert_eq!(sample.power_w, None);

    let live = GpuSample::from_metrics(&rocm_dash_core::GpuMetrics {
        device_id: "card0".to_owned(),
        vram_used_mb: 1_024,
        vram_total_mb: 32_768,
        gpu_utilization_pct: 42.0,
        temperature_c: 55.0,
        power_w: 130.0,
        clock_mhz: None,
    });
    assert_eq!(live.vram_total_mb, Some(32_768));
    assert_eq!(live.temperature_c, Some(55.0));
}

// ---------------------------------------------------------------------------
// Freshness
// ---------------------------------------------------------------------------

/// Criterion: cached data becomes visibly stale after the TTL.
#[test]
fn health_marks_data_stale_after_the_ttl() {
    let snapshot = snapshot_named("healthy");
    let fresh = overview(
        &snapshot,
        &live_telemetry(),
        snapshot.observed_at_unix_ms,
        Some(APP_VERSION),
    );
    assert!(!fresh.freshness.stale);
    assert_eq!(fresh.freshness.label, "Checked just now");
    assert!(!fresh.notices.iter().any(|n| n.code == NoticeCode::Stale));

    let edge = overview(
        &snapshot,
        &live_telemetry(),
        snapshot.observed_at_unix_ms + FRESHNESS_TTL_MS,
        Some(APP_VERSION),
    );
    assert!(!edge.freshness.stale, "exactly at the TTL is still current");

    let stale = overview(
        &snapshot,
        &live_telemetry(),
        snapshot.observed_at_unix_ms + FRESHNESS_TTL_MS + 1,
        Some(APP_VERSION),
    );
    assert!(stale.freshness.stale);
    assert!(stale.notices.iter().any(|n| n.code == NoticeCode::Stale));
    assert!(stale.freshness.label.contains("Last checked"));
}

#[test]
fn health_age_labels_read_like_english() {
    let snapshot = snapshot_named("healthy");
    let label = |age_ms: u64| {
        overview(
            &snapshot,
            &live_telemetry(),
            snapshot.observed_at_unix_ms + age_ms,
            Some(APP_VERSION),
        )
        .freshness
        .label
    };
    assert_eq!(label(0), "Checked just now");
    assert_eq!(label(60_000), "Checked just now");
    assert_eq!(label(100_000), "Last checked 1 minute ago");
    assert_eq!(label(12 * 60_000), "Last checked 12 minutes ago");
    assert_eq!(label(30 * 60_000), "Last checked 30 minutes ago");
    assert_eq!(label(59 * 60_000), "Last checked 59 minutes ago");
    assert_eq!(label(60 * 60_000), "Last checked 1 hour ago");
    assert_eq!(label(90 * 60_000), "Last checked 1 hour ago");
    assert_eq!(label(2 * 60 * 60_000), "Last checked 2 hours ago");
    assert_eq!(label(5 * 60 * 60_000), "Last checked 5 hours ago");
    assert_eq!(label(47 * 60 * 60_000), "Last checked 47 hours ago");
    assert_eq!(label(48 * 60 * 60_000), "Last checked 2 days ago");
    assert_eq!(label(3 * 24 * 60 * 60_000), "Last checked 3 days ago");
    assert_eq!(label(13 * 24 * 60 * 60_000), "Last checked 13 days ago");
    assert_eq!(
        label(14 * 24 * 60 * 60_000),
        "Last checked more than two weeks ago"
    );
    assert_eq!(
        label(4995 * 60 * 60_000),
        "Last checked more than two weeks ago"
    );
}

/// A clock that has gone backwards must not produce a nonsense age.
#[test]
fn health_tolerates_a_clock_behind_the_observation() {
    let snapshot = snapshot_named("healthy");
    let o = overview(
        &snapshot,
        &live_telemetry(),
        snapshot.observed_at_unix_ms - 5_000,
        Some(APP_VERSION),
    );
    assert_eq!(o.freshness.age_ms, 0);
    assert!(!o.freshness.stale);
}

// ---------------------------------------------------------------------------
// Notices and accessibility
// ---------------------------------------------------------------------------

#[test]
fn health_offline_and_untrusted_metadata_are_both_reported() {
    let o = view("offline-stale");
    assert!(o.notices.iter().any(|n| n.code == NoticeCode::Offline));
    assert!(
        o.notices
            .iter()
            .any(|n| n.code == NoticeCode::UntrustedMetadata)
    );
}

#[test]
fn health_partial_probe_is_flagged() {
    let o = view("partial");
    assert!(o.notices.iter().any(|n| n.code == NoticeCode::PartialProbe));
}

/// Criterion (#28): unmanaged installs surface as one counted notice, and
/// only a notice — legal coexistence never overrides the health verdict.
#[test]
fn health_unmanaged_installs_get_a_counted_notice_not_a_verdict() {
    let with = view("attention");
    let notice = with
        .notices
        .iter()
        .find(|n| n.code == NoticeCode::UnmanagedRocm)
        .expect("attention carries three unmanaged installs");
    assert!(
        notice.message.contains('3'),
        "the notice must carry the count: {}",
        notice.message
    );
    assert_eq!(with.verdict, snapshot_named("attention").health.verdict);

    let without = view("healthy");
    assert!(
        !without
            .notices
            .iter()
            .any(|n| n.code == NoticeCode::UnmanagedRocm)
    );
}

/// Criterion: every state is identifiable from text alone.
#[test]
fn health_every_state_is_carried_by_text() {
    for fixture in [
        "healthy",
        "setup-required",
        "attention",
        "partial",
        "offline-stale",
        "unsupported-wsl",
    ] {
        let o = view(fixture);
        for text in visible_text(&o) {
            assert!(!text.trim().is_empty(), "{fixture} has a blank label");
        }
        for notice in &o.notices {
            assert!(!notice.message.trim().is_empty(), "{fixture} blank notice");
        }
    }
}

#[test]
fn health_copy_never_offers_cpu_fallback_or_an_assistant() {
    for fixture in [
        "healthy",
        "setup-required",
        "attention",
        "partial",
        "offline-stale",
        "unsupported-wsl",
    ] {
        for text in visible_text(&view(fixture)) {
            let lower = text.to_lowercase();
            for banned in ["cpu fallback", "fall back to cpu", "llm", "assistant"] {
                assert!(
                    !lower.contains(banned),
                    "{fixture} mentions {banned}: {text}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fixture generation
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardFixture {
    name: &'static str,
    purpose: &'static str,
    now_unix_ms: u64,
    snapshot: AppSnapshot,
    telemetry: TelemetryInput,
    overview: HealthOverview,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FatalFixture {
    name: &'static str,
    purpose: &'static str,
    error: FatalError,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FatalError {
    code: &'static str,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardFixtures {
    states: Vec<DashboardFixture>,
    fatal: Vec<FatalFixture>,
}

fn state(
    name: &'static str,
    purpose: &'static str,
    snapshot: AppSnapshot,
    telemetry: TelemetryInput,
    now_unix_ms: u64,
) -> DashboardFixture {
    let overview = overview(&snapshot, &telemetry, now_unix_ms, Some(APP_VERSION));
    DashboardFixture {
        name,
        purpose,
        now_unix_ms,
        snapshot,
        telemetry,
        overview,
    }
}

// One `state(...)` call per screen the renderer must be able to draw; the
// length is the list, not logic.
#[expect(clippy::too_many_lines, reason = "a flat list of fixture scenarios")]
fn build_fixtures() -> DashboardFixtures {
    let no_gpu = {
        let mut s = snapshot_named("setup-required");
        s.gpu.name = None;
        s.gpu.gfx_target = None;
        s.gpu.therock_family = None;
        s.health.verdict = HealthVerdict::SetupRequired;
        s.health.reasons = vec![HealthReason {
            code: ReasonCode::GpuAbsent,
            detail: "no AMD graphics device was found".to_owned(),
        }];
        s.eligible_actions = Vec::new();
        s
    };
    let untrusted = {
        let mut s = snapshot_named("healthy");
        s.update.state = UpdateState::UntrustedMetadata {
            detail: "signature did not verify".to_owned(),
        };
        s.update.trust = SourceTrust::Untrusted {
            reason: "signature did not verify".to_owned(),
        };
        s
    };
    let no_telemetry = |failure: TelemetryFailure| TelemetryInput {
        sample: None,
        failure: Some(failure),
        history: Vec::new(),
    };

    DashboardFixtures {
        states: vec![
            state(
                "healthy",
                "everything ready, live readings present",
                snapshot_named("healthy"),
                live_telemetry(),
                NOW,
            ),
            state(
                "setup-required",
                "no ROCm yet; the next step is setup",
                snapshot_named("setup-required"),
                live_telemetry(),
                NOW,
            ),
            state(
                "attention",
                "active version failed its check and an update exists",
                snapshot_named("attention"),
                live_telemetry(),
                NOW,
            ),
            state(
                "stale",
                "the last reading is older than the freshness window",
                snapshot_named("healthy"),
                live_telemetry(),
                NOW + FRESHNESS_TTL_MS + 60_000,
            ),
            state(
                "partial",
                "some checks did not finish",
                snapshot_named("partial"),
                live_telemetry(),
                NOW,
            ),
            state(
                "unsupported",
                "WSL: read-only, no action anywhere",
                snapshot_named("unsupported-wsl"),
                live_telemetry(),
                NOW,
            ),
            state(
                "offline",
                "AMD unreachable; update information may be out of date",
                snapshot_named("offline-stale"),
                live_telemetry(),
                NOW,
            ),
            state(
                "untrusted-metadata",
                "the download list failed its signature check",
                untrusted,
                live_telemetry(),
                NOW,
            ),
            state(
                "no-gpu",
                "no AMD graphics card on this computer",
                no_gpu,
                no_telemetry(TelemetryFailure::NoDevice),
                NOW,
            ),
            state(
                "telemetry-permission",
                "health intact, live readings refused for permission",
                snapshot_named("healthy"),
                no_telemetry(TelemetryFailure::Permission),
                NOW,
            ),
            state(
                "telemetry-partial",
                "some readings present, others not reported",
                snapshot_named("healthy"),
                TelemetryInput {
                    sample: Some(GpuSample {
                        temperature_c: None,
                        power_w: None,
                        ..live_sample()
                    }),
                    failure: None,
                    history: live_telemetry().history,
                },
                NOW,
            ),
        ],
        fatal: vec![FatalFixture {
            name: "contract-unreadable",
            purpose: "the backend could not produce a snapshot at all",
            error: FatalError {
                code: "inspection",
                message: contract::ContractError::UnsupportedSchemaVersion {
                    found: 99,
                    supported: contract::SUPPORTED_SCHEMA_VERSION,
                }
                .user_message(),
            },
        }],
    }
}

/// The renderer's fixture file is generated from this module.
///
/// Machine-independent: every input is a committed contract fixture or a
/// literal, and the clock is a constant. No `#[cfg]` gate is needed.
#[test]
fn health_dashboard_fixtures_match_the_committed_file() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../fixtures/dashboard.json"
    );
    let generated = format!(
        "{}\n",
        serde_json::to_string_pretty(&build_fixtures()).expect("fixtures serialize")
    );

    if std::env::var_os("ROCM_APP_WRITE_FIXTURES").is_some() {
        std::fs::write(path, &generated).expect("write fixtures");
        return;
    }

    let committed = std::fs::read_to_string(path).unwrap_or_default();
    assert_eq!(
        committed, generated,
        "fixtures/dashboard.json is stale; regenerate with \
         ROCM_APP_WRITE_FIXTURES=1 cargo test -p rocm-app-core health_dashboard_fixtures"
    );
}
