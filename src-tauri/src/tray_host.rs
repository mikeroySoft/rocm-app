// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! The tray monitor's Tauri wiring.
//!
//! Every decision on this surface lives in [`rocm_app_core::tray`]. What is
//! here is plumbing: build the native icon and menu from the core model, run
//! one background thread that asks the core scheduler what is due, and route
//! menu clicks to windows. No branch in this file decides what the tray should
//! say.
//!
//! # Plugin order
//!
//! Single-instance is registered **first**, as the plugin's own documentation
//! requires, so a second launch is intercepted before any other plugin can act
//! on it. Notification, autostart, and window-state follow; none of them
//! depends on the others.
//!
//! # Capability posture is unchanged
//!
//! Autostart and notifications are used from Rust only, through
//! `ManagerExt`/`NotificationExt`, which read managed Rust state rather than
//! going through plugin IPC commands. `capabilities/default.json` therefore
//! still grants exactly `core:default`, and the webview still cannot reach a
//! shell, the filesystem, or the network.
//!
//! # Windows never die, they hide
//!
//! Wry exits when its last window is *destroyed*. A tray app must not, so an
//! ordinary close is prevented and the window hidden — and
//! `RunEvent::ExitRequested { code: None }` is prevented too, because that is
//! the event a destroyed last window produces. Quit calls `AppHandle::exit(0)`,
//! which carries a code and is deliberately not prevented.

// Same reason as `controller_host`: `#[tauri::command]` fixes its own
// signatures. `State` arrives by value, `AppHandle` is injected by value, and
// taking references instead does not compile.
#![allow(clippy::needless_pass_by_value)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rocm_app_core::controller::Freshness;
use rocm_app_core::controller::adapters::{Notifier, Storage};
use rocm_app_core::health::HealthOverview;
use rocm_app_core::platform::HostPlatform;
use rocm_app_core::tray::notify::{self, NotifyState};
use rocm_app_core::tray::schedule::{Due, Scheduler};
use rocm_app_core::tray::{
    AUTOSTART_KEY, AutostartAction, AutostartState, FullSurface, QuickStatus, TrayInput,
    TrayStatus, TrayView, autostart_desired, icon, left_click_opens_quick_status, menu_id,
    quick_status, reconcile_autostart, start_hidden, tray_view,
};
use tauri::menu::{IsMenuItem, MenuBuilder, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIcon;
use tauri::{AppHandle, Emitter as _, Manager as _, Wry};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_notification::NotificationExt as _;

use crate::controller_host::{ControllerState, now_unix_ms};

/// Stable tray identifier, so the icon can be looked up later.
pub const TRAY_ID: &str = "status";
/// The full window, declared in `tauri.conf.json`.
pub const MAIN_WINDOW: &str = "main";
/// The compact quick-status window, also declared in `tauri.conf.json`.
///
/// Declared rather than built at runtime on purpose: Tauri documents
/// `WebviewWindowBuilder::new` as deadlocking on Windows when called from a
/// synchronous command or an event handler, which is exactly where a tray
/// click lands. A window that already exists only has to be shown.
pub const QUICK_WINDOW: &str = "quick";

/// How often the monitor wakes to ask the scheduler what is due.
///
/// Half a second. The scheduler owns the real cadences; this only bounds how
/// late a due probe can be, and how quickly the tray reacts to a mutation
/// starting or ending.
const TICK: std::time::Duration = std::time::Duration::from_millis(500);

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// The most recent thing the monitor learned.
#[derive(Debug, Default, Clone)]
struct Observation {
    overview: Option<HealthOverview>,
    /// Set when the probe itself failed. The previous overview is kept
    /// alongside it so the compact window can still show *something*, but
    /// [`TrayInput::status`] makes the failure win.
    error: Option<String>,
}

/// Live tray state, managed by Tauri and shared with the monitor thread.
pub struct TrayHost {
    platform: HostPlatform,
    tray: TrayIcon<Wry>,
    /// The info label items whose text changes with each observation. On
    /// Linux a tray menu cannot be replaced once set — only its items mutated
    /// — so the handles are retained rather than the menu rebuilt.
    info_items: Vec<(String, MenuItem<Wry>)>,
    latest: Mutex<Observation>,
    scheduler: Mutex<Scheduler>,
    autostart: AtomicBool,
}

impl TrayHost {
    /// Run something with the current tray inputs.
    ///
    /// The closure form exists because [`TrayInput`] borrows the overview out
    /// of the mutex; handing out a clone of a whole [`HealthOverview`] per
    /// command would be the alternative.
    fn with_input<T>(&self, f: impl FnOnce(&TrayInput<'_>) -> T) -> T {
        let latest = self.latest.lock().expect("poisoned");
        f(&TrayInput {
            overview: latest.overview.as_ref(),
            error: latest.error.as_deref(),
            platform: self.platform,
        })
    }

    /// Push the current model onto the native icon and menu.
    fn render(&self) {
        let view = self.with_input(tray_view);
        let image = icon(view.status);
        // Best-effort throughout: a tray daemon that has gone away must not
        // take the monitoring thread with it.
        let _ = self.tray.set_icon(Some(tauri::image::Image::new_owned(
            image.rgba,
            image.width,
            image.height,
        )));
        let _ = self.tray.set_tooltip(Some(&view.tooltip));
        for (id, item) in &self.info_items {
            if let Some(entry) = view.items.iter().find(|e| e.id == *id) {
                let _ = item.set_text(&entry.text);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

/// Create the tray, reconcile autostart, show the window, start monitoring.
///
/// Called from `setup`, and deliberately in this order: the tray exists before
/// any probe runs, so a boot launch produces a visible icon while the first
/// snapshot is still being taken rather than after it.
pub fn start(app: &AppHandle) -> tauri::Result<()> {
    let platform = HostPlatform::detect();
    let hidden = start_hidden(std::env::args());

    // The initial model, before anything has been probed.
    let checking = TrayInput {
        overview: None,
        error: None,
        platform,
    };
    let view = tray_view(&checking);

    let mut info_items = Vec::new();
    let mut items: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::with_capacity(view.items.len());
    for entry in &view.items {
        use rocm_app_core::tray::MenuKind;
        let item: Box<dyn IsMenuItem<Wry>> = match entry.kind {
            MenuKind::Separator => Box::new(PredefinedMenuItem::separator(app)?),
            // `Check` is wire compatibility only; the model no longer emits it.
            MenuKind::Label | MenuKind::Action | MenuKind::Check { .. } => {
                let built = MenuItem::with_id(
                    app,
                    entry.id.clone(),
                    &entry.text,
                    entry.enabled,
                    None::<&str>,
                )?;
                if matches!(entry.kind, MenuKind::Label) {
                    info_items.push((entry.id.clone(), built.clone()));
                }
                Box::new(built)
            }
        };
        items.push(item);
    }
    let refs: Vec<&dyn IsMenuItem<Wry>> = items.iter().map(AsRef::as_ref).collect();
    let menu = MenuBuilder::new(app).items(&refs).build()?;

    let image = icon(view.status);
    let mut builder = tauri::tray::TrayIconBuilder::with_id(TRAY_ID)
        .icon(tauri::image::Image::new_owned(
            image.rgba,
            image.width,
            image.height,
        ))
        .tooltip(&view.tooltip)
        .menu(&menu)
        // Left click must not pop the menu: on Windows it opens the compact
        // window instead, and on Linux the setting is documented as having no
        // effect at all.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu_event(app, event.id().as_ref()));

    if left_click_opens_quick_status(platform) {
        builder = builder.on_tray_icon_event(|tray, event| {
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle(), QUICK_WINDOW);
            }
        });
    }
    let tray = builder.build(app)?;

    let autostart = reconcile_autostart_on_launch(app, platform);
    app.manage(TrayHost {
        platform,
        tray,
        info_items,
        latest: Mutex::new(Observation::default()),
        scheduler: Mutex::new(Scheduler::default()),
        autostart: AtomicBool::new(autostart),
    });

    // A boot launch stays out of the way; anything else shows the window it
    // was started for.
    if !hidden {
        show_window(app, MAIN_WINDOW);
    }

    spawn_monitor(app);
    Ok(())
}

/// Bring intent and the operating system's login items back into agreement.
///
/// Runs on every launch, not only the first: a user who removed the login item
/// by hand, or an OS upgrade that dropped it, must not leave Settings claiming
/// something untrue.
fn reconcile_autostart_on_launch(app: &AppHandle, platform: HostPlatform) -> bool {
    if !platform.install_allowed() {
        return false;
    }
    let desired = autostart_desired(autostart_preference(app), installed_build());
    let manager = app.autolaunch();
    let os_enabled = manager.is_enabled().unwrap_or(false);
    match reconcile_autostart(desired, os_enabled) {
        AutostartAction::Enable => {
            ensure_autostart_dir();
            let _ = manager.enable();
        }
        AutostartAction::Disable => {
            let _ = manager.disable();
        }
        AutostartAction::Nothing => {}
    }
    // Report what the OS says afterwards, not what was asked for: a failed
    // enable must not show as enabled.
    manager.is_enabled().unwrap_or(desired)
}

/// Whether this binary is an installed release rather than a development
/// build.
///
/// A debug build lives in `target/` and is about to be replaced or deleted;
/// registering a login item for it is a bug. Release builds are what an
/// installer ships, so they get the documented default.
#[must_use]
pub const fn installed_build() -> bool {
    !cfg!(debug_assertions)
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

fn show_window(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn hide_window(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.hide();
    }
}

/// Hide on an ordinary close instead of destroying the window.
///
/// Registered for every window. Monitoring is the product; closing the window
/// is closing a view of it.
pub fn on_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let _ = window.hide();
    }
}

/// A second launch focuses the first instance instead of starting a process.
///
/// A boot launch that arrives while a window is already open must not *steal*
/// focus, so the forwarded argv is honoured: `--hidden` means the user did not
/// ask for a window.
pub fn focus_existing_instance(app: &AppHandle, argv: &[String]) {
    if start_hidden(argv.iter().map(String::as_str)) {
        return;
    }
    show_window(app, MAIN_WINDOW);
}

// ---------------------------------------------------------------------------
// Menu routing
// ---------------------------------------------------------------------------

fn on_menu_event(app: &AppHandle, id: &str) {
    match id {
        menu_id::OPEN_APP => {
            hide_window(app, QUICK_WINDOW);
            show_window(app, MAIN_WINDOW);
        }
        menu_id::MORE_INFO => open_more_info(),
        // Not `PredefinedMenuItem::quit`, which Tauri documents as unsupported
        // on Linux. `exit(0)` carries a code, so the run loop's
        // `prevent_exit` for code-less exit requests does not block it.
        menu_id::QUIT => app.exit(0),
        _ => {}
    }
}

/// Open the project's repository in the default browser. Best-effort: a
/// missing opener must not take the tray with it.
fn open_more_info() {
    const URL: &str = "https://github.com/mikeroySoft/rocm-app";
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(URL).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", URL])
        .spawn();
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    let _ = URL;
}

// ---------------------------------------------------------------------------
// The monitor
// ---------------------------------------------------------------------------

fn spawn_monitor(app: &AppHandle) {
    let worker = app.clone();
    let spawned = std::thread::Builder::new()
        .name("rocm-tray-monitor".to_owned())
        .spawn(move || {
            let mut notify_state = load_notify_state(&worker);
            let mut was_mutating = false;
            loop {
                std::thread::sleep(TICK);
                tick(&worker, &mut notify_state, &mut was_mutating);
            }
        });
    // A tray with no monitor is a tray stuck on "Checking". Say so rather than
    // silently presenting a frozen icon.
    if spawned.is_err()
        && let Some(host) = app.try_state::<TrayHost>()
    {
        host.latest.lock().expect("poisoned").error =
            Some("ROCm App could not start background monitoring.".to_owned());
        host.render();
    }
}

fn tick(app: &AppHandle, notify_state: &mut NotifyState, was_mutating: &mut bool) {
    let Some(host) = app.try_state::<TrayHost>() else {
        return;
    };
    let Some(controller) = app.try_state::<ControllerState>() else {
        return;
    };

    let mutating = controller.controller.is_mutating();
    // The terminal event, observed from outside: the controller's mutation
    // guard is released as the operation returns, after its `Completed`,
    // `Failed`, or `Cancelled` event has been emitted.
    if *was_mutating && !mutating {
        host.scheduler
            .lock()
            .expect("poisoned")
            .request_full_probe();
    }
    *was_mutating = mutating;

    let due = host
        .scheduler
        .lock()
        .expect("poisoned")
        .due(now_unix_ms(), mutating);
    if !due.any() {
        return;
    }

    if due.metrics {
        // Sampling is the point: it keeps the bounded history populated so the
        // Overview's trend has something in it the moment a window opens,
        // instead of starting empty every time.
        let _ = controller.telemetry.read(now_unix_ms());
    }
    if due.health {
        probe(app, &controller, &host, due, notify_state);
    }

    host.scheduler
        .lock()
        .expect("poisoned")
        .finished(due, now_unix_ms());
}

fn probe(
    app: &AppHandle,
    controller: &ControllerState,
    host: &TrayHost,
    due: Due,
    notify_state: &mut NotifyState,
) {
    let now = now_unix_ms();
    let (status, announceable_update) = match controller.controller.snapshot(Freshness::Full) {
        Ok(view) => {
            let telemetry = controller.telemetry.read(now);
            let overview = rocm_app_core::health::overview(
                &view.snapshot,
                &telemetry,
                now,
                Some(env!("CARGO_PKG_VERSION")),
            );
            let status = TrayStatus::from_verdict(overview.verdict);
            // Update availability is only re-evaluated on an update tick, so a
            // one-minute health probe cannot re-announce an offer.
            let update = due
                .update
                .then(|| notify::available_update(&view.snapshot))
                .flatten();
            {
                let mut latest = host.latest.lock().expect("poisoned");
                latest.overview = Some(overview);
                latest.error = None;
            }
            (status, update)
        }
        Err(error) => {
            let message = error.user_message();
            host.latest.lock().expect("poisoned").error = Some(message);
            (TrayStatus::Error, None)
        }
    };

    host.render();

    let before = notify_state.clone();
    if let Some(announcement) = notify::on_observation(notify_state, status, announceable_update) {
        let _ = app
            .notification()
            .builder()
            .title(announcement.title)
            .body(announcement.body)
            .show();
    }
    if *notify_state != before {
        save_notify_state(app, notify_state);
    }
}

// ---------------------------------------------------------------------------
// Persisted settings
// ---------------------------------------------------------------------------

fn storage(app: &AppHandle) -> Option<Arc<dyn Storage>> {
    app.try_state::<ControllerState>()
        .map(|state| state.storage.clone())
}

/// The user's saved autostart choice, or `None` if they have never chosen.
///
/// A missing or unreadable file is "no choice", never `false`: treating an
/// unreadable preference as an explicit opt-out would silently turn monitoring
/// off on a machine whose data directory hiccuped once.
fn autostart_preference(app: &AppHandle) -> Option<bool> {
    let bytes = storage(app)?.read(AUTOSTART_KEY).ok()??;
    serde_json::from_slice(&bytes).ok()
}

fn save_autostart_preference(app: &AppHandle, enabled: bool) {
    if let Some(storage) = storage(app) {
        let bytes = serde_json::to_vec(&enabled).unwrap_or_else(|_| b"true".to_vec());
        let _ = storage.write_atomic(AUTOSTART_KEY, &bytes);
    }
}

fn load_notify_state(app: &AppHandle) -> NotifyState {
    storage(app)
        .and_then(|storage| storage.read(notify::KEY).ok().flatten())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_notify_state(app: &AppHandle, state: &NotifyState) {
    if let Some(storage) = storage(app)
        && let Ok(bytes) = serde_json::to_vec(state)
    {
        let _ = storage.write_atomic(notify::KEY, &bytes);
    }
}

/// Persist a choice, apply it, and report what the OS then says.
fn apply_autostart(app: &AppHandle, host: &TrayHost, enabled: bool) -> AutostartState {
    if !host.platform.install_allowed() {
        return unavailable_autostart(host.platform);
    }
    save_autostart_preference(app, enabled);
    let manager = app.autolaunch();
    let outcome = if enabled {
        ensure_autostart_dir();
        manager.enable()
    } else {
        manager.disable()
    };
    let actual = manager.is_enabled().unwrap_or(enabled);
    host.autostart.store(actual, Ordering::Relaxed);
    host.render();
    AutostartState {
        enabled: actual,
        available: true,
        detail: match outcome {
            Err(error) if actual != enabled => {
                format!("This computer would not change the setting: {error}")
            }
            _ => autostart_detail(actual),
        },
    }
}

/// Where the Linux autostart entry goes, resolved the way the backend does.
///
/// `auto-launch` 0.5 hardcodes `dirs::home_dir()/.config/autostart` — it does
/// **not** consult `XDG_CONFIG_HOME` — and creates the leaf with a bare
/// `fs::create_dir`, which fails with ENOENT whenever `~/.config` itself is
/// absent. A genuinely fresh account lacks `~/.config`; desktops create it
/// lazily. Verified against the vendored crate source, not its docs.
#[cfg(target_os = "linux")]
fn autostart_dir(home: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    home.filter(|value| !value.is_empty())
        .map(|home| std::path::Path::new(home).join(".config").join("autostart"))
}

/// The first "Start at login" toggle on a first-day machine failed with
/// "No such file or directory" because of the backend behaviour above.
/// Creating the directory is idempotent; a real failure still surfaces
/// through `enable()`, which the caller reports honestly.
#[cfg(target_os = "linux")]
fn ensure_autostart_dir() {
    if let Some(dir) = autostart_dir(std::env::var_os("HOME").as_deref()) {
        let _ = std::fs::create_dir_all(dir);
    }
}

/// Windows registers a Run key and macOS is out of scope; nothing to create.
#[cfg(not(target_os = "linux"))]
const fn ensure_autostart_dir() {}

fn autostart_detail(enabled: bool) -> String {
    if enabled {
        "ROCm App starts with this computer and watches ROCm in the background.".to_owned()
    } else {
        "ROCm App only runs when you open it.".to_owned()
    }
}

fn unavailable_autostart(platform: HostPlatform) -> AutostartState {
    AutostartState {
        enabled: false,
        available: false,
        detail: platform
            .unsupported_reason()
            .unwrap_or("This computer cannot run ROCm App in the background.")
            .to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// The compact window's content.
#[tauri::command]
pub fn tray_quick_status(host: tauri::State<'_, TrayHost>) -> QuickStatus {
    host.with_input(quick_status)
}

/// The tray model, for tests and diagnostics. Reading it changes nothing.
#[tauri::command]
pub fn tray_model(host: tauri::State<'_, TrayHost>) -> TrayView {
    host.with_input(tray_view)
}

/// What Settings shows about starting at login.
#[tauri::command]
pub fn tray_autostart_state(app: AppHandle, host: tauri::State<'_, TrayHost>) -> AutostartState {
    if !host.platform.install_allowed() {
        return unavailable_autostart(host.platform);
    }
    let enabled = app.autolaunch().is_enabled().unwrap_or(false);
    host.autostart.store(enabled, Ordering::Relaxed);
    AutostartState {
        enabled,
        available: true,
        detail: autostart_detail(enabled),
    }
}

/// Turn starting at login on or off.
#[tauri::command]
pub fn tray_set_autostart(
    app: AppHandle,
    host: tauri::State<'_, TrayHost>,
    enabled: bool,
) -> AutostartState {
    apply_autostart(&app, &host, enabled)
}

/// Hand off from the compact window to a full window.
///
/// The compact window closes itself here rather than in the webview, so no
/// window-manipulation permission has to be granted to the frontend.
#[tauri::command]
pub fn tray_open_full(app: AppHandle, surface: Option<FullSurface>) {
    hide_window(&app, QUICK_WINDOW);
    show_window(&app, MAIN_WINDOW);
    if let Some(surface) = surface
        && let Some(window) = app.get_webview_window(MAIN_WINDOW)
    {
        // The shell listens for this and switches surface; a failed emit just
        // leaves the user on the Overview, which is never wrong.
        let _ = window.emit("rocm://open-surface", surface);
    }
}

/// Dismiss the compact window without opening anything.
///
/// Esc in the panel calls this. The window is undecorated and always on top,
/// so it has no close button and no title bar; without a keyboard dismissal
/// the only ways out are pointer paths — the tray toggle or the hand-off
/// button. Same rule as `tray_open_full`: the hide happens here so the
/// frontend needs no window-manipulation permission.
#[tauri::command]
pub fn tray_hide_quick(app: AppHandle) {
    hide_window(&app, QUICK_WINDOW);
}

// ---------------------------------------------------------------------------
// Desktop notifications
// ---------------------------------------------------------------------------

/// Shows an operation's outcome on the desktop, and records it.
///
/// Wraps rather than replaces the log notifier: the desktop notification is
/// transient and the Phase 9 log view needs a durable record of the same
/// event. Both are best-effort — a notification daemon that is not running
/// must not fail the install it was reporting on.
pub struct DesktopNotifier {
    app: AppHandle,
    log: Arc<dyn Notifier>,
}

impl DesktopNotifier {
    #[must_use]
    pub const fn new(app: AppHandle, log: Arc<dyn Notifier>) -> Self {
        Self { app, log }
    }
}

impl Notifier for DesktopNotifier {
    fn notify(&self, title: &str, body: &str) {
        let _ = self
            .app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show();
        self.log.notify(title, body);
    }
}

#[cfg(test)]
mod tests {
    use rocm_app_core::controller::adapters::FakeStorage;

    use super::*;

    /// A debug build must never register a login item; a release build is what
    /// an installer ships and gets the documented default.
    #[test]
    fn tray_host_installed_build_tracks_the_build_profile() {
        assert_eq!(installed_build(), !cfg!(debug_assertions));
        // The two halves of the rule, spelled out against the core policy.
        assert!(autostart_desired(None, true));
        assert!(!autostart_desired(None, false));
    }

    #[test]
    fn tray_host_autostart_preference_round_trips_through_storage() {
        let storage = FakeStorage::new();
        for chosen in [true, false] {
            let bytes = serde_json::to_vec(&chosen).expect("serialize");
            storage
                .write_atomic(AUTOSTART_KEY, &bytes)
                .expect("write preference");
            let read: bool = serde_json::from_slice(
                &storage.read(AUTOSTART_KEY).expect("read").expect("present"),
            )
            .expect("deserialize");
            assert_eq!(read, chosen);
        }
    }

    /// Regression: the first "Start at login" toggle on a fresh account
    /// failed with ENOENT. `auto-launch` 0.5 hardcodes
    /// `home_dir()/.config/autostart` (no `XDG_CONFIG_HOME`), and its bare
    /// `create_dir` cannot make the missing `~/.config` parent. The resolver
    /// must mirror exactly that lookup, and invent nothing without a home.
    #[cfg(target_os = "linux")]
    #[test]
    fn tray_host_autostart_dir_follows_the_backend_lookup() {
        use std::ffi::OsStr;
        use std::path::PathBuf;

        assert_eq!(
            autostart_dir(Some(OsStr::new("/home/u"))),
            Some(PathBuf::from("/home/u/.config/autostart")),
        );
        // An empty variable is unset in spirit, not a root to write under.
        assert_eq!(autostart_dir(Some(OsStr::new(""))), None);
        assert_eq!(autostart_dir(None), None);
    }

    /// The dangerous default. An unreadable preference must read as "no choice
    /// yet", not as an explicit opt-out that silently disables monitoring.
    #[test]
    fn tray_host_unreadable_autostart_preference_is_not_an_opt_out() {
        let storage = FakeStorage::new();
        assert!(storage.read(AUTOSTART_KEY).expect("read").is_none());
        assert_eq!(
            serde_json::from_slice::<bool>(b"not json").ok(),
            None,
            "corrupt bytes must not deserialize to false"
        );
        // And the core policy turns that into the installed-build default.
        assert!(autostart_desired(None, true));
    }

    #[test]
    fn tray_host_notify_state_round_trips_through_storage() {
        let storage = FakeStorage::new();
        let mut state = NotifyState::default();
        notify::on_observation(&mut state, TrayStatus::Healthy, None);
        storage
            .write_atomic(notify::KEY, &serde_json::to_vec(&state).expect("serialize"))
            .expect("write");

        let restored: NotifyState =
            serde_json::from_slice(&storage.read(notify::KEY).expect("read").expect("present"))
                .expect("deserialize");
        assert_eq!(restored, state);
    }

    /// A truncated or hand-edited state file must seed fresh, not panic. The
    /// monitor thread has no supervisor to restart it.
    #[test]
    fn tray_host_corrupt_notify_state_seeds_fresh() {
        let recovered = serde_json::from_slice::<NotifyState>(b"{\"status\":")
            .ok()
            .unwrap_or_default();
        assert_eq!(recovered, NotifyState::default());
        // A partially-valid file keeps what it can, because the fields default.
        let partial: NotifyState =
            serde_json::from_slice(br#"{"updateVersion":"7.15.0"}"#).expect("defaults apply");
        assert_eq!(partial.status, None);
        assert_eq!(partial.update_version.as_deref(), Some("7.15.0"));
    }

    #[test]
    fn tray_host_autostart_is_unavailable_on_an_unsupported_host() {
        for platform in [HostPlatform::Wsl, HostPlatform::Unsupported] {
            let state = unavailable_autostart(platform);
            assert!(!state.available);
            assert!(!state.enabled);
            assert!(!state.detail.is_empty(), "{platform:?} gave no reason");
        }
    }

    #[test]
    fn tray_host_autostart_detail_states_both_outcomes_plainly() {
        assert_ne!(autostart_detail(true), autostart_detail(false));
        assert!(autostart_detail(true).contains("starts with this computer"));
        assert!(autostart_detail(false).contains("only runs when you open it"));
    }
}
