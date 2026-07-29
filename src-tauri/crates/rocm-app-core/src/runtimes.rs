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
use version_compare::{Cmp, Version};

use crate::contract::{
    AppSnapshot, AvailableVersionsState, EligibleAction, InstallSource, LegacyRocmInstall,
    LegacyRocmOrigin, RuntimeRecord, RuntimeValidation, SourceTrust, UpdateState, VersionTier,
};
use crate::controller::request::{
    Channel, OperationRequest, RuntimeFamily, RuntimeKey, VersionSelector,
};
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
    /// The pickable versions this machine could get, per the producer's
    /// catalog. Always present; its `state` says how much to trust it.
    pub catalog: CatalogView,
    /// Unmanaged ROCm installs found beside the managed ones, each with
    /// copy-paste removal guidance. Display-only: the app never runs these.
    pub unmanaged: Vec<UnmanagedRow>,
    /// True when this host may be changed at all.
    pub mutable: bool,
}

/// How fresh the "Get another version" list is, in the UI's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogState {
    Fresh,
    Stale,
    Offline,
    /// The producer has never fetched a catalog (old CLI, or a machine that
    /// has not been online yet). The panel explains itself instead of
    /// rendering an empty list.
    NeverFetched,
    /// A freshness state this build does not recognise. Entries still render,
    /// but with a caution line, because unknown freshness is not fresh.
    Unrecognised,
}

/// Which shelf a pickable version sits on. Declaration order is display
/// order: the safe choice first, pre-release shelves after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogTier {
    Stable,
    Beta,
    Nightly,
}

/// Whether a pickable version is already on this machine.
///
/// Derived here by joining the catalog against `runtimes[]` — the contract
/// deliberately carries no such flag (#16), and the renderer never derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogPresence {
    Available,
    Installed,
    Active,
}

/// One version a person could get.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub tier: CatalogTier,
    /// Friendly headline, e.g. "ROCm 7.14.0".
    pub title: String,
    pub version: String,
    pub presence: CatalogPresence,
    /// The exact-version install the Install button would plan. `None` when
    /// the version is already here, or this host may not install at all —
    /// the same rule `RocmController::plan` enforces.
    pub install_request: Option<OperationRequest>,
    /// Advanced identifiers: shown only behind details, never in the headline.
    pub channel: String,
    pub index_url: String,
}

/// The "Get another version" panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogView {
    pub state: CatalogState,
    /// One plain sentence above the list when freshness warrants one.
    pub notice: Option<String>,
    pub checked_at_unix_ms: Option<u64>,
    /// Sorted by tier, safe choice first; producer order within a tier.
    pub entries: Vec<CatalogEntry>,
}

/// One unmanaged ROCm install, with the removal guidance a person can
/// copy into their own terminal.
///
/// The app never executes any of this — no privilege escalation lives in
/// this product (#21). The person runs the commands themselves, then
/// installs a managed version from the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnmanagedRow {
    pub path: String,
    /// Plain-language origin, e.g. "Installed with apt".
    pub origin_label: String,
    pub guidance: RemovalGuidance,
    /// Present only when following the guidance deletes files permanently.
    pub warning: Option<String>,
}

/// What to show a person who wants an unmanaged install gone.
///
/// The load-bearing rule, pinned by tests: `LooseDelete` — the only variant
/// whose copy destroys data — is reachable *only* from a clean `Loose`
/// verdict. Every uncertain, unrecognised, or fact-starved classification
/// falls back to `Diagnostic`, which never removes anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum RemovalGuidance {
    /// Package-owned: the named manager removes exactly these packages.
    Packages {
        package_manager: String,
        commands: Vec<String>,
    },
    /// An unpackaged tree: optional ownership pre-check, then delete the
    /// literal path — never a glob.
    LooseDelete {
        precheck_commands: Vec<String>,
        delete_command: String,
    },
    /// A Windows installer root: no shell command, Settings steps instead.
    WindowsSteps { steps: Vec<String> },
    /// Ownership undetermined: commands that *investigate*, never remove.
    Diagnostic { commands: Vec<String> },
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
        catalog: catalog_for(snapshot),
        unmanaged: snapshot.legacy_rocm.iter().map(unmanaged_row).collect(),
        mutable,
    }
}

/// Quote a path for a copy-paste shell line when it needs it.
///
/// The path ultimately comes from `ROCM_PATH` — user-controlled — and an
/// unquoted space in a `rm -rf` line truncates the target. Plain
/// `/opt/rocm`-shaped paths stay bare so the common copy reads clean.
fn shell_quote(path: &str) -> String {
    let safe = !path.is_empty()
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'+'));
    if safe {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

/// The ownership probes a person can run themselves. Investigate-only.
fn diagnostic_commands(path: &str) -> Vec<String> {
    let quoted = shell_quote(path);
    vec![format!("dpkg -S {quoted}"), format!("rpm -qf {quoted}")]
}

/// Classify one producer-reported unmanaged install into guidance.
///
/// Fail-safe by construction: any combination this function does not fully
/// understand — a package origin with no package names, an rpm frontend it
/// has never heard of, an unrecognised origin — degrades to `Diagnostic`.
fn unmanaged_row(install: &LegacyRocmInstall) -> UnmanagedRow {
    use LegacyRocmOrigin as Origin;

    let path = install.path.clone();
    let diagnostic = |label: &str| {
        (
            label.to_owned(),
            RemovalGuidance::Diagnostic {
                commands: diagnostic_commands(&path),
            },
            None,
        )
    };

    let (origin_label, guidance, warning) = match install.origin {
        Origin::Deb | Origin::Rpm if install.packages.is_empty() => {
            // A package origin without package names cannot build a removal
            // command worth trusting. Investigate instead.
            diagnostic("Installed from system packages")
        }
        Origin::Deb => {
            let packages = install.packages.join(" ");
            (
                "Installed with apt".to_owned(),
                RemovalGuidance::Packages {
                    package_manager: "apt".to_owned(),
                    commands: vec![
                        format!("sudo apt purge {packages}"),
                        "sudo apt autoremove".to_owned(),
                    ],
                },
                None,
            )
        }
        Origin::Rpm => match install.package_manager.as_deref() {
            Some(pm @ ("dnf" | "zypper")) => {
                let packages = install.packages.join(" ");
                (
                    format!("Installed with {pm}"),
                    RemovalGuidance::Packages {
                        package_manager: pm.to_owned(),
                        commands: vec![format!("sudo {pm} remove {packages}")],
                    },
                    None,
                )
            }
            // An rpm system whose frontend this build does not know: a
            // guessed command would be wrong on exactly that system.
            _ => diagnostic("Installed from system packages"),
        },
        Origin::Loose => {
            let quoted = shell_quote(&path);
            (
                "Unpackaged files".to_owned(),
                RemovalGuidance::LooseDelete {
                    precheck_commands: diagnostic_commands(&path),
                    delete_command: format!("sudo rm -rf {quoted}"),
                },
                Some(format!(
                    "This permanently deletes everything under {path}. \
                     Run the check above first and make sure nothing else lives there."
                )),
            )
        }
        Origin::Windows => (
            "Installed by the ROCm installer".to_owned(),
            RemovalGuidance::WindowsSteps {
                steps: vec![
                    "Open Settings, then Apps, then Installed apps.".to_owned(),
                    "Find the entry named \"AMD ROCm\" or \"AMD HIP SDK\".".to_owned(),
                    "Choose Uninstall and follow the prompts.".to_owned(),
                ],
            },
            None,
        ),
        Origin::Unknown | Origin::Unrecognised => {
            diagnostic("Could not determine how it was installed")
        }
    };

    UnmanagedRow {
        path,
        origin_label,
        guidance,
        warning,
    }
}

/// Build the "Get another version" panel from the producer's catalog.
///
/// Guarded the same way twice, per the module doc: an entry only carries an
/// `install_request` when `RocmController::plan` would accept it — this host
/// may install, the action is offered, and the version is not already here.
fn catalog_for(snapshot: &AppSnapshot) -> CatalogView {
    let Some(available) = &snapshot.available_versions else {
        return CatalogView {
            state: CatalogState::NeverFetched,
            notice: None,
            checked_at_unix_ms: None,
            entries: Vec::new(),
        };
    };

    let state = match available.state {
        AvailableVersionsState::Fresh => CatalogState::Fresh,
        AvailableVersionsState::Stale => CatalogState::Stale,
        AvailableVersionsState::Offline => CatalogState::Offline,
        AvailableVersionsState::Unrecognised => CatalogState::Unrecognised,
    };
    let notice = match state {
        CatalogState::Fresh | CatalogState::NeverFetched => None,
        CatalogState::Stale => {
            Some("This list was checked a while ago and may be missing newer versions.".to_owned())
        }
        CatalogState::Offline => Some(
            "This computer could not reach AMD to refresh the version list. \
             Showing the last one it saw."
                .to_owned(),
        ),
        CatalogState::Unrecognised => {
            Some("This version of ROCm App does not recognise how fresh this list is.".to_owned())
        }
    };

    let installable = snapshot.platform.install_allowed()
        && snapshot
            .offerable_actions()
            .contains(&EligibleAction::InstallRuntime);
    // The same allowlist the controller enforces; a family it refuses must
    // not become a request the UI offers.
    let family = snapshot
        .gpu
        .therock_family
        .clone()
        .and_then(|f| RuntimeFamily::new(f).ok());

    let mut entries: Vec<CatalogEntry> = available
        .entries
        .iter()
        .filter_map(|entry| {
            let tier = match entry.tier {
                VersionTier::Stable => CatalogTier::Stable,
                VersionTier::Beta => CatalogTier::Beta,
                VersionTier::Nightly => CatalogTier::Nightly,
                // A shelf this build cannot explain must not grow an
                // Install button; dropping the entry fails closed.
                VersionTier::Unrecognised => return None,
            };
            // The channel becomes an argv element; only the closed set passes.
            let channel = match entry.channel.as_str() {
                "release" => Channel::Release,
                "nightly" => Channel::Nightly,
                _ => return None,
            };
            let presence = match snapshot
                .runtimes
                .iter()
                .find(|r| r.version == entry.version)
            {
                Some(record) if record.active => CatalogPresence::Active,
                Some(_) => CatalogPresence::Installed,
                None => CatalogPresence::Available,
            };
            let install_request = (installable && presence == CatalogPresence::Available)
                .then_some(())
                .zip(family.clone())
                .map(|((), family)| OperationRequest::InstallRuntime {
                    channel,
                    family,
                    version: VersionSelector::Exact {
                        version: entry.version.clone(),
                    },
                    // rocm-cli's own default folder; onboarding names one
                    // because first run reviews it, the picker does not.
                    install_root: None,
                });
            Some(CatalogEntry {
                tier,
                title: format!("ROCm {}", entry.version),
                version: entry.version.clone(),
                presence,
                install_request,
                channel: entry.channel.clone(),
                index_url: entry.index_url.clone(),
            })
        })
        .collect();
    entries.sort_by_key(|entry| entry.tier);

    CatalogView {
        state,
        notice,
        checked_at_unix_ms: available.checked_at_unix_ms,
        entries,
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

/// Derive update standing from the trusted report state and current catalog.
///
/// The report still owns trust and freshness. For a current answer, the
/// catalog supplies the channel-scoped ceiling so a stable install is never
/// nudged toward beta.
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
        UpdateState::Offline { detail } => {
            return UpdateStanding::Offline {
                detail: detail.clone(),
            };
        }
        UpdateState::Stale {
            installed,
            checked_at_unix_ms,
        } => {
            return UpdateStanding::Stale {
                installed: installed.clone(),
                checked_at_unix_ms: *checked_at_unix_ms,
            };
        }
        UpdateState::UntrustedMetadata { detail } => {
            return UpdateStanding::Untrusted {
                detail: detail.clone(),
            };
        }
        UpdateState::NotApplicable => return UpdateStanding::NotApplicable,
        UpdateState::Unrecognised => return UpdateStanding::Unrecognised,
        UpdateState::NoUpdate { .. }
        | UpdateState::Available { .. }
        | UpdateState::AheadOfIndex { .. } => {}
    }

    let Some(available) = &snapshot.available_versions else {
        // Schema v1 made the catalog additive. Keep older CLI snapshots useful
        // when they cannot provide the tier information needed below.
        return match &snapshot.update.state {
            UpdateState::NoUpdate { installed } => UpdateStanding::UpToDate {
                installed: installed.clone(),
            },
            UpdateState::Available { installed, latest } => UpdateStanding::Available {
                installed: installed.clone(),
                latest: latest.clone(),
            },
            UpdateState::AheadOfIndex { installed, latest } => UpdateStanding::AheadOfIndex {
                installed: installed.clone(),
                latest: latest.clone(),
            },
            _ => unreachable!("terminal update states returned above"),
        };
    };
    let Some(active) = snapshot.active_runtime() else {
        return UpdateStanding::NotApplicable;
    };
    let tier = match active.channel.as_str() {
        "release" => VersionTier::Stable,
        "nightly" => VersionTier::Nightly,
        _ => return UpdateStanding::Unrecognised,
    };

    let mut latest: Option<(&str, Version<'_>)> = None;
    for entry in available.entries.iter().filter(|entry| entry.tier == tier) {
        let Some(candidate) = Version::from(&entry.version) else {
            return UpdateStanding::Unrecognised;
        };
        if latest
            .as_ref()
            .is_none_or(|(_, current)| candidate.compare(current) == Cmp::Gt)
        {
            latest = Some((&entry.version, candidate));
        }
    }
    let Some((latest, latest_version)) = latest else {
        return UpdateStanding::Unrecognised;
    };
    let Some(installed_version) = Version::from(&active.version) else {
        return UpdateStanding::Unrecognised;
    };

    match installed_version.compare(&latest_version) {
        Cmp::Lt => match incompatible_family(snapshot) {
            Some(built_for) => UpdateStanding::Incompatible {
                latest: latest.to_owned(),
                built_for,
            },
            None => UpdateStanding::Available {
                installed: active.version.clone(),
                latest: latest.to_owned(),
            },
        },
        Cmp::Eq => UpdateStanding::UpToDate {
            installed: active.version.clone(),
        },
        Cmp::Gt => UpdateStanding::AheadOfIndex {
            installed: active.version.clone(),
            latest: latest.to_owned(),
        },
        _ => unreachable!("version comparison returns only ordering variants"),
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
