// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Tauri composition root.
//!
//! This crate owns no product rules. It wires [`rocm_app_core`] to typed Tauri
//! commands and nothing else — every decision the app makes lives in the core
//! crate, where it is testable without a WebView.

// The product supports native Windows and native Linux. Failing at compile time
// on anything else is the explicit behaviour the plan requires: a best-effort
// build on an unsupported host would ship an app that cannot reach a GPU and
// cannot say why. `rocm_app_core` stays portable so its unit tests can still
// model unsupported hosts from any machine.
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
compile_error!(
    "ROCm App supports native Windows and native Linux only. \
     macOS, BSD, and other targets are out of scope."
);

pub mod controller_host;

use rocm_app_core::fixtures::{self, FixtureSnapshot, Scenario};
use rocm_app_core::platform::HostPlatform;

/// Report the host this process is running on.
#[tauri::command]
fn host_platform() -> HostPlatform {
    HostPlatform::detect()
}

/// List the available fixture scenario identifiers.
#[tauri::command]
fn fixture_scenarios() -> Vec<&'static str> {
    Scenario::ALL.iter().map(|s| s.as_str()).collect()
}

/// Read one deterministic fixture snapshot.
///
/// Unknown identifiers are rejected rather than defaulted: silently returning
/// `healthy` for a typo would let a broken caller render a reassuring screen.
#[tauri::command]
fn fixture_snapshot(scenario: &str) -> Result<FixtureSnapshot, String> {
    Scenario::from_wire(scenario)
        .map(|s| fixtures::snapshot(s).clone())
        .ok_or_else(|| format!("unknown fixture scenario: {scenario}"))
}

/// Build and run the desktop application.
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            use tauri::Manager as _;
            let data_dir = app.path().app_data_dir()?;
            app.manage(controller_host::ControllerState {
                controller: rocm_app_core::RocmController::new(
                    controller_host::production_adapters(data_dir),
                ),
            });
            Ok(())
        })
        // No shell plugin is registered here, and none may be added: the
        // controller owns every process invocation, in Rust, from a typed
        // operation. See capabilities/default.json.
        .invoke_handler(tauri::generate_handler![
            host_platform,
            fixture_scenarios,
            fixture_snapshot,
            controller_host::controller_snapshot,
            controller_host::controller_plan,
            controller_host::controller_execute,
            controller_host::controller_cancel,
            controller_host::onboarding_view,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start ROCm App");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scenario_is_reachable_by_wire_name() {
        for name in fixture_scenarios() {
            let snap = fixture_snapshot(name).expect("known scenario must resolve");
            assert_eq!(snap.scenario.as_str(), name);
        }
    }

    #[test]
    fn unknown_scenario_is_rejected() {
        let err = fixture_snapshot("healthy; drop table").expect_err("must reject");
        assert!(err.contains("unknown fixture scenario"));
    }

    /// The command layer must not widen what the domain layer allows: a WSL
    /// snapshot has to arrive at the renderer still carrying no install offer.
    #[test]
    fn wsl_snapshot_crosses_the_boundary_with_no_install() {
        let snap = fixture_snapshot("unsupported-wsl").expect("wsl fixture");
        assert!(!snap.install_available);
        assert!(!snap.platform.install_allowed());
        assert!(snap.platform.unsupported_reason().is_some());
    }

    #[test]
    fn detect_reports_a_supported_host_here() {
        // This suite runs on native Linux and Windows only; both are supported.
        assert!(matches!(
            host_platform(),
            HostPlatform::Linux | HostPlatform::Windows | HostPlatform::Wsl
        ));
    }
}
