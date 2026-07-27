// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Guided first-run setup: one deterministic answer per machine.
//!
//! # Why the decision lives here and not in the renderer
//!
//! [`recommend`] is a pure function of a snapshot, the user's choices, and the
//! free space on the chosen drive. That is the whole decision — which ROCm to
//! install, where, and whether anything blocks it. The webview renders the
//! result and never re-derives it, so there is exactly one place where "what
//! should this user do" is answered and exactly one place to test it.
//!
//! # No LLM, no CPU fallback, no driver mutation
//!
//! Every branch below is a `match` on machine state. Nothing here consults a
//! model, and nothing offers a way to run ROCm without the GPU. Driver
//! information is carried as [`DriverAdvice`] — a summary and links, with no
//! action of any kind, because [`OperationRequest`] has no driver variant to
//! build one from.

use serde::{Deserialize, Serialize};

use crate::contract::{
    AppSnapshot, DriverVersionState, HealthVerdict, OsFamily, ReasonCode, SourceTrust, SupportLink,
    SupportStatus, UpdateState,
};
use crate::controller::request::{
    Channel, InstallPath, OperationRequest, RuntimeFamily, VersionSelector,
};

/// Disk space a managed ROCm install needs, including its Python environment.
///
/// ponytail: one conservative constant rather than a per-version size table.
/// A real per-build figure needs the index's package sizes; wire that through
/// the catalog seam if users start hitting a wrong estimate.
pub const ESTIMATED_INSTALL_BYTES: u64 = 12 * 1024 * 1024 * 1024;

/// Headroom above the estimate. An install that exactly fills a disk leaves a
/// machine that cannot boot cleanly, so "just enough" is not enough.
pub const REQUIRED_HEADROOM_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Total free space required before setup is offered.
pub const REQUIRED_FREE_BYTES: u64 = ESTIMATED_INSTALL_BYTES + REQUIRED_HEADROOM_BYTES;

/// One sentence that must be true of every build: the app reports the display
/// driver and never changes it.
pub const DRIVER_READ_ONLY_NOTE: &str =
    "ROCm App only reports your display driver. It never installs, updates, or changes it.";

// ---------------------------------------------------------------------------
// Choices
// ---------------------------------------------------------------------------

/// What the user picked, or the defaults they were given.
///
/// `channel` and `version` are the Advanced options; a first-run user never
/// touches either and gets the stable channel's newest build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Choices {
    pub channel: Channel,
    pub version: VersionSelector,
    pub target_folder: String,
}

impl Choices {
    /// The defaults a fresh install starts from: stable channel, newest build,
    /// a folder inside the user's own home directory.
    #[must_use]
    pub fn recommended() -> Self {
        Self {
            channel: Channel::Release,
            version: VersionSelector::Latest,
            target_folder: default_install_folder(),
        }
    }
}

/// The default install folder: a plainly named directory the user owns.
///
/// Deliberately not a cache, launcher, or tool directory — the UX guidelines
/// call those out as bad suggestions, and a user who later goes looking for
/// their ROCm files should find them somewhere they would have chosen.
#[must_use]
pub fn default_install_folder() -> String {
    folder_choices()
        .into_iter()
        .next()
        .unwrap_or_else(|| "ROCm".to_owned())
}

/// Easy folder choices, best first. The picker cycles these; manual entry
/// stays available for anyone who needs a different disk.
#[must_use]
pub fn folder_choices() -> Vec<String> {
    let Some(home) = rocm_core::runtime_home_dir() else {
        // No home directory is a broken environment, not a supported one. An
        // empty list makes the picker require an explicit path instead of
        // inventing a system location.
        return Vec::new();
    };
    let join = |name: &str| home.join(name).display().to_string();
    vec![join("ROCm"), join("AMD/ROCm"), join("Documents/ROCm")]
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

/// A label/value row on the review screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    /// Stable identifier for tests and styling; never shown.
    pub key: String,
    pub label: String,
    pub value: String,
}

/// Driver information. There is no action here, and there must never be one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverAdvice {
    pub summary: String,
    /// Always [`DRIVER_READ_ONLY_NOTE`]; carried in the payload so the renderer
    /// cannot forget to show it.
    pub note: String,
    /// Links that arrived through trusted, signed metadata. An unsigned or
    /// unreachable source contributes none: a "download your driver here" link
    /// from an unverified source is the one link that must never be wrong.
    pub links: Vec<SupportLink>,
}

/// Why setup cannot start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockerCode {
    UnsupportedWsl,
    UnsupportedPlatform,
    UnknownHardware,
    IncompleteProbe,
    Offline,
    UntrustedMetadata,
    InsufficientSpace,
    ProtectedFolder,
}

/// The single thing the user can do about a blocker.
///
/// One variant, not a list: a blocked screen offering three buttons makes the
/// user pick which of them is the real one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum NextAction {
    /// Check the machine again.
    Refresh { label: String },
    /// Return to the folder step.
    ChooseFolder { label: String },
    /// Free space, then check again.
    FreeSpace {
        label: String,
        needed_bytes: u64,
        available_bytes: u64,
    },
    /// Nothing on this machine will help. Says so instead of pretending.
    Nothing { label: String },
}

/// A refusal, written for a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blocker {
    pub code: BlockerCode,
    pub headline: String,
    pub detail: String,
    pub next_action: NextAction,
}

/// Everything the recommendation and review screens display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recommendation {
    /// Ordered rows: graphics card, system, driver, ROCm, space, folder.
    pub facts: Vec<Fact>,
    pub driver: DriverAdvice,
    /// True when no ROCm version is active yet. The shell uses it to decide
    /// whether onboarding is the screen this user should land on; a machine
    /// that already has ROCm running belongs on the dashboard, even though a
    /// side-by-side install is still perfectly legal from there.
    pub first_run: bool,
    /// Advanced identifiers, kept off the first view. The renderer shows these
    /// only behind Advanced options.
    pub channel: Channel,
    pub family: String,
    pub target_folder: String,
    pub folder_choices: Vec<String>,
    pub estimated_bytes: u64,
    pub available_bytes: Option<u64>,
    /// The request the review step plans. Built here so the button the user
    /// presses and the plan they read describe the same change.
    pub request: OperationRequest,
}

/// What the onboarding flow should show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum OnboardingView {
    Ready { recommendation: Box<Recommendation> },
    Blocked { blocker: Blocker },
}

impl OnboardingView {
    /// The recommendation, when there is one.
    #[must_use]
    pub fn recommendation(&self) -> Option<&Recommendation> {
        match self {
            Self::Ready { recommendation } => Some(recommendation),
            Self::Blocked { .. } => None,
        }
    }

    /// The blocker, when there is one.
    #[must_use]
    pub const fn blocker(&self) -> Option<&Blocker> {
        match self {
            Self::Blocked { blocker } => Some(blocker),
            Self::Ready { .. } => None,
        }
    }

    /// Whether an Install action may be offered at all.
    #[must_use]
    pub const fn offers_install(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

// ---------------------------------------------------------------------------
// The decision
// ---------------------------------------------------------------------------

/// Decide what a user should be shown.
///
/// Checks run cheapest-and-most-fundamental first: a WSL user is told about
/// WSL, not about disk space on a machine they cannot install to anyway.
///
/// `folder_choices` is passed in rather than read from the environment so the
/// whole function is a pure mapping — the generated fixtures would otherwise
/// embed the generating machine's home directory and fail everywhere else.
#[must_use]
pub fn recommend(
    snapshot: &AppSnapshot,
    choices: &Choices,
    available_bytes: Option<u64>,
    folder_choices: &[String],
) -> OnboardingView {
    if let Some(blocker) = platform_blocker(snapshot)
        .or_else(|| probe_blocker(snapshot))
        .or_else(|| metadata_blocker(snapshot))
    {
        return OnboardingView::Blocked { blocker };
    }

    // Unwrapped after `platform_blocker`, which rejects a missing family.
    let Some(family) = snapshot.gpu.therock_family.clone() else {
        return OnboardingView::Blocked {
            blocker: unknown_hardware(),
        };
    };

    let install_root = match InstallPath::new(choices.target_folder.clone()) {
        Ok(root) => root,
        Err(error) => {
            return OnboardingView::Blocked {
                blocker: Blocker {
                    code: BlockerCode::ProtectedFolder,
                    headline: "That folder cannot be used".to_owned(),
                    // The validator's own detail already names the fix for
                    // each case; appending a generic second sentence produced
                    // "choose a folder inside your home folder" twice.
                    detail: format!(
                        "The folder you chose {error_detail}.",
                        error_detail = detail_of(&error)
                    ),
                    next_action: NextAction::ChooseFolder {
                        label: "Choose another folder".to_owned(),
                    },
                },
            };
        }
    };

    if let Some(available) = available_bytes
        && available < REQUIRED_FREE_BYTES
    {
        return OnboardingView::Blocked {
            blocker: Blocker {
                code: BlockerCode::InsufficientSpace,
                headline: "Not enough free space".to_owned(),
                detail: format!(
                    "Setting up ROCm needs about {needed} free, and this drive has {available_text}. Free some space or choose a folder on another drive.",
                    needed = format_bytes(REQUIRED_FREE_BYTES),
                    available_text = format_bytes(available),
                ),
                next_action: NextAction::FreeSpace {
                    label: "Check again".to_owned(),
                    needed_bytes: REQUIRED_FREE_BYTES,
                    available_bytes: available,
                },
            },
        };
    }

    // `RuntimeFamily` is the same allowlist the controller enforces. A family
    // the producer reported but the request type refuses is a contract
    // problem, not something to pass through and fail later at execute.
    let Ok(validated_family) = RuntimeFamily::new(family.clone()) else {
        return OnboardingView::Blocked {
            blocker: unknown_hardware(),
        };
    };

    OnboardingView::Ready {
        recommendation: Box::new(Recommendation {
            facts: facts_for(snapshot, choices, available_bytes),
            driver: driver_advice(snapshot),
            first_run: snapshot.active_runtime().is_none(),
            channel: choices.channel,
            family,
            target_folder: choices.target_folder.clone(),
            folder_choices: folder_choices.to_vec(),
            estimated_bytes: ESTIMATED_INSTALL_BYTES,
            available_bytes,
            request: OperationRequest::InstallRuntime {
                channel: choices.channel,
                family: validated_family,
                version: choices.version.clone(),
                install_root: Some(install_root),
            },
        }),
    }
}

/// Free bytes on the volume that holds `folder`, or the nearest existing
/// ancestor of it.
///
/// The chosen folder usually does not exist yet, so the mount point is found
/// by longest matching prefix rather than by asking the filesystem about a
/// path that is not there. `None` when no mount point matches, which the
/// caller must treat as "unknown", never as "too little".
#[must_use]
pub fn available_bytes_for(folder: &str) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|disk| folder.starts_with(&*disk.mount_point().to_string_lossy()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(sysinfo::Disk::available_space)
}

fn detail_of(error: &crate::controller::request::RequestError) -> String {
    let crate::controller::request::RequestError::Invalid { detail, .. } = error;
    detail.clone()
}

fn unknown_hardware() -> Blocker {
    Blocker {
        code: BlockerCode::UnknownHardware,
        headline: "This graphics card is not recognised".to_owned(),
        detail: "ROCm App could not match your graphics card to a supported ROCm build, so it will not guess. Check that a supported AMD card is installed and its driver is loaded.".to_owned(),
        next_action: NextAction::Refresh {
            label: "Check this computer again".to_owned(),
        },
    }
}

/// Platform and hardware eligibility.
fn platform_blocker(snapshot: &AppSnapshot) -> Option<Blocker> {
    if snapshot.platform.is_wsl
        || matches!(
            snapshot.platform.support,
            SupportStatus::Unsupported {
                reason: ReasonCode::PlatformWsl
            }
        )
    {
        return Some(Blocker {
            code: BlockerCode::UnsupportedWsl,
            headline: "ROCm App cannot set up ROCm inside WSL".to_owned(),
            detail: "You are running inside Windows Subsystem for Linux, which cannot reach the graphics card the way ROCm App needs. Install and run ROCm App on Windows itself.".to_owned(),
            next_action: NextAction::Nothing {
                label: "Close this window and open ROCm App on Windows".to_owned(),
            },
        });
    }
    if !matches!(snapshot.platform.support, SupportStatus::Supported) {
        return Some(Blocker {
            code: BlockerCode::UnsupportedPlatform,
            headline: "This system is not supported".to_owned(),
            detail:
                "ROCm App sets up ROCm on Windows and Linux. It cannot make changes on this system."
                    .to_owned(),
            next_action: NextAction::Nothing {
                label: "No setup is available here".to_owned(),
            },
        });
    }
    if snapshot.gpu.therock_family.is_none() {
        return Some(unknown_hardware());
    }
    None
}

/// An incomplete probe is not a green light.
fn probe_blocker(snapshot: &AppSnapshot) -> Option<Blocker> {
    let incomplete = snapshot
        .health
        .reasons
        .iter()
        .any(|r| r.code == ReasonCode::ProbeIncomplete);
    // An offline machine is also `unknown`, but it has its own, more useful
    // blocker; do not shadow it with a generic "checks did not finish".
    let unfinished =
        incomplete || (snapshot.health.verdict == HealthVerdict::Unknown && !is_offline(snapshot));
    unfinished.then(
        || Blocker {
            code: BlockerCode::IncompleteProbe,
            headline: "Some checks did not finish".to_owned(),
            detail: "ROCm App could not finish checking this computer, so it will not recommend a setup it is not sure about. Try the check again.".to_owned(),
            next_action: NextAction::Refresh {
                label: "Check this computer again".to_owned(),
            },
        },
    )
}

const fn is_offline(snapshot: &AppSnapshot) -> bool {
    matches!(snapshot.update.state, UpdateState::Offline { .. })
}

/// Setup downloads ROCm, so unreachable or unverifiable metadata blocks it.
fn metadata_blocker(snapshot: &AppSnapshot) -> Option<Blocker> {
    if is_offline(snapshot) {
        return Some(Blocker {
            code: BlockerCode::Offline,
            headline: "No internet connection".to_owned(),
            detail: "Setting up ROCm downloads files from AMD, and this computer cannot reach them right now. Reconnect and check again.".to_owned(),
            next_action: NextAction::Refresh {
                label: "Check again".to_owned(),
            },
        });
    }
    let untrusted = matches!(snapshot.update.state, UpdateState::UntrustedMetadata { .. })
        || matches!(snapshot.update.trust, SourceTrust::Untrusted { .. });
    untrusted.then(|| Blocker {
        code: BlockerCode::UntrustedMetadata,
        headline: "The download list could not be verified".to_owned(),
        detail: "ROCm App checks that AMD's download list is genuine before using it, and that check did not pass. Nothing was downloaded or changed. Check again later.".to_owned(),
        next_action: NextAction::Refresh {
            label: "Check again".to_owned(),
        },
    })
}

// ---------------------------------------------------------------------------
// Copy
// ---------------------------------------------------------------------------

fn facts_for(snapshot: &AppSnapshot, choices: &Choices, available_bytes: Option<u64>) -> Vec<Fact> {
    let fact = |key: &str, label: &str, value: String| Fact {
        key: key.to_owned(),
        label: label.to_owned(),
        value,
    };
    let mut facts = vec![
        fact(
            "gpu",
            "Graphics card",
            snapshot
                .gpu
                .name
                .clone()
                .unwrap_or_else(|| "AMD graphics card".to_owned()),
        ),
        fact("system", "System", system_label(snapshot)),
        fact("driver", "Display driver", driver_summary(snapshot)),
        fact("rocm", "ROCm to install", version_label(choices)),
        fact(
            "space",
            "Space needed",
            format!("About {}", format_bytes(ESTIMATED_INSTALL_BYTES)),
        ),
        fact("folder", "Install folder", choices.target_folder.clone()),
    ];
    if let Some(available) = available_bytes {
        facts.push(fact(
            "free-space",
            "Free space there",
            format_bytes(available),
        ));
    }
    facts
}

fn system_label(snapshot: &AppSnapshot) -> String {
    let os = match snapshot.platform.os {
        OsFamily::Windows => "Windows",
        OsFamily::Linux => "Linux",
        OsFamily::Other => "This system",
    };
    let width = match snapshot.platform.arch.as_str() {
        "x86_64" | "amd64" | "aarch64" | "arm64" => "64-bit",
        other => other,
    };
    format!("{os}, {width}")
}

/// Plain-language driver state. No version invented where none was read.
fn driver_summary(snapshot: &AppSnapshot) -> String {
    match &snapshot.driver.installed {
        DriverVersionState::Known { version } => format!("Installed, version {version}"),
        DriverVersionState::DetectedWithoutVersion { .. } => {
            "Installed (version not reported)".to_owned()
        }
        DriverVersionState::NotDetected { .. } => "Not detected".to_owned(),
        DriverVersionState::Unknown { .. } | DriverVersionState::Unrecognised => {
            "Could not be checked".to_owned()
        }
    }
}

/// What the user is about to install, without naming a channel or a build id.
fn version_label(choices: &Choices) -> String {
    match (&choices.version, choices.channel) {
        (VersionSelector::Exact { version }, _) => format!("Version {version}"),
        (VersionSelector::Latest, Channel::Release) => "Newest stable release".to_owned(),
        (VersionSelector::Latest, Channel::Nightly) => "Newest preview build".to_owned(),
    }
}

fn driver_advice(snapshot: &AppSnapshot) -> DriverAdvice {
    DriverAdvice {
        summary: driver_summary(snapshot),
        note: DRIVER_READ_ONLY_NOTE.to_owned(),
        links: trusted_links(snapshot),
    }
}

/// Links worth putting in front of a user.
///
/// Two conditions, both required: the metadata that carried the link was
/// signed, and the link is `https`. A plain-text link from an unverified
/// source is exactly the vector a driver-download page should not be.
fn trusted_links(snapshot: &AppSnapshot) -> Vec<SupportLink> {
    if !matches!(snapshot.update.trust, SourceTrust::Signed { .. }) {
        return Vec::new();
    }
    snapshot
        .driver
        .support_links
        .iter()
        .filter(|link| link.url.starts_with("https://"))
        .cloned()
        .collect()
}

/// Bytes as a person would say them: `12 GB`, not `12884901888`.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;
    if bytes >= GB {
        let whole = bytes / GB;
        let tenths = (bytes % GB) * 10 / GB;
        if tenths == 0 {
            format!("{whole} GB")
        } else {
            format!("{whole}.{tenths} GB")
        }
    } else {
        format!("{} MB", bytes.div_ceil(MB))
    }
}

#[cfg(test)]
mod tests;
