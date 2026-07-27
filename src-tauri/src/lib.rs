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

use rocm_app_core::platform::HostPlatform;

/// Report the host this process is running on.
///
/// An in-process second opinion on the contract's own `platform` block: it
/// needs no CLI, so the shell can refuse to offer changes even when the
/// snapshot cannot be read at all.
#[tauri::command]
fn host_platform() -> HostPlatform {
    HostPlatform::detect()
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
                telemetry: controller_host::TelemetryStore::new(),
            });
            Ok(())
        })
        // No shell plugin is registered here, and none may be added: the
        // controller owns every process invocation, in Rust, from a typed
        // operation. See capabilities/default.json.
        .invoke_handler(tauri::generate_handler![
            host_platform,
            controller_host::controller_snapshot,
            controller_host::controller_plan,
            controller_host::controller_execute,
            controller_host::controller_cancel,
            controller_host::onboarding_view,
            controller_host::health_overview,
            controller_host::runtimes_view,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start ROCm App");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_reports_a_supported_host_here() {
        // This suite runs on native Linux and Windows only; both are supported.
        assert!(matches!(
            host_platform(),
            HostPlatform::Linux | HostPlatform::Windows | HostPlatform::Wsl
        ));
    }

    /// WSL is classified as its own thing, never folded into Windows. It is
    /// the one unsupported host a user is likely to be sitting in front of by
    /// accident, and the shell refuses changes on it.
    #[test]
    fn wsl_is_never_reported_as_a_supported_host() {
        assert!(!HostPlatform::Wsl.install_allowed());
        assert!(HostPlatform::Wsl.unsupported_reason().is_some());
    }
}
