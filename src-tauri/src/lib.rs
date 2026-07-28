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
pub mod tray_host;

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
///
/// `Builder::build` then `App::run`, not `Builder::run`: only the latter form
/// gives access to `RunEvent::ExitRequested`, and without intercepting it Wry
/// terminates the process as soon as its last window is destroyed — which is
/// exactly what a tray-only app must survive.
pub fn run() {
    let app = tauri::Builder::default()
        // Registered first, as the plugin's documentation requires, so a second
        // launch is intercepted before anything else can act on it.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            tray_host::focus_existing_instance(app, &argv);
        }))
        .plugin(tauri_plugin_notification::init())
        // The login item launches with `--hidden`, so a boot start monitors
        // without putting a window in front of anyone.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![rocm_app_core::tray::HIDDEN_FLAG]),
        ))
        // The compact window is transient and positioned by the tray; restoring
        // a remembered geometry for it would fight that.
        .plugin(
            tauri_plugin_window_state::Builder::new()
                .with_denylist(&[tray_host::QUICK_WINDOW])
                .build(),
        )
        .setup(|app| {
            use tauri::Manager as _;
            let data_dir = app.path().app_data_dir()?;
            let mut adapters = controller_host::production_adapters(data_dir);
            let storage = adapters.storage.clone();
            let diagnostics = adapters.diagnostics.clone();
            // Operation outcomes now reach the desktop as well as the log the
            // Phase 9 diagnostics view reads.
            adapters.notifier = std::sync::Arc::new(tray_host::DesktopNotifier::new(
                app.handle().clone(),
                adapters.notifier.clone(),
            ));
            app.manage(controller_host::ControllerState {
                controller: rocm_app_core::RocmController::new(adapters),
                telemetry: controller_host::TelemetryStore::new(),
                storage,
                diagnostics,
            });
            // The tray is created before the first probe runs, so a boot launch
            // shows an icon while the snapshot is still being taken.
            tray_host::start(app.handle())?;
            Ok(())
        })
        // Closing a window closes a view, not the product.
        .on_window_event(tray_host::on_window_event)
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
            controller_host::diagnostics_logs,
            controller_host::diagnostics_diagnose,
            controller_host::diagnostics_export,
            controller_host::diagnostics_fix_plan,
            tray_host::tray_quick_status,
            tray_host::tray_model,
            tray_host::tray_check_now,
            tray_host::tray_autostart_state,
            tray_host::tray_set_autostart,
            tray_host::tray_open_full,
            tray_host::tray_hide_quick,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start ROCm App");

    app.run(|_app, event| {
        // `code: None` is a user-driven exit request — the one Wry raises when
        // the last window is destroyed. Quit calls `AppHandle::exit(0)`, which
        // carries a code and is therefore never prevented here. An
        // unconditional `prevent_exit` would make the app unquittable.
        if let tauri::RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            api.prevent_exit();
        }
    });
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
