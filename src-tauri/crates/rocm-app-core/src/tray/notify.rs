// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! When the app is allowed to interrupt the user.
//!
//! # Transitions only
//!
//! A monitor that notifies on every observation is a monitor users mute, and a
//! muted monitor is worse than no monitor: the one time it matters, nobody
//! sees it. So the rule is narrow and mechanical — say something when the
//! **status changed**, or when an update that was not being offered **is now**.
//! Nothing else. An unchanged verdict, however alarming, produces silence
//! because the user has already been told.
//!
//! # Deduplicated across restarts
//!
//! The last thing said is persisted, so quitting and relaunching does not
//! re-announce a state the user already dismissed. That also means the first
//! observation after a fresh install *seeds* and stays quiet: a brand-new
//! install announcing "setup needed" the instant it starts is not news, it is
//! the reason the user opened it.
//!
//! # What is not here
//!
//! Operation completion and failure are notified by the controller through
//! [`crate::controller::adapters::Notifier`], once per operation. They need no
//! deduplication — a completed install is by definition a transition — and
//! routing them through this module would make one event two notifications.

use serde::{Deserialize, Serialize};

use super::TrayStatus;
use crate::contract::AppSnapshot;
use crate::runtimes::{UpdateStanding, standing_for};

/// Storage key for the persisted last-notified state.
pub const KEY: &str = "notify-state.json";

/// The last thing the app told the user, as persisted between runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NotifyState {
    /// `None` only before the very first observation.
    pub status: Option<TrayStatus>,
    /// The update version last announced, if any.
    pub update_version: Option<String>,
}

/// One desktop notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub title: String,
    pub body: String,
}

/// The version an update is genuinely offering this machine.
///
/// Delegates to [`standing_for`], the same derivation the ROCm versions screen
/// uses, rather than re-reading [`crate::contract::UpdateState`] here. That
/// reuse is what makes an untrusted index and an update built for a different
/// graphics card silent: neither is an [`UpdateStanding::Available`], so
/// neither can produce a notification offering something the app would then
/// refuse to install.
#[must_use]
pub fn available_update(snapshot: &AppSnapshot) -> Option<String> {
    match standing_for(snapshot) {
        UpdateStanding::Available { latest, .. } => Some(latest),
        // Every other standing — up to date, offline, stale, untrusted,
        // incompatible, ahead of the index, unrecognised — is not an offer.
        _ => None,
    }
}

/// Decide what, if anything, to say about a new observation, and record it.
///
/// `state` is updated in place whether or not a notification is produced: what
/// is stored is "the last thing observed", so a silent seed still suppresses a
/// duplicate announcement on the next launch. Callers persist `state` when it
/// differs from the value they loaded.
///
/// Precedence is deliberate. A status change subsumes a simultaneous new
/// update — a machine that went from Ready to Needs-attention *because* an
/// update appeared is one event, and two notifications for it is noise.
pub fn on_observation(
    state: &mut NotifyState,
    status: TrayStatus,
    available_update: Option<String>,
) -> Option<Notification> {
    // Never interrupt, and never *record*, while the answer is still "we are
    // looking". Checking is a transient the user caused by launching the app.
    // Storing it would make the next real answer look like a transition and
    // re-announce a verdict the user already has, and would forget an update
    // that is still being offered.
    if status == TrayStatus::Checking {
        return None;
    }

    let previous_status = state.status.replace(status);
    let previous_update = std::mem::replace(&mut state.update_version, available_update.clone());

    match previous_status {
        // First ever observation: seed, stay quiet.
        None => None,
        Some(before) if before != status => Some(Notification {
            title: "ROCm".to_owned(),
            body: transition_body(status),
        }),
        Some(_) => {
            let newly_offered = available_update.filter(|latest| {
                previous_update
                    .as_ref()
                    .is_none_or(|before| before != latest)
            })?;
            Some(Notification {
                title: "ROCm update available".to_owned(),
                body: format!("ROCm {newly_offered} is available. Open ROCm App to review it."),
            })
        }
    }
}

/// Reviewed copy for arriving at a status. Keyed by the status, never by
/// producer prose, for the same reason the Overview's copy is.
fn transition_body(status: TrayStatus) -> String {
    match status {
        TrayStatus::Healthy => "ROCm is ready to use.".to_owned(),
        TrayStatus::Unknown => {
            "ROCm cannot be confirmed on this computer. Open ROCm App for details.".to_owned()
        }
        TrayStatus::SetupRequired => "ROCm needs setting up. Open ROCm App to start.".to_owned(),
        TrayStatus::Attention => "ROCm needs attention. Open ROCm App for details.".to_owned(),
        TrayStatus::Unsupported => "ROCm is not supported on this computer.".to_owned(),
        TrayStatus::Error => {
            "ROCm App could not check this computer. Open it for details.".to_owned()
        }
        // Filtered out before this point; a transient is never announced.
        TrayStatus::Checking => "Checking this computer.".to_owned(),
    }
}
