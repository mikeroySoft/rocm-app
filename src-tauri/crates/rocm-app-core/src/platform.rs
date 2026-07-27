// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Host platform classification and the single install-eligibility gate.
//!
//! ROCm App supports **native Windows and native Linux only**. WSL is modelled
//! as its own variant rather than folded into [`HostPlatform::Unsupported`]
//! because it is the one unsupported host a user is likely to be sitting in
//! front of by accident, and it earns a specific explanation.
//!
//! Classification is split from detection on purpose: [`HostPlatform::classify`]
//! is a pure function over evidence, so every platform's behaviour — including
//! the ones this machine can never be — is reachable from a unit test on any
//! host. A `#[cfg]`-only gate would make the WSL and unsupported paths
//! untestable on the Linux box that has to prove them.

use serde::{Deserialize, Serialize};

/// The host this process is running on, as far as install eligibility cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostPlatform {
    Windows,
    Linux,
    /// Linux kernel running under the Windows Subsystem for Linux.
    Wsl,
    /// Anything else: macOS, BSD, an unrecognised target.
    Unsupported,
}

/// Which operating system family the running binary was built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOs {
    Windows,
    Linux,
    Other,
}

impl HostPlatform {
    /// Classify a host from evidence. Pure — every branch is unit-testable.
    #[must_use]
    pub const fn classify(os: TargetOs, under_wsl: bool) -> Self {
        match os {
            TargetOs::Windows => Self::Windows,
            TargetOs::Linux if under_wsl => Self::Wsl,
            TargetOs::Linux => Self::Linux,
            TargetOs::Other => Self::Unsupported,
        }
    }

    /// Detect the current host.
    #[must_use]
    pub fn detect() -> Self {
        Self::classify(current_target_os(), detect_wsl())
    }

    /// Whether this host may be offered **any** install, update, or activate
    /// operation.
    ///
    /// This is the one gate the rest of the product consults. Callers must
    /// *omit* the control when this is false rather than render it disabled: a
    /// greyed-out Install button on WSL still promises the operation is nearly
    /// available, which it is not.
    #[must_use]
    pub const fn install_allowed(self) -> bool {
        matches!(self, Self::Windows | Self::Linux)
    }

    /// Plain-language reason this host is unsupported, or `None` when supported.
    ///
    /// Kept textually identical to `unsupportedReason` in `src/lib/platform.ts`.
    /// The renderer needs the sentence before a backend answers, so the string
    /// lives on both sides; Phase 12's copy review owns keeping them in step.
    #[must_use]
    pub const fn unsupported_reason(self) -> Option<&'static str> {
        match self {
            Self::Windows | Self::Linux => None,
            Self::Wsl => Some(
                "ROCm App manages ROCm on native Windows and native Linux. \
                 WSL cannot reach the GPU the way this app requires — run ROCm \
                 App on your Windows desktop instead.",
            ),
            Self::Unsupported => Some("ROCm App runs on native Windows and native Linux only."),
        }
    }

    /// Stable wire identifier, matching the `serde` representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Wsl => "wsl",
            Self::Unsupported => "unsupported",
        }
    }
}

const fn current_target_os() -> TargetOs {
    #[cfg(target_os = "windows")]
    {
        TargetOs::Windows
    }
    #[cfg(target_os = "linux")]
    {
        TargetOs::Linux
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        TargetOs::Other
    }
}

/// True when a Linux kernel is running under WSL.
///
/// Both signals are checked because either alone has a false negative: the
/// `WSL_*` variables are absent when a service is started outside an interop
/// shell, and a custom-built WSL2 kernel can drop the `microsoft` marker from
/// its release string.
fn detect_wsl() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let env_marker =
        std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some();
    let osrelease = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    wsl_marker_in_osrelease(&osrelease) || env_marker
}

/// Whether a `/proc/sys/kernel/osrelease` string carries a WSL marker.
#[must_use]
pub fn wsl_marker_in_osrelease(osrelease: &str) -> bool {
    let lower = osrelease.to_ascii_lowercase();
    lower.contains("microsoft") || lower.contains("wsl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_covers_every_host() {
        assert_eq!(
            HostPlatform::classify(TargetOs::Windows, false),
            HostPlatform::Windows
        );
        assert_eq!(
            HostPlatform::classify(TargetOs::Linux, false),
            HostPlatform::Linux
        );
        assert_eq!(
            HostPlatform::classify(TargetOs::Linux, true),
            HostPlatform::Wsl
        );
        assert_eq!(
            HostPlatform::classify(TargetOs::Other, false),
            HostPlatform::Unsupported
        );
    }

    /// WSL claims a Windows kernel underneath, but the app cannot reach the GPU
    /// through it. Classifying it as Windows would offer a broken install.
    #[test]
    fn wsl_is_never_treated_as_windows() {
        assert_eq!(
            HostPlatform::classify(TargetOs::Windows, true),
            HostPlatform::Windows,
            "the WSL flag only applies to a Linux target"
        );
        assert_ne!(
            HostPlatform::classify(TargetOs::Linux, true),
            HostPlatform::Linux
        );
    }

    #[test]
    fn only_native_windows_and_linux_may_install() {
        assert!(HostPlatform::Windows.install_allowed());
        assert!(HostPlatform::Linux.install_allowed());
        assert!(!HostPlatform::Wsl.install_allowed());
        assert!(!HostPlatform::Unsupported.install_allowed());
    }

    #[test]
    fn every_ineligible_host_explains_itself() {
        for host in [HostPlatform::Wsl, HostPlatform::Unsupported] {
            let reason = host.unsupported_reason().expect("must explain refusal");
            assert!(!reason.is_empty());
        }
        assert!(HostPlatform::Windows.unsupported_reason().is_none());
        assert!(HostPlatform::Linux.unsupported_reason().is_none());
    }

    /// The refusal reason and the install gate must never disagree: a host that
    /// can install must not also print a reason it cannot, and vice versa.
    #[test]
    fn reason_and_gate_agree() {
        for host in [
            HostPlatform::Windows,
            HostPlatform::Linux,
            HostPlatform::Wsl,
            HostPlatform::Unsupported,
        ] {
            assert_eq!(
                host.install_allowed(),
                host.unsupported_reason().is_none(),
                "{host:?} disagrees with itself"
            );
        }
    }

    #[test]
    fn osrelease_markers() {
        assert!(wsl_marker_in_osrelease(
            "5.15.153.1-microsoft-standard-WSL2"
        ));
        assert!(wsl_marker_in_osrelease("6.6.0-WSL2-custom"));
        assert!(wsl_marker_in_osrelease("4.4.0-19041-Microsoft"));
        assert!(!wsl_marker_in_osrelease("7.0.0-28-generic"));
        assert!(!wsl_marker_in_osrelease(""));
    }

    #[test]
    fn wire_names_round_trip() {
        for host in [
            HostPlatform::Windows,
            HostPlatform::Linux,
            HostPlatform::Wsl,
            HostPlatform::Unsupported,
        ] {
            let json = serde_json::to_string(&host).expect("serialize");
            assert_eq!(json, format!("\"{}\"", host.as_str()));
            let back: HostPlatform = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, host);
        }
    }

    /// Detection must terminate and yield a real variant on this host. On the
    /// Linux CI/dev box that means a non-Windows answer.
    #[test]
    fn detect_runs_on_this_host() {
        let host = HostPlatform::detect();
        if cfg!(target_os = "linux") {
            assert!(matches!(host, HostPlatform::Linux | HostPlatform::Wsl));
        }
    }
}
