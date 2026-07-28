// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! The Overview: "ROCm is healthy — or here is exactly why not".
//!
//! # One typed derivation
//!
//! [`overview`] is a pure function of a decoded snapshot, whatever telemetry
//! the host managed to collect, and the current time. Every branch reads a
//! typed field — [`HealthVerdict`], [`ReasonCode`], [`ComponentState`]. No
//! branch reads a process exit code, and no branch matches a substring of the
//! producer's prose. Prose is *displayed*; it is never *decided on*.
//!
//! That distinction is the phase's load-bearing rule: a producer that reworded
//! "no runtime installed" must not change what this screen offers.
//!
//! # Degrading one thing at a time
//!
//! Telemetry arrives as [`TelemetryInput`], where the collector's failure and
//! each individual reading are separate. A collector that cannot start leaves
//! the health verdict, the component inventory, and the driver row exactly as
//! they were, and marks only the metrics unavailable — with a reason a user can
//! act on.

use serde::{Deserialize, Serialize};

use crate::contract::{
    AppSnapshot, ComponentKind, ComponentState, EligibleAction, HealthVerdict, ReasonCode,
    RuntimeValidation, SourceTrust, SupportStatus, UpdateState,
};
use crate::onboarding::{DriverAdvice, Fact, driver_advice, format_bytes};

/// How long an observation stays current.
///
/// Five minutes: long enough that an idle window is not permanently shouting
/// "stale", short enough that a user acting on this screen is acting on
/// something true.
pub const FRESHNESS_TTL_MS: u64 = 5 * 60 * 1_000;

// ---------------------------------------------------------------------------
// Telemetry input
// ---------------------------------------------------------------------------

/// Why the telemetry collector produced nothing.
///
/// Separate variants because the fixes differ: a permission problem is the
/// user's to solve, an unsupported host never will be, and a timeout is worth
/// retrying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TelemetryFailure {
    /// No GPU this collector can read.
    NoDevice,
    /// The device exists but this process may not read it.
    Permission,
    /// The collector is not available on this host.
    Unsupported,
    /// The collector ran but did not answer in time.
    Timeout,
    /// The collector ran and failed.
    Error,
}

/// One GPU reading. Every field is optional: a collector that reports
/// temperature but not power must not force the whole sample to be discarded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuSample {
    pub device: String,
    pub utilization_pct: Option<f32>,
    pub vram_used_mb: Option<u64>,
    pub vram_total_mb: Option<u64>,
    pub temperature_c: Option<f32>,
    pub power_w: Option<f32>,
}

impl GpuSample {
    /// Convert a shared dashboard reading.
    ///
    /// `GpuMetrics` uses zero as its default, so zero has to be interpreted
    /// per field: 0% utilisation is a real, common reading, but 0 MB of total
    /// VRAM, 0 °C, and 0 W are what an unfilled struct looks like, not what a
    /// running GPU reports.
    #[must_use]
    pub fn from_metrics(metrics: &rocm_dash_core::GpuMetrics) -> Self {
        let megabytes = |v: u64| (v > 0).then_some(v);
        let reading = |v: f32| (v > 0.0).then_some(v);
        Self {
            device: metrics.device_id.clone(),
            // Kept even at zero: an idle GPU genuinely reads 0%.
            utilization_pct: metrics
                .gpu_utilization_pct
                .is_finite()
                .then_some(metrics.gpu_utilization_pct),
            vram_used_mb: megabytes(metrics.vram_used_mb),
            vram_total_mb: megabytes(metrics.vram_total_mb),
            temperature_c: reading(metrics.temperature_c),
            power_w: reading(metrics.power_w),
        }
    }
}

/// One point of bounded local history.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPoint {
    pub at_unix_ms: u64,
    pub utilization_pct: Option<f32>,
    pub vram_used_mb: Option<u64>,
}

/// Everything the host could gather about the GPU right now.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryInput {
    pub sample: Option<GpuSample>,
    /// Set when the collector itself could not produce a sample.
    pub failure: Option<TelemetryFailure>,
    pub history: Vec<HistoryPoint>,
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// A component's state, reduced to the handful of things a user can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentStatus {
    Ready,
    UpdateAvailable,
    NotInstalled,
    Unsupported,
    Stale,
    Unknown,
}

impl ComponentStatus {
    /// Text that carries the status on its own.
    ///
    /// Colour is decoration here; this label is the actual signal, so a
    /// monochrome or colour-blind reading of the screen loses nothing.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::UpdateAvailable => "Update available",
            Self::NotInstalled => "Not installed",
            Self::Unsupported => "Not supported",
            Self::Stale => "Last known",
            Self::Unknown => "Unknown",
        }
    }
}

/// One row of the component inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentRow {
    pub kind: ComponentKind,
    /// Plain-language name of the thing, e.g. "Display driver".
    pub label: String,
    /// Version or an explicit non-empty stand-in. Never blank.
    pub value: String,
    pub status: ComponentStatus,
    pub status_label: String,
    /// Extra sentence, only where the status alone is not enough.
    pub note: Option<String>,
}

/// How current the reading is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Freshness {
    pub observed_at_unix_ms: u64,
    pub age_ms: u64,
    pub stale: bool,
    /// e.g. "Checked just now", "Last checked 12 minutes ago".
    pub label: String,
}

/// The single thing to do about the current verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextStep {
    pub label: String,
    /// The typed operation the button would start, when there is one. `None`
    /// means the step is not a mutation — refresh, reconnect, or nothing.
    pub action: Option<EligibleAction>,
}

/// A reading, or an explicit reason there is none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum MetricValue {
    Reading {
        /// Formatted for display, e.g. "43 °C", "12.1 GB of 32 GB".
        text: String,
        /// 0.0–1.0 where a proportion is meaningful, for a bar.
        ratio: Option<f32>,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricRow {
    pub key: String,
    pub label: String,
    pub value: MetricValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryPanel {
    pub device: Option<String>,
    pub metrics: Vec<MetricRow>,
    pub history: Vec<HistoryPoint>,
    /// Set when nothing could be read at all; individual rows still say so.
    pub failure: Option<String>,
}

/// Something true about this machine that is not the headline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub code: NoticeCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoticeCode {
    Offline,
    PartialProbe,
    Unsupported,
    UntrustedMetadata,
    TelemetryPermission,
    Stale,
}

/// Everything the Overview renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthOverview {
    pub verdict: HealthVerdict,
    /// Text form of the verdict. Colour never carries it alone.
    pub verdict_label: String,
    /// The primary reason, in reviewed copy keyed by [`ReasonCode`].
    pub summary: String,
    pub next_step: NextStep,
    /// True when no ROCm version is active yet. The shell sends a first-run
    /// machine straight to guided setup instead of showing an Overview whose
    /// every row reads "none".
    pub first_run: bool,
    /// GPU, active ROCm, and the other first-viewport identity rows.
    pub headline_facts: Vec<Fact>,
    pub freshness: Freshness,
    pub components: Vec<ComponentRow>,
    pub driver: DriverAdvice,
    pub telemetry: TelemetryPanel,
    pub notices: Vec<Notice>,
}

/// Kinds the inventory always shows, in display order.
///
/// A kind the producer omitted still gets a row saying so. Silently dropping
/// it reads as "fine", which is the one thing an unknown component is not.
pub const REQUIRED_KINDS: [ComponentKind; 8] = [
    ComponentKind::App,
    ComponentKind::Cli,
    ComponentKind::Driver,
    ComponentKind::SystemHipRocm,
    ComponentKind::ManagedRuntime,
    ComponentKind::Python,
    ComponentKind::PyTorch,
    ComponentKind::Engine,
];

// ---------------------------------------------------------------------------
// Derivation
// ---------------------------------------------------------------------------

/// Build the Overview.
#[must_use]
pub fn overview(
    snapshot: &AppSnapshot,
    telemetry: &TelemetryInput,
    now_unix_ms: u64,
    app_version: Option<&str>,
) -> HealthOverview {
    let freshness = freshness_of(snapshot.observed_at_unix_ms, now_unix_ms);
    HealthOverview {
        verdict: snapshot.health.verdict,
        verdict_label: verdict_label(snapshot.health.verdict).to_owned(),
        summary: summary_for(snapshot),
        next_step: next_step_for(snapshot),
        first_run: snapshot.active_runtime().is_none(),
        headline_facts: headline_facts(snapshot),
        components: components_for(snapshot, app_version, now_unix_ms),
        driver: driver_advice(snapshot),
        telemetry: telemetry_panel(telemetry),
        notices: notices_for(snapshot, telemetry, &freshness),
        freshness,
    }
}

/// Text label for a verdict. Exhaustive by construction.
#[must_use]
pub const fn verdict_label(verdict: HealthVerdict) -> &'static str {
    match verdict {
        HealthVerdict::Healthy => "Ready",
        HealthVerdict::Unknown => "Not sure",
        HealthVerdict::SetupRequired => "Setup needed",
        HealthVerdict::Attention => "Needs attention",
        HealthVerdict::Unsupported => "Not supported",
    }
}

/// Reviewed plain-English copy for every reason the producer can report.
///
/// Keyed by the code, never by the detail string. A producer that rewords its
/// prose changes nothing here; a producer that adds a *code* lands on
/// `Unrecognised` and the app says so rather than guessing.
#[must_use]
pub const fn reason_copy(code: ReasonCode) -> &'static str {
    match code {
        ReasonCode::PlatformWsl => {
            "This is running inside WSL, which cannot reach the graphics card the way ROCm App needs."
        }
        ReasonCode::PlatformUnsupportedOs => "ROCm App manages ROCm on Windows and Linux only.",
        ReasonCode::GpuAbsent => "No AMD graphics card was found on this computer.",
        ReasonCode::GpuUnrecognisedFamily => {
            "This graphics card does not match a supported ROCm build."
        }
        ReasonCode::RuntimeAbsent => "ROCm is not set up on this computer yet.",
        ReasonCode::RuntimeValidationFailed => "The ROCm version in use did not pass its check.",
        ReasonCode::RuntimeActiveMissing => {
            "The ROCm version marked as active is no longer on this computer."
        }
        ReasonCode::RuntimeAmbiguousSelection => "More than one ROCm version claims to be active.",
        ReasonCode::DriverNotDetected => {
            "No AMD display driver was detected. ROCm needs one, and ROCm App does not install it."
        }
        ReasonCode::UpdateAvailable => "A newer ROCm version is available.",
        ReasonCode::UpdateMetadataUntrusted => {
            "AMD's download list could not be verified, so it was not used."
        }
        ReasonCode::UpdateOffline => "This computer could not reach AMD to check for updates.",
        ReasonCode::ProbeIncomplete => "Some checks did not finish, so this view is incomplete.",
        ReasonCode::Unrecognised => {
            "This version of ROCm App does not recognise what the ROCm tool reported."
        }
    }
}

/// The headline sentence.
///
/// The first reason when there is one; otherwise a statement of the verdict.
/// Both come from typed fields.
fn summary_for(snapshot: &AppSnapshot) -> String {
    snapshot.health.reasons.first().map_or_else(
        || match snapshot.health.verdict {
            HealthVerdict::Healthy => "ROCm is set up and working.".to_owned(),
            HealthVerdict::Unknown => {
                "ROCm App could not finish checking this computer.".to_owned()
            }
            HealthVerdict::SetupRequired => "ROCm is not set up on this computer yet.".to_owned(),
            HealthVerdict::Attention => {
                "Something about this ROCm setup needs attention.".to_owned()
            }
            HealthVerdict::Unsupported => {
                "ROCm App cannot manage ROCm on this computer.".to_owned()
            }
        },
        |reason| reason_copy(reason.code).to_owned(),
    )
}

/// The one thing to do next.
///
/// Derived from the typed reason plus the actions the backend says are
/// eligible — never from `health.next_action`, which is producer prose.
fn next_step_for(snapshot: &AppSnapshot) -> NextStep {
    let offerable = snapshot.offerable_actions();
    let offers = |action: EligibleAction| offerable.contains(&action);
    let step = |label: &str, action: Option<EligibleAction>| NextStep {
        label: label.to_owned(),
        action,
    };

    let primary = snapshot.health.reasons.first().map(|r| r.code);
    match primary {
        Some(ReasonCode::PlatformWsl | ReasonCode::PlatformUnsupportedOs) => {
            step("No setup is available on this system", None)
        }
        Some(ReasonCode::GpuAbsent | ReasonCode::GpuUnrecognisedFamily) => {
            step("Check this computer again", None)
        }
        Some(ReasonCode::RuntimeAbsent) if offers(EligibleAction::InstallRuntime) => {
            step("Set up ROCm", Some(EligibleAction::InstallRuntime))
        }
        Some(ReasonCode::UpdateAvailable) if offers(EligibleAction::UpdateRuntime) => {
            step("Update ROCm", Some(EligibleAction::UpdateRuntime))
        }
        Some(ReasonCode::RuntimeValidationFailed) if offers(EligibleAction::ValidateRuntime) => {
            step(
                "Check the ROCm version in use",
                Some(EligibleAction::ValidateRuntime),
            )
        }
        Some(ReasonCode::RuntimeActiveMissing | ReasonCode::RuntimeAmbiguousSelection)
            if offers(EligibleAction::ActivateRuntime) =>
        {
            step(
                "Choose which ROCm to use",
                Some(EligibleAction::ActivateRuntime),
            )
        }
        Some(ReasonCode::UpdateOffline | ReasonCode::UpdateMetadataUntrusted) => {
            step("Check again", None)
        }
        Some(ReasonCode::DriverNotDetected) => step("See AMD's driver guidance", None),
        // Guarded arms do not count towards exhaustiveness, so this is a
        // catch-all rather than the list of remaining codes: it also absorbs
        // a reason whose matching action the backend declined to offer.
        _ => {
            if snapshot.health.verdict != HealthVerdict::Healthy
                && snapshot.runtimes.is_empty()
                && offers(EligibleAction::InstallRuntime)
            {
                step("Set up ROCm", Some(EligibleAction::InstallRuntime))
            } else {
                step("Check this computer again", None)
            }
        }
    }
}

/// The first-viewport identity rows.
fn headline_facts(snapshot: &AppSnapshot) -> Vec<Fact> {
    let fact = |key: &str, label: &str, value: String| Fact {
        key: key.to_owned(),
        label: label.to_owned(),
        value,
    };
    let active = snapshot.active_runtime();
    vec![
        fact(
            "gpu",
            "Graphics card",
            snapshot
                .gpu
                .name
                .clone()
                .unwrap_or_else(|| "No AMD graphics card found".to_owned()),
        ),
        fact("system", "System", system_label(snapshot)),
        fact(
            "rocm",
            "ROCm in use",
            active.map_or_else(
                || "None yet".to_owned(),
                |runtime| match runtime.validation {
                    RuntimeValidation::Ready => runtime.version.clone(),
                    RuntimeValidation::Failed { .. } => {
                        format!("{} — failed its check", runtime.version)
                    }
                    RuntimeValidation::Unvalidated => {
                        format!("{} — not checked yet", runtime.version)
                    }
                    RuntimeValidation::Unrecognised => {
                        format!("{} — state not recognised", runtime.version)
                    }
                },
            ),
        ),
    ]
}

fn system_label(snapshot: &AppSnapshot) -> String {
    use crate::contract::OsFamily;
    let os = match snapshot.platform.os {
        OsFamily::Windows => "Windows",
        OsFamily::Linux => "Linux",
        OsFamily::Other => "This system",
    };
    let width = match snapshot.platform.arch.as_str() {
        "x86_64" | "amd64" | "aarch64" | "arm64" => "64-bit",
        other => other,
    };
    if snapshot.platform.is_wsl {
        format!("{os}, {width} (WSL)")
    } else {
        format!("{os}, {width}")
    }
}

fn freshness_of(observed_at_unix_ms: u64, now_unix_ms: u64) -> Freshness {
    let age_ms = now_unix_ms.saturating_sub(observed_at_unix_ms);
    Freshness {
        observed_at_unix_ms,
        age_ms,
        stale: age_ms > FRESHNESS_TTL_MS,
        label: age_label(age_ms),
    }
}

/// Humanized age wording. Each unit rolls into the next before the count
/// stops reading as a duration: "Last checked 4995 hours ago" reads as a bug,
/// not an age — nobody counts hours past two days, and past two weeks the
/// exact count stops mattering at all.
fn age_label(age_ms: u64) -> String {
    if age_ms < JUST_NOW {
        return "Checked just now".to_owned();
    }
    format!("Last checked {}", age_phrase(age_ms))
}

// Bucket boundaries shared by `age_label` and `age_phrase`.
const MINUTE: u64 = 60 * 1_000;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const JUST_NOW: u64 = 90 * 1_000;
const MINUTES_UNTIL: u64 = HOUR;
const HOURS_UNTIL: u64 = 48 * HOUR;
const DAYS_UNTIL: u64 = 14 * DAY;

/// The bare "<n> unit(s) ago" phrase, shared by the freshness line and the
/// stale-component note so the two can never disagree about buckets.
fn age_phrase(age_ms: u64) -> String {
    match age_ms {
        0..JUST_NOW => "just now".to_owned(),
        JUST_NOW..MINUTES_UNTIL => {
            let minutes = age_ms / MINUTE;
            format!("{minutes} minute{} ago", if minutes == 1 { "" } else { "s" })
        }
        MINUTES_UNTIL..HOURS_UNTIL => {
            let hours = age_ms / HOUR;
            format!("{hours} hour{} ago", if hours == 1 { "" } else { "s" })
        }
        HOURS_UNTIL..DAYS_UNTIL => {
            let days = age_ms / DAY;
            format!("{days} day{} ago", if days == 1 { "" } else { "s" })
        }
        _ => "more than two weeks ago".to_owned(),
    }
}

/// One row per required kind, plus anything extra the producer reported.
fn components_for(
    snapshot: &AppSnapshot,
    app_version: Option<&str>,
    now_unix_ms: u64,
) -> Vec<ComponentRow> {
    let mut consumed = vec![false; snapshot.components.len()];
    let mut rows = Vec::with_capacity(REQUIRED_KINDS.len());

    for kind in REQUIRED_KINDS {
        // Two kinds the CLI cannot answer for, and does not try to:
        //
        // - the desktop app's own version, which only this process knows;
        // - the display driver, which the contract already reports in its own
        //   top-level block. Reading it from there keeps one source of truth
        //   instead of a component row that can disagree with the driver card
        //   three sections below it.
        if kind == ComponentKind::App && app_version.is_some() {
            rows.push(row_for(
                kind,
                "",
                &ComponentState::Installed {
                    version: app_version.unwrap_or_default().to_owned(),
                },
                now_unix_ms,
            ));
            if let Some(index) = snapshot.components.iter().position(|c| c.kind == kind) {
                consumed[index] = true;
            }
            continue;
        }
        if kind == ComponentKind::Driver {
            if let Some(index) = snapshot.components.iter().position(|c| c.kind == kind) {
                consumed[index] = true;
            }
            rows.push(driver_row(snapshot));
            continue;
        }
        if let Some(index) = snapshot.components.iter().position(|c| c.kind == kind) {
            consumed[index] = true;
            let component = &snapshot.components[index];
            rows.push(row_for(kind, &component.name, &component.state, now_unix_ms));
        } else {
            rows.push(missing_row(kind));
        }
    }

    // Anything beyond one row per required kind — a second engine, or a kind a
    // newer producer added — is appended rather than dropped. Dropping it
    // reads as "not present", which is not what "not understood" means.
    for (index, component) in snapshot.components.iter().enumerate() {
        if !consumed[index] {
            rows.push(row_for(component.kind, &component.name, &component.state, now_unix_ms));
        }
    }
    rows
}

/// The driver inventory row, read from the contract's own driver block.
fn driver_row(snapshot: &AppSnapshot) -> ComponentRow {
    use crate::contract::DriverVersionState;
    let (status, value, note) = match &snapshot.driver.installed {
        DriverVersionState::Known { version } => (ComponentStatus::Ready, version.clone(), None),
        DriverVersionState::DetectedWithoutVersion { detail } => (
            ComponentStatus::Ready,
            "Installed".to_owned(),
            Some(detail.clone()),
        ),
        DriverVersionState::NotDetected { detail } => (
            ComponentStatus::NotInstalled,
            "Not detected".to_owned(),
            Some(detail.clone()),
        ),
        DriverVersionState::Unknown { reason } => (
            ComponentStatus::Unknown,
            "Could not be checked".to_owned(),
            Some(reason.clone()),
        ),
        DriverVersionState::Unrecognised => {
            (ComponentStatus::Unknown, "Not recognised".to_owned(), None)
        }
    };
    ComponentRow {
        kind: ComponentKind::Driver,
        label: kind_label(ComponentKind::Driver).to_owned(),
        value,
        status,
        status_label: status.label().to_owned(),
        note,
    }
}

const fn kind_label(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::App => "ROCm App",
        ComponentKind::Cli => "ROCm command-line tool",
        ComponentKind::Driver => "Display driver",
        ComponentKind::SystemHipRocm => "System ROCm",
        ComponentKind::ManagedRuntime => "Managed ROCm",
        ComponentKind::Python => "Python",
        ComponentKind::PyTorch => "PyTorch",
        ComponentKind::Engine => "Model engine",
        ComponentKind::Unrecognised => "Other",
    }
}

/// A kind the producer did not mention at all.
fn missing_row(kind: ComponentKind) -> ComponentRow {
    ComponentRow {
        kind,
        label: kind_label(kind).to_owned(),
        value: "Not reported".to_owned(),
        status: ComponentStatus::Unknown,
        status_label: ComponentStatus::Unknown.label().to_owned(),
        note: Some("The ROCm tool did not report this.".to_owned()),
    }
}

fn row_for(
    kind: ComponentKind,
    name: &str,
    state: &ComponentState,
    now_unix_ms: u64,
) -> ComponentRow {
    let (status, value, note) = match state {
        ComponentState::LatestCompatible { version } => (
            ComponentStatus::Ready,
            version.clone(),
            Some("Newest version that works with this setup.".to_owned()),
        ),
        ComponentState::Installed { version } => (ComponentStatus::Ready, version.clone(), None),
        ComponentState::UpdateAvailable { installed, latest } => (
            ComponentStatus::UpdateAvailable,
            installed.clone(),
            Some(format!("Version {latest} is available.")),
        ),
        ComponentState::Unsupported { version, reason } => (
            ComponentStatus::Unsupported,
            version.clone(),
            Some(reason.clone()),
        ),
        ComponentState::NotInstalled => (
            ComponentStatus::NotInstalled,
            "Not installed".to_owned(),
            None,
        ),
        ComponentState::Stale {
            version,
            checked_at_unix_ms,
        } => (
            ComponentStatus::Stale,
            version.clone().unwrap_or_else(|| "Unknown".to_owned()),
            // Humanized like the freshness line: raw epoch milliseconds on
            // the Overview read as a bug, not a time.
            Some(format!(
                "Last read {} and not re-checked since.",
                age_phrase(now_unix_ms.saturating_sub(*checked_at_unix_ms))
            )),
        ),
        ComponentState::Unknown { reason } => (
            ComponentStatus::Unknown,
            "Could not be checked".to_owned(),
            Some(reason.clone()),
        ),
        ComponentState::Unrecognised => (
            ComponentStatus::Unknown,
            "Not recognised".to_owned(),
            Some("This version of ROCm App does not understand what was reported.".to_owned()),
        ),
    };
    ComponentRow {
        kind,
        // The producer's own slug is appended only for kinds that can repeat,
        // where it is the only thing telling two rows apart. Everywhere else
        // it is backend jargon on a first-view screen: "PyTorch (torch)".
        label: match kind {
            ComponentKind::Engine | ComponentKind::Unrecognised if !name.trim().is_empty() => {
                format!("{} ({name})", kind_label(kind))
            }
            _ => kind_label(kind).to_owned(),
        },
        value,
        status,
        status_label: status.label().to_owned(),
        note,
    }
}

const fn failure_reason(failure: TelemetryFailure) -> &'static str {
    match failure {
        TelemetryFailure::NoDevice => "No AMD graphics card to read.",
        TelemetryFailure::Permission => {
            "ROCm App is not allowed to read the graphics card on this computer."
        }
        TelemetryFailure::Unsupported => "Live readings are not available on this computer.",
        TelemetryFailure::Timeout => "The graphics card did not answer in time.",
        TelemetryFailure::Error => "The graphics card readings could not be taken.",
    }
}

/// Build the metrics panel, one row at a time.
///
/// Each row is derived independently, so a collector that reports temperature
/// but not power produces one reading and one explicit "not reported" rather
/// than an empty panel.
fn telemetry_panel(input: &TelemetryInput) -> TelemetryPanel {
    let blanket = input.failure.map(failure_reason);
    let unavailable = |reason: &str| MetricValue::Unavailable {
        reason: blanket.unwrap_or(reason).to_owned(),
    };
    let sample = input.sample.as_ref();

    let utilization = sample.and_then(|s| s.utilization_pct).map_or_else(
        || unavailable("Not reported by this graphics card."),
        |pct| MetricValue::Reading {
            text: format!("{}%", pct.round()),
            ratio: Some((pct / 100.0).clamp(0.0, 1.0)),
        },
    );
    let vram = match (
        sample.and_then(|s| s.vram_used_mb),
        sample.and_then(|s| s.vram_total_mb),
    ) {
        (Some(used), Some(total)) if total > 0 => MetricValue::Reading {
            text: format!(
                "{} of {}",
                format_bytes(used * 1024 * 1024),
                format_bytes(total * 1024 * 1024)
            ),
            #[expect(
                clippy::cast_precision_loss,
                reason = "megabyte counts are far below f32's exact-integer range"
            )]
            ratio: Some((used as f32 / total as f32).clamp(0.0, 1.0)),
        },
        (Some(used), _) => MetricValue::Reading {
            text: format_bytes(used * 1024 * 1024),
            ratio: None,
        },
        _ => unavailable("Not reported by this graphics card."),
    };
    let temperature = sample.and_then(|s| s.temperature_c).map_or_else(
        || unavailable("Not reported by this graphics card."),
        |c| MetricValue::Reading {
            text: format!("{} °C", c.round()),
            ratio: None,
        },
    );
    let power = sample.and_then(|s| s.power_w).map_or_else(
        || unavailable("Not reported by this graphics card."),
        |w| MetricValue::Reading {
            text: format!("{} W", w.round()),
            ratio: None,
        },
    );

    let row = |key: &str, label: &str, value: MetricValue| MetricRow {
        key: key.to_owned(),
        label: label.to_owned(),
        value,
    };
    TelemetryPanel {
        device: sample.map(|s| s.device.clone()).filter(|d| !d.is_empty()),
        metrics: vec![
            row("utilization", "GPU use", utilization),
            row("vram", "Memory in use", vram),
            row("temperature", "Temperature", temperature),
            row("power", "Power", power),
        ],
        history: input.history.clone(),
        failure: blanket.map(str::to_owned),
    }
}

fn notices_for(
    snapshot: &AppSnapshot,
    telemetry: &TelemetryInput,
    freshness: &Freshness,
) -> Vec<Notice> {
    let mut notices = Vec::new();
    let mut push = |code: NoticeCode, message: &str| {
        notices.push(Notice {
            code,
            message: message.to_owned(),
        });
    };

    if !matches!(snapshot.platform.support, SupportStatus::Supported) || snapshot.platform.is_wsl {
        push(
            NoticeCode::Unsupported,
            "ROCm App cannot change anything on this computer. Everything below is read-only.",
        );
    }
    if snapshot
        .health
        .reasons
        .iter()
        .any(|r| r.code == ReasonCode::ProbeIncomplete)
    {
        push(
            NoticeCode::PartialProbe,
            "Some checks did not finish, so parts of this page may be missing.",
        );
    }
    if matches!(snapshot.update.state, UpdateState::Offline { .. }) {
        push(
            NoticeCode::Offline,
            "This computer could not reach AMD, so update information may be out of date.",
        );
    }
    if matches!(snapshot.update.state, UpdateState::UntrustedMetadata { .. })
        || matches!(snapshot.update.trust, SourceTrust::Untrusted { .. })
    {
        push(
            NoticeCode::UntrustedMetadata,
            "AMD's download list could not be verified, so it was not used.",
        );
    }
    if telemetry.failure == Some(TelemetryFailure::Permission) {
        push(
            NoticeCode::TelemetryPermission,
            "Live graphics readings need permission to read the GPU device on this computer.",
        );
    }
    if freshness.stale {
        push(
            NoticeCode::Stale,
            "This information is more than a few minutes old.",
        );
    }
    notices
}

#[cfg(test)]
mod tests;
