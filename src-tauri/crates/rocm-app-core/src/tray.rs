// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! The tray monitor: what the icon looks like, what the menu says, and what
//! the compact window shows.
//!
//! # Why this is a core module and not host code
//!
//! Everything on this surface is a decision — which of seven states the
//! machine is in, which glyph carries it, what the menu offers on an
//! unsupported host, whether a left click means anything on this platform.
//! Decisions belong where they can be tested without a display server, a tray
//! daemon, or a GPU. [`crate::tray`] therefore returns *models*; the Tauri
//! layer above it only renders them.
//!
//! # The icon is computed, not shipped
//!
//! [`icon`] rasterises the AMD arrow — the mark from the ROCm lockup — from
//! the artwork's own geometry: two polygons transcribed from
//! `src-tauri/icons/brand/AMD_ROCm_RGB_Blk.svg`, so there is no image decoder,
//! no rasterised asset, and no generator script in the way.
//!
//! # The icon says one thing, in colour alone
//!
//! White while there is nothing to do, orange when the app wants something.
//! That is a deliberate narrowing, taken with eyes open: the mark used to
//! carry a per-status glyph so that shape distinguished all seven states
//! without colour, and it no longer does — a monochrome tray theme, or a
//! reading that cannot separate white from orange, now gets nothing from the
//! icon. What still carries the state in words is the menu's first line, the
//! tooltip, and the compact window; none of them is optional.
//!
//! # Nothing here mutates anything
//!
//! The tray menu offers no install, update, activate, or remove. Those live
//! behind a reviewed plan in a real window. The compact window's primary
//! action is a *handoff* — [`FullSurface`] names the screen to open, never an
//! operation to run.

pub mod notify;
pub mod schedule;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

use crate::contract::{EligibleAction, HealthVerdict};
use crate::health::HealthOverview;
use crate::platform::HostPlatform;

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// What the tray is currently saying.
///
/// One variant per [`HealthVerdict`], plus the two states a verdict cannot
/// express: [`TrayStatus::Checking`] before the first probe answers, and
/// [`TrayStatus::Error`] when the probe could not answer at all. Folding
/// either into a verdict would make "we do not know yet" and "your ROCm is
/// broken" the same icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrayStatus {
    Checking,
    Healthy,
    Unknown,
    SetupRequired,
    Attention,
    Unsupported,
    Error,
}

impl TrayStatus {
    /// Text that carries the status on its own.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Checking => "Checking",
            Self::Healthy => "Ready",
            Self::Unknown => "Not sure",
            Self::SetupRequired => "Setup needed",
            Self::Attention => "Needs attention",
            Self::Unsupported => "Not supported",
            Self::Error => "Cannot check",
        }
    }

    /// Map a health verdict. Total, so a new verdict is a compile error here.
    #[must_use]
    pub const fn from_verdict(verdict: HealthVerdict) -> Self {
        match verdict {
            HealthVerdict::Healthy => Self::Healthy,
            HealthVerdict::Unknown => Self::Unknown,
            HealthVerdict::SetupRequired => Self::SetupRequired,
            HealthVerdict::Attention => Self::Attention,
            HealthVerdict::Unsupported => Self::Unsupported,
        }
    }

    /// Does this state want something from the user?
    ///
    /// The one thing the icon says. A host the app cannot change wants
    /// nothing — an unsupported machine would otherwise nag forever about a
    /// state nobody can leave.
    const fn wants_something(self) -> bool {
        match self {
            Self::Checking | Self::Healthy | Self::Unsupported => false,
            Self::Unknown | Self::SetupRequired | Self::Attention | Self::Error => true,
        }
    }

    /// Mark colour, `(r, g, b)`: white while there is nothing to do, orange
    /// when there is.
    const fn rgb(self) -> (u8, u8, u8) {
        if self.wants_something() {
            (0xE8, 0xA3, 0x3D)
        } else {
            (0xFF, 0xFF, 0xFF)
        }
    }

    /// Every status, for exhaustive tests and fixture generation.
    pub const ALL: [Self; 7] = [
        Self::Checking,
        Self::Healthy,
        Self::Unknown,
        Self::SetupRequired,
        Self::Attention,
        Self::Unsupported,
        Self::Error,
    ];
}

/// Edge length of a rendered tray icon, in pixels.
///
/// Larger than a panel will ever show it (GNOME renders around 22). The mark
/// is all diagonals, and giving the compositor room to scale *down* is what
/// keeps them from stepping.
pub const ICON_SIZE: u32 = 64;

/// Transparent margin around the mark, in pixels.
const MARK_PAD: u32 = 2;

/// Samples per axis when measuring how much of a pixel the mark covers.
const SAMPLES: u32 = 4;

/// The AMD arrow, in hundredths of an SVG unit, relative to its own bounding
/// box: the two polygons of the arrow group in
/// `src-tauri/icons/brand/AMD_ROCm_RGB_Blk.svg`, less the 160.59 x-offset the
/// full lockup gives them. Geometry rather than pixels, so the mark is exact
/// at any [`ICON_SIZE`] and the crate still needs no image decoder.
const MARK: [&[(i64, i64)]; 2] = [
    &[
        (3660, 1370),
        (1413, 1370),
        (42, 0),
        (5030, 0),
        (5030, 4988),
        (3660, 3618),
    ],
    &[
        (1411, 3619),
        (1411, 1645),
        (0, 3055),
        (0, 5030),
        (1975, 5030),
        (3385, 3619),
    ],
];

/// Edge length of [`MARK`]'s bounding box, in the same hundredths.
const MARK_SPAN: i64 = 5030;

/// A rasterised tray icon, ready for `tauri::image::Image::new_owned`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayImage {
    /// `ICON_SIZE * ICON_SIZE * 4` bytes, RGBA, top row first.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Is this point inside the polygon? Even-odd, by counting edge crossings.
fn inside(poly: &[(i64, i64)], x: i64, y: i64) -> bool {
    let mut hit = false;
    for (i, &(x1, y1)) in poly.iter().enumerate() {
        let (x2, y2) = poly[(i + 1) % poly.len()];
        if (y1 > y) == (y2 > y) {
            continue;
        }
        // `x < x1 + (y - y1) * (x2 - x1) / (y2 - y1)`, cross-multiplied: the
        // edge is never divided, so the test is exact and needs no floats.
        let left = (x - x1) * (y2 - y1);
        let right = (y - y1) * (x2 - x1);
        if (y2 > y1) == (left < right) {
            hit = !hit;
        }
    }
    hit
}

/// How much of one pixel the mark covers, as an alpha value.
fn mark_alpha(x: u32, y: u32) -> u8 {
    let span = i64::from(ICON_SIZE - 2 * MARK_PAD);
    let steps = span * i64::from(SAMPLES) * 2;
    // Sub-sample centres, in the mark's own space. `None` is a sample outside
    // the mark's box: the margin, which is always clear.
    let axis = |pixel: u32, sub: u32| -> Option<i64> {
        let offset = (i64::from(pixel) - i64::from(MARK_PAD)) * i64::from(SAMPLES);
        let step = (offset + i64::from(sub)) * 2 + 1;
        (0..=steps).contains(&step).then(|| step * MARK_SPAN / steps)
    };
    let mut covered = 0_u32;
    for sub_y in 0..SAMPLES {
        for sub_x in 0..SAMPLES {
            let (Some(u), Some(v)) = (axis(x, sub_x), axis(y, sub_y)) else {
                continue;
            };
            if MARK.iter().any(|poly| inside(poly, u, v)) {
                covered += 1;
            }
        }
    }
    u8::try_from(covered * 255 / (SAMPLES * SAMPLES)).unwrap_or(u8::MAX)
}

/// Rasterise the icon for a status.
///
/// The AMD arrow, and nothing else: white while there is nothing to do,
/// orange when the app wants something. Everything around the mark is fully
/// transparent, so it sits on a light or dark tray equally well, and its
/// diagonals are antialiased because the panel only ever scales it down.
#[must_use]
pub fn icon(status: TrayStatus) -> TrayImage {
    let (r, g, b) = status.rgb();
    let mut rgba = vec![0_u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let alpha = mark_alpha(x, y);
            if alpha == 0 {
                continue;
            }
            let offset = ((y * ICON_SIZE + x) * 4) as usize;
            rgba[offset] = r;
            rgba[offset + 1] = g;
            rgba[offset + 2] = b;
            rgba[offset + 3] = alpha;
        }
    }
    TrayImage {
        rgba,
        width: ICON_SIZE,
        height: ICON_SIZE,
    }
}

// ---------------------------------------------------------------------------
// Menu model
// ---------------------------------------------------------------------------

/// Stable menu item identifiers. The host matches on these, never on text.
pub mod menu_id {
    pub const INFO_GPU: &str = "info-gpu";
    pub const INFO_SYSTEM: &str = "info-system";
    pub const INFO_ROCM: &str = "info-rocm";
    pub const OPEN_APP: &str = "open-app";
    pub const MORE_INFO: &str = "more-info";
    pub const QUIT: &str = "quit";
}

/// What kind of native item an entry becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MenuKind {
    /// Non-interactive current status.
    Label,
    Action,
    /// Kept for wire compatibility; no current entry emits it.
    Check {
        checked: bool,
    },
    Separator,
}

/// One tray menu entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuEntry {
    pub id: String,
    pub text: String,
    pub kind: MenuKind,
    pub enabled: bool,
}

/// Everything the tray renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayView {
    pub status: TrayStatus,
    /// The menu's first line, e.g. `ROCm 7.14.0 — Ready`.
    pub short_status: String,
    /// Hover text. Linux does not render tray tooltips (documented
    /// unsupported), so the same string is the compact window's subtitle
    /// rather than data only one platform can see.
    pub tooltip: String,
    pub items: Vec<MenuEntry>,
}

/// Everything the tray and compact window are derived from.
///
/// `overview` absent means the first probe has not answered; `error` present
/// means it answered with a failure. Both are states a user sees, so both are
/// modelled rather than collapsed into an `Option`.
#[derive(Debug, Clone, Copy)]
pub struct TrayInput<'a> {
    pub overview: Option<&'a HealthOverview>,
    pub error: Option<&'a str>,
    pub platform: HostPlatform,
}

impl TrayInput<'_> {
    /// The status these inputs describe.
    ///
    /// A failed probe wins over a stale overview: showing the last good
    /// verdict while the app can no longer read the machine is the one
    /// reading that is actively misleading.
    #[must_use]
    pub fn status(&self) -> TrayStatus {
        if self.error.is_some() {
            return TrayStatus::Error;
        }
        self.overview.map_or(TrayStatus::Checking, |o| {
            TrayStatus::from_verdict(o.verdict)
        })
    }

    /// Active ROCm version, when there is one.
    fn rocm_fact(&self) -> Option<&str> {
        self.fact("rocm")
    }

    fn fact(&self, key: &str) -> Option<&str> {
        self.overview?
            .headline_facts
            .iter()
            .find(|f| f.key == key)
            .map(|f| f.value.as_str())
    }
}

/// Build the tray model.
///
/// The item list is fixed: same entries, same order, same ids on every
/// platform and in every state. A menu whose shape depends on health is a menu
/// users cannot learn, and on Linux a tray menu cannot be replaced after it is
/// set — only its contents updated — so a stable shape is a platform
/// requirement, not a preference.
#[must_use]
pub fn tray_view(input: &TrayInput<'_>) -> TrayView {
    let status = input.status();
    let short_status = short_status(input, status);
    let tooltip = match input.error {
        Some(detail) => detail.to_owned(),
        None => input.overview.map_or_else(
            || "Checking this computer…".to_owned(),
            |o| o.summary.clone(),
        ),
    };
    // A fact the probe has not delivered yet is honestly "Checking…" — unless
    // the probe failed, in which case it will never arrive and "Unknown" is
    // the truthful reading.
    let fallback = if input.error.is_some() {
        "Unknown"
    } else {
        "Checking…"
    };
    let info = |key: &str| input.fact(key).unwrap_or(fallback);
    let entry = |id: &str, text: String, kind: MenuKind, enabled: bool| MenuEntry {
        id: id.to_owned(),
        text,
        kind,
        enabled,
    };
    let items = vec![
        entry(
            menu_id::INFO_GPU,
            format!("Graphics card: {}", info("gpu")),
            MenuKind::Label,
            false,
        ),
        entry(
            menu_id::INFO_SYSTEM,
            format!("System: {}", info("system")),
            MenuKind::Label,
            false,
        ),
        entry(
            menu_id::INFO_ROCM,
            format!("ROCm in use: {}", info("rocm")),
            MenuKind::Label,
            false,
        ),
        entry("separator-info", String::new(), MenuKind::Separator, false),
        entry(
            menu_id::OPEN_APP,
            "Open ROCm App".to_owned(),
            MenuKind::Action,
            true,
        ),
        entry(
            menu_id::MORE_INFO,
            "More Info".to_owned(),
            MenuKind::Action,
            true,
        ),
        entry("separator-quit", String::new(), MenuKind::Separator, false),
        entry(
            menu_id::QUIT,
            "Quit ROCm App".to_owned(),
            MenuKind::Action,
            true,
        ),
    ];
    TrayView {
        status,
        short_status,
        tooltip,
        items,
    }
}

/// `ROCm 7.14.0 — Ready`, or the shortest honest thing available.
fn short_status(input: &TrayInput<'_>, status: TrayStatus) -> String {
    match input.rocm_fact() {
        // The version fact carries its own qualifier when a runtime failed its
        // check ("7.14.0 — failed its check"); repeating the status after it
        // would read as two verdicts.
        Some(version) if version.contains('—') => format!("ROCm {version}"),
        Some(version) => format!("ROCm {version} — {}", status.label()),
        None => format!("ROCm — {}", status.label()),
    }
}

/// Whether a left click on the tray icon should open the compact window.
///
/// Windows only. Tauri documents tray icon click events as unsupported on
/// Linux — "the event is not emitted even though the icon is shown" — so a
/// Linux build that relied on them would ship a dead affordance. Both
/// platforms reach the same two windows through the menu.
#[must_use]
pub const fn left_click_opens_quick_status(platform: HostPlatform) -> bool {
    matches!(platform, HostPlatform::Windows)
}

// ---------------------------------------------------------------------------
// Compact quick status
// ---------------------------------------------------------------------------

/// Which full window a compact action hands off to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FullSurface {
    Dashboard,
    Onboarding,
    Runtimes,
}

/// The compact window's single button: it opens a window, never runs a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickAction {
    pub label: String,
    pub opens: FullSurface,
}

/// Everything the compact quick-status window renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickStatus {
    pub status: TrayStatus,
    pub status_label: String,
    pub gpu: String,
    pub rocm_version: String,
    pub last_check: String,
    /// The primary reason, in the same reviewed copy the Overview uses.
    pub reason: String,
    /// `None` only when there is genuinely nothing to do and nowhere useful to
    /// go; the window always offers "Open ROCm App" separately.
    pub action: Option<QuickAction>,
}

/// Build the compact view.
#[must_use]
pub fn quick_status(input: &TrayInput<'_>) -> QuickStatus {
    let status = input.status();
    let unknown = |what: &str| format!("Not known yet — {what}");
    QuickStatus {
        status,
        status_label: status.label().to_owned(),
        gpu: input
            .fact("gpu")
            .map_or_else(|| unknown("still checking"), str::to_owned),
        rocm_version: input
            .rocm_fact()
            .map_or_else(|| unknown("still checking"), str::to_owned),
        last_check: input
            .overview
            .map_or_else(|| "Checking now".to_owned(), |o| o.freshness.label.clone()),
        reason: match input.error {
            Some(detail) => detail.to_owned(),
            None => input.overview.map_or_else(
                || "Reading this computer's ROCm setup.".to_owned(),
                |o| o.summary.clone(),
            ),
        },
        action: quick_action(input),
    }
}

fn quick_action(input: &TrayInput<'_>) -> Option<QuickAction> {
    let overview = input.overview?;
    if input.error.is_some() {
        return None;
    }
    let opens = if overview.first_run {
        FullSurface::Onboarding
    } else {
        match overview.next_step.action? {
            EligibleAction::InstallRuntime => FullSurface::Onboarding,
            EligibleAction::UpdateRuntime
            | EligibleAction::ActivateRuntime
            | EligibleAction::RemoveRuntime
            | EligibleAction::ValidateRuntime => FullSurface::Runtimes,
            // A step this app does not recognise gets no shortcut; the full
            // window is still one click away.
            EligibleAction::Unrecognised => return None,
        }
    };
    Some(QuickAction {
        label: overview.next_step.label.clone(),
        opens,
    })
}

// ---------------------------------------------------------------------------
// Startup and autostart policy
// ---------------------------------------------------------------------------

/// Argument the autostart entry passes, and the one flag that keeps a boot
/// launch out of the user's face.
pub const HIDDEN_FLAG: &str = "--hidden";

/// Whether this launch should keep the main window closed.
///
/// Takes the argument list rather than reading `std::env::args` so the rule is
/// testable, and so a second instance's forwarded argv can be judged by the
/// same function.
pub fn start_hidden<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|a| a.as_ref() == HIDDEN_FLAG)
}

/// Whether autostart should be on, given what the user last chose.
///
/// `installed_build` is the only reason this is not `unwrap_or(true)`: a
/// developer running a debug build from `target/` has not installed anything,
/// and registering a login item behind their back for a binary that will be
/// deleted is a bug, not a default. Release builds — which is what an
/// installer ships — default it on.
#[must_use]
pub const fn autostart_desired(persisted: Option<bool>, installed_build: bool) -> bool {
    match persisted {
        Some(chosen) => chosen,
        None => installed_build,
    }
}

/// What to do about the gap between the wanted state and the operating
/// system's actual state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartAction {
    Enable,
    Disable,
    Nothing,
}

/// Reconcile intent with reality.
///
/// Called on every launch, not only the first: a user who removed the login
/// item by hand, or an OS that dropped it during an upgrade, must not leave
/// the app claiming a setting it does not have.
#[must_use]
pub const fn reconcile_autostart(desired: bool, os_enabled: bool) -> AutostartAction {
    match (desired, os_enabled) {
        (true, false) => AutostartAction::Enable,
        (false, true) => AutostartAction::Disable,
        _ => AutostartAction::Nothing,
    }
}

/// Storage key holding the user's autostart choice, as `true`/`false` JSON.
pub const AUTOSTART_KEY: &str = "autostart.json";

/// What Settings shows about autostart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartState {
    /// What the operating system reports, not what the app asked for. A
    /// Settings screen that echoes intent cannot show a failed enable.
    pub enabled: bool,
    /// False on hosts where the app cannot manage ROCm; the toggle is shown
    /// disabled rather than hidden, so the reason can be read.
    pub available: bool,
    pub detail: String,
}
