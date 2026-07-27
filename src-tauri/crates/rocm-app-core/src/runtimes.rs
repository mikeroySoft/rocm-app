// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! ROCm Installs: which versions are here, which one is in use, and what may
//! safely be done to each.
//!
//! # Two places, one rule
//!
//! Every guard in this module exists twice on purpose:
//!
//! - here, as a pure function, so the UI never draws a control that would be
//!   refused; and
//! - in [`crate::controller`], as a refusal at `plan` time, so nothing that
//!   bypasses the UI can reach the CLI either.
//!
//! The pair is what makes "rejected before mutation" true rather than merely
//! displayed. A UI-only guard is decoration; a backend-only guard leaves a
//! live-looking button that fails on click.
//!
//! # No driver lifecycle
//!
//! Nothing here targets a driver, and [`crate::controller::request`] has no
//! variant that could. The driver stays a report-only row on the Overview.

use serde::{Deserialize, Serialize};

use crate::contract::{
    AppSnapshot, EligibleAction, InstallSource, RuntimeRecord, RuntimeValidation, SourceTrust,
    UpdateState,
};
use crate::controller::request::{OperationRequest, RuntimeKey};
use crate::onboarding::format_bytes;

/// What may be done to one installed version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RowAction {
    Activate,
    Remove,
    Validate,
    Update,
}

/// Why an action is not offered.
///
/// Every variant is a refusal a user can understand and, where possible, undo.
/// "Unavailable" with no reason is what makes a greyed-out button infuriating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockReason {
    /// This is the version currently in use.
    Active,
    /// Kept as the version to fall back to.
    Previous,
    /// Installed somewhere this app must not delete, or marked read-only.
    Protected,
    /// More than one installed version answers to this identity.
    Ambiguous,
    /// The app does not know enough about this install to touch it.
    Unknown,
    /// Not checked yet, or the check failed.
    Unvalidated,
    /// The host itself cannot be modified.
    UnsupportedHost,
    /// The backend did not offer this operation for this machine.
    NotOffered,
}

impl BlockReason {
    /// Plain-language explanation. Shown in place of the missing control.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Active => "This is the version ROCm is using now.",
            Self::Previous => {
                "This is the version ROCm falls back to. Choose another version first."
            }
            Self::Protected => "This version was not installed by ROCm App.",
            Self::Ambiguous => {
                "More than one installed version answers to this name, so ROCm App will not guess."
            }
            Self::Unknown => "ROCm App does not have enough information about this version.",
            Self::Unvalidated => "This version has not passed its check yet.",
            Self::UnsupportedHost => "ROCm App cannot change anything on this computer.",
            Self::NotOffered => "The ROCm tool did not offer this on this computer.",
        }
    }
}

/// An action the row does not offer, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedAction {
    pub action: RowAction,
    pub reason: BlockReason,
}

/// Whether this version matches the graphics card in this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum Compatibility {
    /// Built for this card's family.
    Matches,
    /// Built for a different family. Still listed; never offered for use.
    Mismatched { built_for: String },
    /// The card, or the version's family, could not be identified.
    Unknown,
}

/// How a version reports itself after a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckState {
    Passed,
    Failed,
    NotChecked,
    Unrecognised,
}

impl CheckState {
    /// Text form. Colour never carries this alone.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Passed => "Working",
            Self::Failed => "Failed its check",
            Self::NotChecked => "Not checked yet",
            Self::Unrecognised => "State not recognised",
        }
    }
}

/// One installed ROCm version, as a person should see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRow {
    /// Friendly headline, e.g. "ROCm 7.14.0".
    pub title: String,
    pub version: String,
    /// "In use", "Previous", and similar. Text, never a bare colour.
    pub badges: Vec<String>,
    pub compatibility: Compatibility,
    pub check: CheckState,
    pub check_label: String,
    /// Formatted disk usage, when the host could measure it.
    pub disk: Option<String>,
    pub actions: Vec<RowAction>,
    pub blocked: Vec<BlockedAction>,
    /// Everything below is an advanced identifier: shown only behind details.
    pub key: String,
    pub channel: String,
    pub family: String,
    pub format: String,
    pub install_root: String,
    pub source: String,
}

/// Where an update stands, in the words the UI uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum UpdateStanding {
    /// Nothing newer, and the check was trustworthy.
    UpToDate { installed: String },
    /// A newer version exists and may be installed side by side.
    Available { installed: String, latest: String },
    /// Installed is newer than anything the index knows about.
    AheadOfIndex { installed: String, latest: String },
    /// The check could not reach AMD.
    Offline { detail: String },
    /// The last answer is old enough not to be trusted as current.
    Stale {
        installed: String,
        checked_at_unix_ms: u64,
    },
    /// The download list did not verify.
    Untrusted { detail: String },
    /// A newer version exists but is not built for this graphics card.
    Incompatible { latest: String, built_for: String },
    /// Nothing is installed yet, so there is nothing to update.
    NotApplicable,
    /// A state this build does not recognise.
    Unrecognised,
}

impl UpdateStanding {
    /// One sentence, in plain language.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::UpToDate { installed } => format!("ROCm {installed} is the newest version."),
            Self::Available { installed, latest } => {
                format!("ROCm {latest} is available. You have {installed}.")
            }
            Self::AheadOfIndex { installed, latest } => format!(
                "You have ROCm {installed}, which is newer than the {latest} AMD currently lists."
            ),
            Self::Offline { .. } => {
                "This computer could not reach AMD to check for a newer version.".to_owned()
            }
            Self::Stale { installed, .. } => format!(
                "The last check said ROCm {installed} was current, but that answer is old now."
            ),
            Self::Untrusted { .. } => {
                "AMD's download list could not be verified, so it was not used.".to_owned()
            }
            Self::Incompatible { latest, built_for } => format!(
                "ROCm {latest} is available but is built for {built_for}, not your graphics card."
            ),
            Self::NotApplicable => "ROCm is not set up on this computer yet.".to_owned(),
            Self::Unrecognised => {
                "This version of ROCm App does not recognise what the ROCm tool reported."
                    .to_owned()
            }
        }
    }

    /// Whether an update is worth offering. Only one state qualifies: an
    /// offline or unverified answer is not evidence that an update exists.
    #[must_use]
    pub const fn offers_update(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
}

/// The whole ROCm Installs view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimesView {
    pub rows: Vec<RuntimeRow>,
    pub update: UpdateStanding,
    pub update_message: String,
    /// The request the Update button would plan, when there is one.
    pub update_request: Option<OperationRequest>,
    /// True when this host may be changed at all.
    pub mutable: bool,
}

/// Disk usage the host measured, keyed by install root.
pub type DiskUsage = std::collections::BTreeMap<String, u64>;

/// Build the ROCm Installs view.
#[must_use]
pub fn view(snapshot: &AppSnapshot, disk: &DiskUsage) -> RuntimesView {
    let mutable = snapshot.platform.install_allowed();
    let update = standing_for(snapshot);
    let update_request = update
        .offers_update()
        .then(|| snapshot.active_runtime())
        .flatten()
        .filter(|_| {
            mutable
                && snapshot
                    .offerable_actions()
                    .contains(&EligibleAction::UpdateRuntime)
        })
        .and_then(|active| RuntimeKey::new(active.key.clone()).ok())
        .map(|key| OperationRequest::UpdateRuntime { key });

    RuntimesView {
        rows: snapshot
            .runtimes
            .iter()
            .map(|runtime| row_for(snapshot, runtime, disk))
            .collect(),
        update_message: update.message(),
        update,
        update_request,
        mutable,
    }
}

fn row_for(snapshot: &AppSnapshot, runtime: &RuntimeRecord, disk: &DiskUsage) -> RuntimeRow {
    let check = check_state(&runtime.validation);
    let mut badges = Vec::new();
    if runtime.active {
        badges.push("In use".to_owned());
    }
    if runtime.previous {
        badges.push("Previous".to_owned());
    }
    if runtime.read_only {
        badges.push("Read only".to_owned());
    }

    let mut actions = Vec::new();
    let mut blocked = Vec::new();
    let mut offer = |action: RowAction, block: Option<BlockReason>| match block {
        None => actions.push(action),
        Some(reason) => blocked.push(BlockedAction { action, reason }),
    };
    offer(RowAction::Activate, activate_block(snapshot, runtime));
    offer(RowAction::Remove, remove_block(snapshot, runtime));
    offer(RowAction::Validate, validate_block(snapshot, runtime));

    RuntimeRow {
        title: format!("ROCm {}", runtime.version),
        version: runtime.version.clone(),
        badges,
        compatibility: compatibility_of(snapshot, runtime),
        check,
        check_label: check.label().to_owned(),
        disk: disk
            .get(&runtime.install_root.display().to_string())
            .map(|bytes| format_bytes(*bytes)),
        actions,
        blocked,
        key: runtime.key.clone(),
        channel: runtime.channel.clone(),
        family: runtime.family.clone(),
        format: runtime.format.clone(),
        install_root: runtime.install_root.display().to_string(),
        source: source_label(&runtime.install_source),
    }
}

const fn check_state(validation: &RuntimeValidation) -> CheckState {
    match validation {
        RuntimeValidation::Ready => CheckState::Passed,
        RuntimeValidation::Failed { .. } => CheckState::Failed,
        RuntimeValidation::Unvalidated => CheckState::NotChecked,
        RuntimeValidation::Unrecognised => CheckState::Unrecognised,
    }
}

fn source_label(source: &InstallSource) -> String {
    match source {
        InstallSource::Index { url } => format!("Downloaded from {url}"),
        InstallSource::Tarball { file_name, .. } => format!("Installed from {file_name}"),
        InstallSource::Adopted { path } => format!("Adopted from {}", path.display()),
        InstallSource::Imported { path } => format!("Imported from {}", path.display()),
        InstallSource::Unknown | InstallSource::Unrecognised => "Source not recorded".to_owned(),
    }
}

fn compatibility_of(snapshot: &AppSnapshot, runtime: &RuntimeRecord) -> Compatibility {
    match snapshot.gpu.therock_family.as_deref() {
        None => Compatibility::Unknown,
        Some(_) if runtime.family.trim().is_empty() => Compatibility::Unknown,
        Some(host) if host == runtime.family => Compatibility::Matches,
        Some(_) => Compatibility::Mismatched {
            built_for: runtime.family.clone(),
        },
    }
}

/// Whether more than one installed version answers to the same identity.
///
/// Two records with one key is a state the CLI should never produce, which is
/// exactly why the app must not act on it: something has already gone wrong,
/// and picking one of them at random compounds it.
fn is_ambiguous(snapshot: &AppSnapshot, runtime: &RuntimeRecord) -> bool {
    snapshot
        .runtimes
        .iter()
        .filter(|other| other.key == runtime.key)
        .count()
        > 1
}

/// Whether the app knows enough about this install to touch it.
const fn is_understood(runtime: &RuntimeRecord) -> bool {
    !matches!(
        runtime.install_source,
        InstallSource::Unknown | InstallSource::Unrecognised
    )
}

/// Why activation is not offered, or `None` when it is.
///
/// The load-bearing rule: a version may not be activated until its check has
/// passed. A freshly installed, unvalidated runtime must not become the one
/// the machine uses.
#[must_use]
pub fn activate_block(snapshot: &AppSnapshot, runtime: &RuntimeRecord) -> Option<BlockReason> {
    if !snapshot.platform.install_allowed() {
        return Some(BlockReason::UnsupportedHost);
    }
    if runtime.active {
        return Some(BlockReason::Active);
    }
    if is_ambiguous(snapshot, runtime) {
        return Some(BlockReason::Ambiguous);
    }
    if !matches!(runtime.validation, RuntimeValidation::Ready) {
        return Some(BlockReason::Unvalidated);
    }
    if !snapshot
        .offerable_actions()
        .contains(&EligibleAction::ActivateRuntime)
    {
        return Some(BlockReason::NotOffered);
    }
    None
}

/// Why removal is not offered, or `None` when it is.
#[must_use]
pub fn remove_block(snapshot: &AppSnapshot, runtime: &RuntimeRecord) -> Option<BlockReason> {
    if !snapshot.platform.install_allowed() {
        return Some(BlockReason::UnsupportedHost);
    }
    if runtime.active {
        return Some(BlockReason::Active);
    }
    if runtime.previous {
        return Some(BlockReason::Previous);
    }
    if runtime.read_only {
        return Some(BlockReason::Protected);
    }
    if is_ambiguous(snapshot, runtime) {
        return Some(BlockReason::Ambiguous);
    }
    if !is_understood(runtime) {
        return Some(BlockReason::Unknown);
    }
    if !snapshot
        .offerable_actions()
        .contains(&EligibleAction::RemoveRuntime)
    {
        return Some(BlockReason::NotOffered);
    }
    None
}

/// Why a check is not offered. Checking is read-only, so it is refused only
/// where the record itself is not actionable.
#[must_use]
pub fn validate_block(snapshot: &AppSnapshot, runtime: &RuntimeRecord) -> Option<BlockReason> {
    if is_ambiguous(snapshot, runtime) {
        return Some(BlockReason::Ambiguous);
    }
    if !snapshot
        .offerable_actions()
        .contains(&EligibleAction::ValidateRuntime)
    {
        return Some(BlockReason::NotOffered);
    }
    None
}

/// Find a runtime by key, refusing an ambiguous match.
///
/// Returns `Err(Ambiguous)` rather than the first hit: the whole point of the
/// ambiguity guard is that "pick one" is the wrong answer.
pub fn find(snapshot: &AppSnapshot, key: &str) -> Result<RuntimeRecord, BlockReason> {
    let mut matches = snapshot.runtimes.iter().filter(|r| r.key == key);
    let first = matches.next().ok_or(BlockReason::Unknown)?;
    if matches.next().is_some() {
        return Err(BlockReason::Ambiguous);
    }
    Ok(first.clone())
}

/// Map the contract's update report into the standing the UI shows.
///
/// Compatibility is layered on top: the contract says what version exists, and
/// this decides whether it is one this machine can use. An update offered for
/// the wrong graphics card is worse than no update at all.
#[must_use]
pub fn standing_for(snapshot: &AppSnapshot) -> UpdateStanding {
    // An unverifiable download list is decided first: whatever it *says* about
    // versions is exactly what cannot be trusted.
    if let SourceTrust::Untrusted { reason } = &snapshot.update.trust
        && !matches!(snapshot.update.state, UpdateState::Offline { .. })
    {
        return UpdateStanding::Untrusted {
            detail: reason.clone(),
        };
    }

    match &snapshot.update.state {
        UpdateState::NoUpdate { installed } => UpdateStanding::UpToDate {
            installed: installed.clone(),
        },
        UpdateState::Available { installed, latest } => match incompatible_family(snapshot) {
            Some(built_for) => UpdateStanding::Incompatible {
                latest: latest.clone(),
                built_for,
            },
            None => UpdateStanding::Available {
                installed: installed.clone(),
                latest: latest.clone(),
            },
        },
        UpdateState::AheadOfIndex { installed, latest } => UpdateStanding::AheadOfIndex {
            installed: installed.clone(),
            latest: latest.clone(),
        },
        UpdateState::Offline { detail } => UpdateStanding::Offline {
            detail: detail.clone(),
        },
        UpdateState::Stale {
            installed,
            checked_at_unix_ms,
        } => UpdateStanding::Stale {
            installed: installed.clone(),
            checked_at_unix_ms: *checked_at_unix_ms,
        },
        UpdateState::UntrustedMetadata { detail } => UpdateStanding::Untrusted {
            detail: detail.clone(),
        },
        UpdateState::NotApplicable => UpdateStanding::NotApplicable,
        UpdateState::Unrecognised => UpdateStanding::Unrecognised,
    }
}

/// The family the active runtime is built for, when it is not this card's.
///
/// An update replaces the active runtime with a newer build of the *same*
/// family, so a mismatch between the active runtime's family and the host's is
/// what makes the offered update unusable here.
fn incompatible_family(snapshot: &AppSnapshot) -> Option<String> {
    let host = snapshot.gpu.therock_family.as_deref()?;
    let active = snapshot.active_runtime()?;
    (!active.family.is_empty() && active.family != host).then(|| active.family.clone())
}

#[cfg(test)]
mod tests;
