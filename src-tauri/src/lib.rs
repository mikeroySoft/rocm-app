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
pub mod window_host;

use rocm_app_core::platform::HostPlatform;
use tauri_plugin_window_state::StateFlags;

fn window_state_flags() -> StateFlags {
    #[cfg(target_os = "linux")]
    let wayland = is_wayland_backend(
        std::env::var_os("GDK_BACKEND").as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
    );
    #[cfg(target_os = "windows")]
    let wayland = false;
    window_state_flags_for(wayland)
}

fn window_state_flags_for(wayland: bool) -> StateFlags {
    if wayland {
        // Wayland owns placement, and restoring a physical size before GTK's
        // fractional scale settles leaves client-side titlebar hitboxes stale.
        StateFlags::all() & !(StateFlags::SIZE | StateFlags::POSITION)
    } else {
        StateFlags::all()
    }
}

#[cfg(target_os = "linux")]
fn is_wayland_backend(
    gdk_backend: Option<&std::ffi::OsStr>,
    wayland_display: Option<&std::ffi::OsStr>,
) -> bool {
    match gdk_backend {
        Some(backend) if backend == std::ffi::OsStr::new("x11") => false,
        Some(backend) if backend == std::ffi::OsStr::new("wayland") => true,
        _ => wayland_display.is_some(),
    }
}

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
        // Used from Rust only (the bundle-destination picker command), so the
        // webview still holds exactly `core:default`.
        .plugin(tauri_plugin_dialog::init())
        // The compact window is transient and positioned by the tray; restoring
        // a remembered geometry for it would fight that.
        .plugin(
            tauri_plugin_window_state::Builder::new()
                .with_denylist(&[tray_host::QUICK_WINDOW])
                .with_state_flags(window_state_flags())
                .build(),
        )
        .setup(|app| {
            use tauri::Manager as _;
            build_declared_windows(app)?;
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
            controller_host::diagnostics_pick_destination,
            tray_host::tray_quick_status,
            tray_host::tray_model,
            tray_host::tray_autostart_state,
            tray_host::tray_set_autostart,
            tray_host::tray_open_full,
            tray_host::tray_hide_quick,
            window_host::window_minimize,
            window_host::window_toggle_maximize,
            window_host::window_close,
            window_host::window_start_drag,
            window_host::window_start_resize,
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

/// Create the two config-declared windows (`create: false` keeps Tauri from
/// auto-creating them), so the WebView2 launch arguments can be decided at
/// runtime.
///
/// Wry always sets WebView2's `AdditionalBrowserArguments` itself, and the
/// runtime then ignores the `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` variable
/// msedgedriver plants to open the remote-debugging port. A driven app
/// therefore never opened the port, its `DevToolsActivePort` file never
/// appeared, and every WebDriver session on Windows died with "Microsoft
/// Edge failed to start: crashed" while the same binary ran fine launched
/// bare — measured by the CI probe step, which failed identically with and
/// without a user-data-folder hint. When the variable is present (only a
/// driver plants it), append it to wry's own defaults (wry 0.55.1,
/// src/webview2/mod.rs). An undriven launch sees no variable and builds the
/// exact arguments wry would have chosen, so no debug port ever opens
/// unless the environment explicitly asked for one.
fn build_declared_windows(app: &tauri::App) -> tauri::Result<()> {
    #[cfg(windows)]
    let driver_args = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").ok();
    let windows = app.config().app.windows.clone();
    for config in &windows {
        #[allow(unused_mut)]
        let mut builder = tauri::WebviewWindowBuilder::from_config(app.handle(), config)?;
        #[cfg(windows)]
        if let Some(args) = driver_args.as_deref() {
            builder = builder.additional_browser_args(&format!(
                "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection {args}"
            ));
        }
        // Both windows draw their own chrome, and on Linux the toolkit
        // installs one of its own regardless. Take it off before the window is
        // ever shown.
        window_host::strip_toolkit_titlebar(&builder.build()?);
    }
    Ok(())
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
    #[test]
    fn wayland_window_state_keeps_modes_but_not_geometry() {
        let flags = window_state_flags_for(true);
        assert!(!flags.intersects(StateFlags::SIZE | StateFlags::POSITION));
        assert!(flags.contains(
            StateFlags::MAXIMIZED
                | StateFlags::VISIBLE
                | StateFlags::DECORATIONS
                | StateFlags::FULLSCREEN
        ));
        assert_eq!(
            window_state_flags_for(false).bits(),
            StateFlags::all().bits()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn forced_x11_wins_inside_a_wayland_session() {
        let display = std::ffi::OsStr::new("wayland-0");
        assert!(!is_wayland_backend(
            Some(std::ffi::OsStr::new("x11")),
            Some(display)
        ));
        assert!(is_wayland_backend(
            Some(std::ffi::OsStr::new("wayland")),
            None
        ));
        assert!(is_wayland_backend(None, Some(display)));
    }
}
